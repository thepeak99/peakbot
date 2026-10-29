#!/bin/sh
#
# test-release.sh — host-level TDD tests for the CI release-pipeline rework.
#
# Written BEFORE the implementation: expect RED until the developer lands
#   - the Makefile contract: RELEASE_VERSION, release-check, explicit
#     sequential release recipe, 10-asset draft-first release-publish with
#     DELETE-on-failure and a "prerelease" flag derived from '-' in TAG,
#     no-prompt release-bump with notes promotion, dumb release-tag (no PR
#     automation), and the removal of .release-version / NOTES / ALLOW_DIRTY
#   - the auto-updater pipeline: shared RELEASE_ASSETS (10 = 8 legacy +
#     macos-universal.app.tar.gz + latest.json), release order
#     desktop-updater → release-publish → release-publish-github
#     (curl+jq, idempotent, polls the GitHub tag ref, draft + trap-delete,
#     10 uploads to uploads.github.com, PATCH make_latest true/false),
#     release-check asserting the tauri.conf.json updater endpoint contains
#     GH_MIRROR_REPO
#   - .gitea/workflows/release.yml (tag-push → make release TAG=...), with
#     the GH_RELEASE_TOKEN + TAURI_SIGNING_PRIVATE_KEY(_PASSWORD) secrets
#     mapped to env, and desktop.yml + desktop/scripts/attach-release.sh
#     deleted
#
# Runs NO docker builds, NO network: make release-publish is exercised with
# a fake `curl` on PATH (logs every invocation, returns canned JSON), and
# release-bump/release-tag run in throwaway local clones against a local
# bare `origin` (cargo stubbed to a no-op). release-publish-github is
# exercised the same way — the fake also speaks the GitHub API shapes
# (tag-ref polling with a counter, releases/tags lookup per GH_REL_MODE,
# uploads.github.com, -o/-w %{http_code} status reporting).
#
# Run:  sh tests/release.sh               (from the repo root, or anywhere)
# Exit: 0 when every check passes (GREEN); 1 with a failing-check list
#       otherwise (RED — each failure names the missing implementation piece).

set -u

script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/.." >/dev/null 2>&1 && pwd)
cd "$root"

MK=Makefile
GI=.gitignore
RY=.gitea/workflows/release.yml
DY=.gitea/workflows/desktop.yml
AR=desktop/scripts/attach-release.sh

# Version under test = root Cargo.toml [package] version (same awk as today).
V=$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' Cargo.toml)
[ -n "$V" ] || { echo "❌ could not read version from Cargo.toml" >&2; exit 1; }

# Fake Gitea coordinates (explicit vars must override origin-derived defaults).
GU="https://git.fake.example"
GO="fakeowner"
GR="fakerepo"
API="$GU/api/v1/repos/$GO/$GR"

PASS=0
FAIL=0
FAILED=""

ok()  { PASS=$((PASS+1)); echo "✅ $1"; }
bad() { FAIL=$((FAIL+1)); FAILED="${FAILED}  - $1\n"; echo "❌ $1"; }

# grep_ok <name> <fixed-string> <file>
grep_ok() {
    if grep -qF -- "$2" "$3" 2>/dev/null; then ok "$1"; else bad "$1 (missing: $2 in $3)"; fi
}

# grep_bad <name> <fixed-string> <file> — passes when the string is ABSENT.
grep_bad() {
    if grep -qF -- "$2" "$3" 2>/dev/null; then bad "$1 (forbidden: $2 in $3)"; else ok "$1"; fi
}

workdirs=""
cleanup() { [ -n "$workdirs" ] && rm -rf $workdirs; }
trap cleanup EXIT

# ── Shared fake-curl infrastructure (T08, T26–T28, T45–T51, T58) ───────────
# ONE fake `curl` on PATH serves BOTH the Gitea and the GitHub fakes. It
# logs "METHOD URL [BODY=…] [FILE=…] [DATA=…] [ | -H …]" per invocation to
# $CURL_LOG and answers by URL shape:
#   git.fake.example   — legacy Gitea canned answers (release id 42).
#   api.github.com     — GET git/ref/tags/<v>: 404 twice, then 200 (poll
#                        counter in $CURL_LOG.refpoll); GH_REF_ALWAYS_404=1
#                        keeps it at 404. GET releases/tags/<v>: 404, or
#                        200 per GH_REL_MODE=done|draft (done builds
#                        assets from the JSON-object lines in
#                        $CURL_LOG.doneassets — {"name":…,"size":…} per
#                        line, the shape real GitHub returns in .assets[]).
#                        POST releases → 201 {"id":77,"draft":true};
#                        DELETE releases/<id> → 204; PATCH → 200.
#   uploads.github.com — POST …/releases/<id>/assets?name=<f> → 201, or
#                        422 when FAIL_UPLOAD=<f>.
# GitHub answers carry their HTTP status via -w %{http_code} (body on
# stdout or via -o), so the implementation may check status and/or body.
# Makefile contract under test: GH_MIRROR_REPO ?= thepeak99/peakbot,
# GH_API ?= https://api.github.com, GH_UPLOADS ?= https://uploads.github.com,
# poll GET <GH_API>/repos/<repo>/git/ref/tags/<v> GH_TAG_WAIT_TRIES (60) ×
# GH_TAG_WAIT_SECS (10s), auth header "Authorization: Bearer <token>",
# token never printed. GitHub asset uploads must be RAW bodies
# (--data-binary @<file>, logged as DATA=…); Gitea asset uploads stay
# multipart (-F attachment=@<file>, logged as FILE=…).

D=$(mktemp -d); workdirs="$workdirs $D"
mkdir -p "$D/bin" "$D/out/desktop"

cat > "$D/bin/curl" <<'FAKECURL'
#!/bin/sh
: "${CURL_LOG:?fake curl needs CURL_LOG}"
method=GET; url=''; body=''; fdata=''; data=''; ofile=''; wfmt=''; hdrs=''
prev=''
for a in "$@"; do
  case "$prev" in
    -X|--request)    method=$a; prev=''; continue ;;
    -d|--data)       body=$a; prev=''; continue ;;
    -F|--form)       fdata=$a; prev=''; continue ;;
    --data-binary)   data=$a; prev=''; continue ;;
    -o)              ofile=$a; prev=''; continue ;;
    -w)              wfmt=$a; prev=''; continue ;;
    -H|--header)     hdrs="$hdrs | $a"; prev=''; continue ;;
  esac
  case "$a" in
    -*) prev=$a ;;
    *) prev=''; if [ -z "$url" ]; then url=$a; fi ;;
  esac
done
line="$method $url"
[ -n "$body" ]  && line="$line BODY=$body"
[ -n "$fdata" ] && line="$line FILE=$fdata"
[ -n "$data" ]  && line="$line DATA=$data"
[ -n "$hdrs" ]  && line="$line$hdrs"
printf '%s\n' "$line" >> "$CURL_LOG"

case "$url" in
  *uploads.github.com*)
    name=${url##*name=}
    if [ -n "${FAIL_UPLOAD:-}" ] && [ "$name" = "$FAIL_UPLOAD" ]; then
      status=422; resp="{\"message\":\"upload failed for $name\"}"
    else
      status=201; resp="{\"id\":1,\"name\":\"$name\"}"
    fi
    if [ -n "$ofile" ]; then printf '%s\n' "$resp" > "$ofile"; else printf '%s\n' "$resp"; fi
    [ -n "$wfmt" ] && printf '%s\n' "$wfmt" | sed "s/%{http_code}/$status/g"
    exit 0 ;;
  *api.github.com*)
    status=200; resp='{}'
    case "$method" in
      GET)
        case "$url" in
          *git/ref/tags/*)
            cf="$CURL_LOG.refpoll"
            n=$(cat "$cf" 2>/dev/null || echo 0); n=$((n+1)); printf '%s\n' "$n" > "$cf"
            if [ "${GH_REF_ALWAYS_404:-0}" = "1" ] || [ "$n" -le 2 ]; then
              status=404; resp='{"message":"Not Found"}'
            else
              status=200; resp='{"ref":"refs/tags/fake","object":{"type":"tag","sha":"fake"}}'
            fi ;;
          *releases/tags/*)
            case "${GH_REL_MODE:-none}" in
              done)
                status=200
                resp=$(jq -nc '{id:77, draft:false, tag_name:"", assets: [inputs]}' < "$CURL_LOG.doneassets") ;;
              draft)
                status=200; resp='{"id":77,"draft":true,"tag_name":"","assets":[]}' ;;
              *)
                status=404; resp='{"message":"Not Found"}' ;;
            esac ;;
          *)
            status=404; resp='{"message":"Not Found"}' ;;
        esac ;;
      POST)
        status=201; resp='{"id":77,"draft":true}' ;;
      DELETE)
        status=204; resp='' ;;
      PATCH)
        status=200; resp='{"id":77,"draft":false}' ;;
    esac
    if [ -n "$ofile" ]; then printf '%s\n' "$resp" > "$ofile"; else printf '%s\n' "$resp"; fi
    [ -n "$wfmt" ] && printf '%s\n' "$wfmt" | sed "s/%{http_code}/$status/g"
    exit 0 ;;
  *)
    # Gitea (git.fake.example) — legacy canned answers.
    case "$method" in
      POST)
        case "$url" in
          *releases/*/assets*)
            name=${url##*name=}
            if [ "$name" = "${FAIL_UPLOAD:-}" ]; then
              printf '{"message":"upload failed for %s"}\n' "$name"
            else
              printf '{"name":"%s","browser_download_url":"https://fake.example/%s"}\n' "$name" "$name"
            fi ;;
          */releases)
            if [ -n "${CREATE_FAIL:-}" ]; then
              printf '{"message":"create failed"}\n'
            else
              printf '{"id":42}\n'
            fi ;;
          *) printf '{}\n' ;;
        esac ;;
      PATCH)  printf '{"id":42,"draft":false}\n' ;;
      DELETE) printf '{"success":true}\n' ;;
      *)      printf '{}\n' ;;
    esac
    exit 0 ;;
esac
FAKECURL
chmod +x "$D/bin/curl"

# ASSET10 — the 10 release asset basenames (8 legacy + the macOS updater
# tarball + latest.json), in the spec order.
ASSET10="peakbot-$V-linux-amd64
peakbot-$V-windows-amd64.exe
peakbot-$V-macos-universal2
peakbot-$V-android-arm64
PeakBot-Desktop-$V-linux-x86_64.AppImage
PeakBot-Desktop-$V-macos-universal.zip
PeakBot-Desktop-$V-macos-universal.app.tar.gz
PeakBot-Desktop-$V-windows-x86_64-portable.exe
PeakBot-Desktop-$V-windows-x86_64-setup.exe
latest.json"

make_assets() { # $1 = output dir, $2 = version (default: $V): create all 10 release assets
    ma_ver=${2:-$V}
    mkdir -p "$1/desktop"
    for n in peakbot-$ma_ver-linux-amd64 peakbot-$ma_ver-windows-amd64.exe \
             peakbot-$ma_ver-macos-universal2 peakbot-$ma_ver-android-arm64; do
        printf x > "$1/$n"
    done
    for n in PeakBot-Desktop-$ma_ver-linux-x86_64.AppImage \
             PeakBot-Desktop-$ma_ver-macos-universal.zip \
             PeakBot-Desktop-$ma_ver-macos-universal.app.tar.gz \
             PeakBot-Desktop-$ma_ver-windows-x86_64-portable.exe \
             PeakBot-Desktop-$ma_ver-windows-x86_64-setup.exe latest.json; do
        printf x > "$1/desktop/$n"
    done
}

# ST — controlled STABLE tag for the stable-side fixture runs (T08's
# stable half, T27, T46). The worktree version is deliberately irrelevant:
# on a release branch $V is itself a prerelease, so the stable-side
# contract (prerelease:false, make_latest:"true") must be exercised with a
# tag that has no '-'.
ST=0.8.0
ASSET10_ST="peakbot-$ST-linux-amd64
peakbot-$ST-windows-amd64.exe
peakbot-$ST-macos-universal2
peakbot-$ST-android-arm64
PeakBot-Desktop-$ST-linux-x86_64.AppImage
PeakBot-Desktop-$ST-macos-universal.zip
PeakBot-Desktop-$ST-macos-universal.app.tar.gz
PeakBot-Desktop-$ST-windows-x86_64-portable.exe
PeakBot-Desktop-$ST-windows-x86_64-setup.exe
latest.json"

# stable_fixture <clone-dir> — throwaway clone of $root with the $ST notes
# file + $ST asset set (stable twin of the $D/pre 0.9.0-rc.1 fixture used
# by T08/T47).
stable_fixture() {
    git clone -q --depth 1 "$root" "$1" 2>/dev/null || return 1
    printf 'stable sentinel notes\n' > "$1/release-notes/$ST.md"
    make_assets "$D/out" "$ST"
}

# run_publish <logfile> [FAIL_UPLOAD] [TAG] [CWD] → sets P_RC / P_OUT
run_publish() {
    rp_log=$1; rp_fail=${2:-}; rp_tag=${3:-$V}; rp_cwd=${4:-$root}
    rm -f "$rp_log"; : > "$rp_log"
    P_OUT=$( cd "$rp_cwd" && \
      FAIL_UPLOAD="$rp_fail" CURL_LOG="$rp_log" PATH="$D/bin:$PATH" \
      make release-publish TAG="$rp_tag" OUTPUT_DIR="$D/out" \
      GITEA_URL="$GU" OWNER="$GO" REPO="$GR" GITEA_TOKEN=fake-token 2>&1 )
    P_RC=$?
}

# run_gh <cwd> <logfile> <tag> [TOKEN] → sets G_RC / G_OUT.
# The fake's knobs (GH_FAIL_UPLOAD, GH_REF_ALWAYS_404, GH_REL_MODE,
# GH_TAG_WAIT_TRIES) are read from the caller's environment.
# TOKEN uses ${4-…} (not ${4:-…}) so an EXPLICIT empty token stays empty
# (T45 must really pass an empty GH_RELEASE_TOKEN).
run_gh() {
    gh_cwd=$1; gh_log=$2; gh_tag=$3; gh_tok=${4-ghsecret-token-123}
    # NOTE: $gh_log.doneassets is seeded by the test (T49) and only ever
    # READ by the fake — do not rm it here, or the seed is wiped.
    rm -f "$gh_log" "$gh_log.refpoll"; : > "$gh_log"
    G_OUT=$( cd "$gh_cwd" && \
      CURL_LOG="$gh_log" FAIL_UPLOAD="${GH_FAIL_UPLOAD:-}" \
      GH_REF_ALWAYS_404="${GH_REF_ALWAYS_404:-0}" \
      GH_REL_MODE="${GH_REL_MODE:-none}" PATH="$D/bin:$PATH" \
      make release-publish-github TAG="$gh_tag" OUTPUT_DIR="$D/out" \
      GH_RELEASE_TOKEN="$gh_tok" GH_TAG_WAIT_SECS=0 \
      ${GH_TAG_WAIT_TRIES:+GH_TAG_WAIT_TRIES=$GH_TAG_WAIT_TRIES} 2>&1 )
    G_RC=$?
}

# ── A: Makefile static contract ────────────────────────────────────────────

# T01: RELEASE_VERSION is defined from the root Cargo.toml [package] version
# (same awk extraction as today's release-bump).
if grep -qE '^RELEASE_VERSION[[:space:]]*[:?]?=' "$MK" 2>/dev/null \
    && grep -E '^RELEASE_VERSION[[:space:]]*[:?]?=' "$MK" | grep -qF 'Cargo.toml' \
    && grep -E '^RELEASE_VERSION[[:space:]]*[:?]?=' "$MK" | grep -qF 'awk'; then
    ok "T01 Makefile defines RELEASE_VERSION from root Cargo.toml via awk"
else
    bad "T01 Makefile defines RELEASE_VERSION from root Cargo.toml via awk"
fi

# T02/T03: the .release-version state file is gone everywhere (Makefile
# (including clean), .gitignore).
grep_bad "T02 Makefile has no .release-version" '.release-version' "$MK"
grep_bad "T03 .gitignore has no .release-version" '.release-version' "$GI"

# T04: no NOTES variable (definition or reference). Lowercase "notes" is fine.
t04=1
grep -qE '^[[:space:]]*NOTES[[:space:]]*[:?]?=' "$MK" 2>/dev/null && t04=0
grep -qF '${NOTES' "$MK" 2>/dev/null && t04=0
grep -qE '\$NOTES\b' "$MK" 2>/dev/null && t04=0
if [ "$t04" -eq 1 ]; then ok "T04 Makefile has no NOTES variable"; else bad "T04 Makefile has no NOTES variable"; fi

# T05: no ALLOW_DIRTY escape hatch.
grep_bad "T05 Makefile has no ALLOW_DIRTY" 'ALLOW_DIRTY' "$MK"

# T06: release-check target exists.
if grep -qE '^release-check:' "$MK" 2>/dev/null; then
    ok "T06 Makefile has a release-check target"
else
    bad "T06 Makefile has a release-check target"
fi

# T07: no "Release <v>" body/name fallback.
grep_bad "T07 Makefile has no 'Release <v>' fallback" 'Release $$v' "$MK"

# T08: release-publish sends "prerelease":true EXACTLY when the TAG contains
# '-', "prerelease":false otherwise. Behavioural, via the fake curl's
# capture of the release-create body. Both polarities run in throwaway
# clones with controlled tags (stable $ST / rc 0.9.0-rc.1) so their notes
# files never touch the worktree — and the worktree version is irrelevant
# (on a release branch $V is itself a prerelease).
t08=1; why=""
stable_fixture "$D/stb" || { t08=0; why="stable fixture clone failed"; }
[ -d "$D/stb" ] && run_publish "$D/log-t08-stable" "" "$ST" "$D/stb"
if [ -d "$D/stb" ]; then
    grep -qF '"prerelease":false' "$D/log-t08-stable" 2>/dev/null \
        || { t08=0; why="stable create body lacks \"prerelease\":false"; }
    grep -qF '"prerelease":true' "$D/log-t08-stable" 2>/dev/null \
        && { t08=0; why="${why}${why:+; }stable create body has \"prerelease\":true"; }
fi
git clone -q --depth 1 "$root" "$D/pre" 2>/dev/null || { t08=0; why="${why}${why:+; }prerelease clone failed"; }
[ -d "$D/pre" ] && printf 'prerelease notes\n' > "$D/pre/release-notes/0.9.0-rc.1.md"
# The rc run looks for TAG-named assets (release-publish derives names from
# TAG) — create the 0.9.0-rc.1 set alongside the $V set.
[ -d "$D/pre" ] && make_assets "$D/out" 0.9.0-rc.1
[ -d "$D/pre" ] && run_publish "$D/log-t08-pre" "" 0.9.0-rc.1 "$D/pre"
if [ -d "$D/pre" ]; then
    grep -qF '"prerelease":true' "$D/log-t08-pre" 2>/dev/null \
        || { t08=0; why="${why}${why:+; }prerelease create body lacks \"prerelease\":true"; }
    grep -qF '"prerelease":false' "$D/log-t08-pre" 2>/dev/null \
        && { t08=0; why="${why}${why:+; }prerelease create body has \"prerelease\":false"; }
fi
if [ "$t08" -eq 1 ]; then
    ok "T08 release-publish: prerelease flag follows '-' in TAG (false stable / true rc)"
else
    bad "T08 release-publish: prerelease flag follows '-' in TAG (false stable / true rc) ($why)"
fi

# T09: release-tag exists and has NO PR create/merge automation.
t09=1
grep -qE '^release-tag:' "$MK" 2>/dev/null || t09=0
sed -n '/^release-tag:/,/^##/p' "$MK" 2>/dev/null | grep -qF '/pulls' && t09=0
if [ "$t09" -eq 1 ]; then
    ok "T09 Makefile release-tag: exists, no /pulls PR automation"
else
    bad "T09 Makefile release-tag: exists, no /pulls PR automation"
fi

# ── B: workflow contract (.gitea/workflows/release.yml) ────────────────────

if [ -f "$RY" ]; then ok "T10 .gitea/workflows/release.yml exists"; else bad "T10 .gitea/workflows/release.yml exists"; fi

# T11: triggered by tag pushes.
if grep -qE '^[[:space:]]*push:' "$RY" 2>/dev/null \
    && grep -qE '^[[:space:]]*tags:' "$RY" 2>/dev/null; then
    ok "T11 release.yml: on: push: tags"
else
    bad "T11 release.yml: on: push: tags"
fi

grep_ok "T12 release.yml: runs-on: dind" 'runs-on: dind' "$RY"
# T13: the release job gets a 300-minute budget and publishes 10 assets
# (the 8 legacy + PeakBot-Desktop-<v>-macos-universal.app.tar.gz + latest.json).
if grep -qF 'timeout-minutes: 300' "$RY" 2>/dev/null \
    && grep -qF '(10 assets)' "$RY" 2>/dev/null; then
    ok "T13 release.yml: timeout-minutes: 300 + '(10 assets)'"
else
    bad "T13 release.yml: timeout-minutes: 300 + '(10 assets)'"
fi

grep_bad "T14 release.yml: no container: key" 'container:' "$RY"

# T15: secret RELEASE_TOKEN mapped to env GITEA_TOKEN.
if grep -qF 'RELEASE_TOKEN' "$RY" 2>/dev/null && grep -qF 'GITEA_TOKEN' "$RY" 2>/dev/null; then
    ok "T15 release.yml: RELEASE_TOKEN secret → GITEA_TOKEN env"
else
    bad "T15 release.yml: RELEASE_TOKEN secret → GITEA_TOKEN env"
fi

grep_ok "T16 release.yml: runs 'make release TAG='" 'make release TAG=' "$RY"
grep_bad "T17 release.yml: no actions/upload-artifact" 'actions/upload-artifact' "$RY"
grep_bad "T18 release.yml: no workflow_dispatch" 'workflow_dispatch' "$RY"

# T19/T20: the old desktop release path is deleted.
if [ ! -e "$DY" ]; then ok "T19 .gitea/workflows/desktop.yml deleted"; else bad "T19 .gitea/workflows/desktop.yml deleted"; fi
if [ ! -e "$AR" ]; then ok "T20 desktop/scripts/attach-release.sh deleted"; else bad "T20 desktop/scripts/attach-release.sh deleted"; fi

# ── C: release-check behavior (shallow-clone fixtures — worktree untouched) ─

FIX=$(mktemp -d); workdirs="$workdirs $FIX"
git clone -q --depth 1 "$root" "$FIX/repo1" 2>/dev/null || { echo "❌ fixture clone failed" >&2; exit 1; }

# T21: TAG unset → non-zero + clear message naming TAG.
out=$(make -C "$FIX/repo1" release-check 2>&1); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qi 'TAG'; then
    ok "T21 release-check: TAG unset → fails, names TAG"
else
    bad "T21 release-check: TAG unset → fails, names TAG (rc=$rc: $(printf '%s' "$out" | head -1))"
fi

# T22: TAG != RELEASE_VERSION → non-zero + message names the mismatch.
out=$(make -C "$FIX/repo1" release-check TAG=99.99.99 2>&1); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q '99.99.99'; then
    ok "T22 release-check: TAG != version → fails, names the tag"
else
    bad "T22 release-check: TAG != version → fails, names the tag (rc=$rc: $(printf '%s' "$out" | head -1))"
fi

# T23: good TAG (== Cargo.toml version, notes present, versions in sync) → pass.
out=$(make -C "$FIX/repo1" release-check TAG="$V" 2>&1); rc=$?
if [ "$rc" -eq 0 ]; then
    ok "T23 release-check: TAG=$V → passes"
else
    bad "T23 release-check: TAG=$V → passes (rc=$rc: $(printf '%s' "$out" | head -1))"
fi

# T24: release-notes/<v>.md missing → non-zero + message names notes.
git clone -q --depth 1 "$root" "$FIX/repo2" 2>/dev/null
rm -f "$FIX/repo2/release-notes/$V.md"
out=$(make -C "$FIX/repo2" release-check TAG="$V" 2>&1); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qi 'notes'; then
    ok "T24 release-check: missing notes file → fails, names notes"
else
    bad "T24 release-check: missing notes file → fails, names notes (rc=$rc: $(printf '%s' "$out" | head -1))"
fi

# T25: root vs desktop version drift (check-version.sh fails) → non-zero
# with a message about the version drift.
git clone -q --depth 1 "$root" "$FIX/repo3" 2>/dev/null
sed -i "s/^version = \"$V\"/version = \"0.0.1\"/" "$FIX/repo3/desktop/Cargo.toml"
out=$(make -C "$FIX/repo3" release-check TAG="$V" 2>&1); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qi 'version'; then
    ok "T25 release-check: root/desktop version drift → fails"
else
    bad "T25 release-check: root/desktop version drift → fails (rc=$rc: $(printf '%s' "$out" | head -1))"
fi

# ── D: release-publish behavior (fake curl on PATH, canned JSON) ───────────
# (the fake curl + make_assets + run_publish live in the shared block up top)

# T26: one asset missing → non-zero, message names it, ZERO HTTP requests.
# (the NEW macOS updater tarball — proof the 10th asset is in the check loop)
make_assets "$D/out"
rm -f "$D/out/desktop/PeakBot-Desktop-$V-macos-universal.app.tar.gz"
run_publish "$D/log-missing"
if [ "$P_RC" -ne 0 ] && [ ! -s "$D/log-missing" ] \
    && printf '%s' "$P_OUT" | grep -qF "PeakBot-Desktop-$V-macos-universal.app.tar.gz"; then
    ok "T26 release-publish: missing asset → fails, names it, zero HTTP"
else
    logstate=$([ -f "$D/log-missing" ] && wc -l < "$D/log-missing" || echo absent)
    bad "T26 release-publish: missing asset → fails, names it, zero HTTP (rc=$P_RC, log lines=$logstate)"
fi

# T27: happy path — draft create, 10 exact uploads, final PATCH, no DELETE,
# and the explicit GITEA_URL/OWNER/REPO vars are used (override origin).
# The controlled stable tag $ST in the T08 fixture clone (the worktree
# version may be a prerelease).
run_publish "$D/log-happy" "" "$ST" "$D/stb"
t27=1; why=""
fail27() { t27=0; why="${why}${why:+; }$1"; }
[ "$P_RC" -eq 0 ] || fail27 "rc=$P_RC: $(printf '%s' "$P_OUT" | grep '❌' | head -1)"
lines=$(wc -l < "$D/log-happy" | tr -d ' ')
[ "$lines" = "12" ] || fail27 "log has $lines lines, want 12 (create+10 uploads+patch)"
grep -qF "POST $API/releases " "$D/log-happy" 2>/dev/null || fail27 "no create POST to $API/releases"
grep -qF '"draft":true' "$D/log-happy" 2>/dev/null || fail27 "create body lacks draft:true"
grep -qF "\"tag_name\":\"$ST\"" "$D/log-happy" 2>/dev/null || fail27 "create body lacks tag_name=$ST"
grep -qF "\"name\":\"$ST\"" "$D/log-happy" 2>/dev/null || fail27 "create body name is not exactly $ST"
grep -qF '"prerelease":false' "$D/log-happy" 2>/dev/null || fail27 "create body lacks prerelease:false (stable tag)"
# Sentinel: a long word from the fixture notes file — the assertion is
# "notes content reached the create body".
notes_word=$(grep -m1 -oE '[A-Za-z]{8,}' "$D/stb/release-notes/$ST.md" 2>/dev/null)
[ -n "$notes_word" ] || fail27 "fixture release-notes/$ST.md has no content to verify"
grep -qF "$notes_word" "$D/log-happy" 2>/dev/null || fail27 "create body lacks release-notes/$ST.md content"
upl=$(grep -cF 'assets?name=' "$D/log-happy" 2>/dev/null || true)
[ "$upl" = "10" ] || fail27 "$upl asset uploads, want 10"
for n in $ASSET10_ST; do
    c=$(grep -cF "name=$n " "$D/log-happy" 2>/dev/null || true)
    [ "$c" = "1" ] || fail27 "upload of $n: $c occurrence(s), want 1"
done
# Gitea's attachment API is multipart BY DESIGN — the uploads must stay
# -F attachment=@<file> (it is the GitHub mirror that takes raw bodies).
for n in $ASSET10_ST; do
    case $n in peakbot-*) p="$D/out/$n" ;; *) p="$D/out/desktop/$n" ;; esac
    grep -F "name=$n " "$D/log-happy" 2>/dev/null | grep -qF "FILE=attachment=@$p" \
        || fail27 "Gitea upload of $n is not multipart (-F attachment=@$p)"
done
# The old bare Windows desktop name must never be uploaded.
bare=$(grep -cF "name=PeakBot-Desktop-$ST-windows-x86_64.exe " "$D/log-happy" 2>/dev/null || true)
[ "$bare" = "0" ] || fail27 "bare PeakBot-Desktop-$ST-windows-x86_64.exe uploaded $bare times, want 0"
grep -qF "PATCH $API/releases/42" "$D/log-happy" 2>/dev/null \
    && grep -F 'PATCH ' "$D/log-happy" | grep -qF '"draft":false' \
    || fail27 "no final PATCH /releases/42 with draft:false"
grep -q '^DELETE' "$D/log-happy" 2>/dev/null && fail27 "DELETE present on happy path"
if [ "$t27" -eq 1 ]; then
    ok "T27 release-publish: happy path (draft create, 10 exact uploads, PATCH, no DELETE)"
else
    bad "T27 release-publish: happy path ($why)"
fi

# T28: an upload fails (3rd asset in spec order) → non-zero, the draft is
# DELETEd, and NO final PATCH draft:false.
make_assets "$D/out"
run_publish "$D/log-fail" peakbot-$V-macos-universal2
t28=1; why=""
[ "$P_RC" -ne 0 ] || { t28=0; why="rc=0 despite failed upload"; }
grep -qF "name=peakbot-$V-macos-universal2 " "$D/log-fail" 2>/dev/null || { t28=0; why="failing upload never attempted"; }
grep -qF "DELETE $API/releases/42" "$D/log-fail" 2>/dev/null || { t28=0; why="no DELETE /releases/42"; }
grep -q '^PATCH' "$D/log-fail" 2>/dev/null && { t28=0; why="PATCH draft:false after failure"; }
if [ "$t28" -eq 1 ]; then
    ok "T28 release-publish: upload failure → DELETE draft, non-zero, no PATCH"
else
    bad "T28 release-publish: upload failure → DELETE draft, non-zero, no PATCH ($why)"
fi

# ── E: `make -n release` step order (PRUNE=1 / no-PRUNE) ───────────────────

E_OUT=$(make -n release TAG="$V" PRUNE=1 CONTAINER_BUILDER=docker 2>&1)

# line_of <fixed-pattern> <which-occurrence> — line number in $E_OUT
line_of() {
    printf '%s\n' "$E_OUT" | grep -nF -- "$1" | awk -F: -v n="$2" 'NR==n {print $1; exit}'
}

chk=$(line_of 'release-check' 1)
[ -n "$chk" ] || chk=$(line_of 'check-version.sh' 1)
lin=$(line_of 'Dockerfile.linux' 1)
dlin=$(line_of 'build-desktop.sh linux' 1)
p1=$(line_of 'builder prune -af' 1)
mac=$(line_of 'Dockerfile.macos' 1)
dmac=$(line_of 'build-desktop.sh macos' 1)
p2=$(line_of 'builder prune -af' 2)
win=$(line_of 'Dockerfile.windows' 1)
dwin=$(line_of 'build-desktop.sh windows' 1)
p3=$(line_of 'builder prune -af' 3)
and=$(line_of 'Dockerfile.android' 1)
pub=$(printf '%s\n' "$E_OUT" | awk -v start="$and" \
    'NR>start && ($0 ~ /release-publish/ || $0 ~ /api\/v1\/repos/ || $0 ~ /GITEA/) {print NR; exit}')

# T29: every step marker is present.
t29=1; missing=""
for m in "release-check:$chk" "build-linux:$lin" "desktop-linux:$dlin" "prune1:$p1" \
         "build-macos:$mac" "desktop-macos:$dmac" "prune2:$p2" \
         "build-windows:$win" "desktop-windows:$dwin" "prune3:$p3" \
         "build-android:$and" "publish:$pub"; do
    [ -n "${m#*:}" ] || { t29=0; missing="$missing ${m%%:*}"; }
done
if [ "$t29" -eq 1 ]; then
    ok "T29 make -n release: all 12 step markers present"
else
    bad "T29 make -n release: all 12 step markers present (missing:$missing)"
fi

# T30: the steps appear in the spec order (strictly increasing line numbers).
t30=1; prev=0
for n in "$chk" "$lin" "$dlin" "$p1" "$mac" "$dmac" "$p2" "$win" "$dwin" "$p3" "$and" "$pub"; do
    if [ -z "$n" ] || [ "$n" -le "$prev" ]; then t30=0; break; fi
    prev=$n
done
if [ "$t30" -eq 1 ]; then
    ok "T30 make -n release: steps in spec order (check→linux→macos→windows→android→publish)"
else
    bad "T30 make -n release: steps in spec order (check→linux→macos→windows→android→publish)"
fi

# T31: exactly 3 prunes with PRUNE=1.
pcount=$(printf '%s\n' "$E_OUT" | grep -cF 'builder prune -af')
if [ "$pcount" = "3" ]; then
    ok "T31 make -n release PRUNE=1: exactly 3 'builder prune -af'"
else
    bad "T31 make -n release PRUNE=1: exactly 3 'builder prune -af' (got $pcount)"
fi

# T32: no prunes without PRUNE. Accepts either make-level conditional
# inclusion (prune lines absent from -n output) or a shell guard on the
# same line (if [ "$(PRUNE)" = "1" ] … builder prune -af …).
E_OUT2=$(make -n release TAG="$V" CONTAINER_BUILDER=docker 2>&1)
pcount2=$(printf '%s\n' "$E_OUT2" | grep -cF 'builder prune -af')
unguarded=$(printf '%s\n' "$E_OUT2" | grep -F 'builder prune -af' | grep -cvF 'PRUNE' || true)
if [ "$pcount2" = "0" ] || [ "$unguarded" = "0" ]; then
    ok "T32 make -n release (no PRUNE): no unguarded prunes"
else
    bad "T32 make -n release (no PRUNE): no unguarded prunes ($unguarded of $pcount2 prune lines lack a PRUNE guard)"
fi

# T33: the release recipe has no bump/tag steps (releases are PR'd manually).
if ! printf '%s\n' "$E_OUT" | grep -qE 'release-(bump|tag)'; then
    ok "T33 make -n release: no release-bump/release-tag steps"
else
    bad "T33 make -n release: no release-bump/release-tag steps"
fi

# ── F/G: release-bump + release-tag in throwaway clones ────────────────────
# Local bare repo as `origin`; `cargo` stubbed to a no-op on PATH (lockfile
# assertions stay loose — only the Cargo.toml version bumps are exact).

F=$(mktemp -d); workdirs="$workdirs $F"
git clone -q --bare "$root" "$F/origin.git" 2>/dev/null || { echo "❌ bare origin clone failed" >&2; exit 1; }
# The fixture's master must be the code under test: point it at the worktree's
# HEAD (the implementation branch), not at the pre-rework master.
git -C "$F/origin.git" update-ref refs/heads/master "$(git -C "$root" rev-parse HEAD)"
# Drop the repo's real release tag for $V from the fixture origin too, so
# tag-state assertions are deterministic (T41/T42 create it via make).
git -C "$F/origin.git" tag -d "$V" >/dev/null 2>&1 || true
mkdir -p "$F/bin"
printf '#!/bin/sh\nexit 0\n' > "$F/bin/cargo"
chmod +x "$F/bin/cargo"

fresh_work() {
    rm -rf "$F/work"
    git clone -q --depth 1 -b master "$F/origin.git" "$F/work" 2>/dev/null \
        || { echo "❌ work clone failed" >&2; exit 1; }
    git -C "$F/work" config user.name "Release Test"
    git -C "$F/work" config user.email "release-test@example.com"
    # Drop any tag the clone fetched for $V so tag-state assertions are
    # deterministic (T41 creates it, T42 must create it via make).
    git -C "$F/work" tag -d "$V" >/dev/null 2>&1 || true
}

# run_bump <version> → sets B_RC / B_OUT   (run_tag → T_RC / T_OUT)
run_bump() {
    B_OUT=$( cd "$F/work" && PATH="$F/bin:$PATH" \
      make release-bump VERSION="$1" </dev/null 2>&1 )
    B_RC=$?
}
run_tag() {
    T_OUT=$( cd "$F/work" && PATH="$F/bin:$PATH" \
      make release-tag </dev/null 2>&1 )
    T_RC=$?
}

# T34: release-bump with no VERSION → non-zero, NO interactive prompt,
# message names VERSION.
fresh_work
run_bump ""
if [ "$B_RC" -ne 0 ] && ! printf '%s' "$B_OUT" | grep -qF 'New version:' \
    && printf '%s' "$B_OUT" | grep -qi 'VERSION'; then
    ok "T34 release-bump: no VERSION → fails, no prompt, names VERSION"
else
    bad "T34 release-bump: no VERSION → fails, no prompt, names VERSION (rc=$B_RC: $(printf '%s' "$B_OUT" | head -1))"
fi

# T35: non-semver VERSION → non-zero, no branch created.
fresh_work
run_bump "banana"
if [ "$B_RC" -ne 0 ] && printf '%s' "$B_OUT" | grep -qi 'invalid' \
    && [ -z "$(git -C "$F/work" branch --list release/banana 2>/dev/null)" ]; then
    ok "T35 release-bump: invalid semver → fails, no branch"
else
    bad "T35 release-bump: invalid semver → fails, no branch (rc=$B_RC: $(printf '%s' "$B_OUT" | head -1))"
fi

# T36: tag already exists → non-zero.
fresh_work
git -C "$F/work" tag 9.8.7
run_bump "9.8.7"
if [ "$B_RC" -ne 0 ] && printf '%s' "$B_OUT" | grep -qF '9.8.7'; then
    ok "T36 release-bump: existing tag → fails"
else
    bad "T36 release-bump: existing tag → fails (rc=$B_RC: $(printf '%s' "$B_OUT" | head -1))"
fi

# T37: dirty tree → non-zero (no ALLOW_DIRTY override).
fresh_work
echo "# dirty" >> "$F/work/Cargo.toml"
run_bump "9.9.8"
if [ "$B_RC" -ne 0 ] && printf '%s' "$B_OUT" | grep -qiE 'dirty|uncommitted'; then
    ok "T37 release-bump: dirty tree → fails"
else
    bad "T37 release-bump: dirty tree → fails (rc=$B_RC: $(printf '%s' "$B_OUT" | head -1))"
fi

# T38: happy bump VERSION=9.9.9 — branch release/9.9.9 from origin/master,
# Cargo.toml + desktop/Cargo.toml bumped, notes promoted, committed, pushed,
# no .release-version.
# Seed the fixture origin's master with a draft entry so the promotion moves
# real content and the commit touches current.md — independent of the
# worktree's (possibly already-promoted) current.md.
fresh_work
printf -- '- T38 sentinel draft entry.\n' >> "$F/work/release-notes/current.md"
git -C "$F/work" commit -qam "test: seed notes draft"
git -C "$F/work" push -q origin master
fresh_work
awk '/^## /{f=1} f' "$F/work/release-notes/current.md" > "$F/expected_notes"
awk '/^## /{exit} {print}' "$F/work/release-notes/current.md" > "$F/old_header"
run_bump "9.9.9"
t38=1; why=""
fail38() { t38=0; why="${why}${why:+; }$1"; }
[ "$B_RC" -eq 0 ] || fail38 "rc=$B_RC: $(printf '%s' "$B_OUT" | grep '❌' | head -1)"
branch=$(git -C "$F/work" rev-parse --abbrev-ref HEAD 2>/dev/null)
[ "$branch" = "release/9.9.9" ] || fail38 "on branch '$branch', want release/9.9.9"
rv=$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' "$F/work/Cargo.toml")
[ "$rv" = "9.9.9" ] || fail38 "Cargo.toml version $rv"
dv=$(awk '/^\[workspace\.package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' "$F/work/desktop/Cargo.toml")
[ "$dv" = "9.9.9" ] || fail38 "desktop/Cargo.toml version $dv"
committed=$(git -C "$F/work" show --name-only --format= HEAD 2>/dev/null)
for f in Cargo.toml desktop/Cargo.toml release-notes/9.9.9.md release-notes/current.md; do
    printf '%s\n' "$committed" | grep -qx "$f" || fail38 "HEAD commit does not touch $f"
done
cmp -s "$F/expected_notes" "$F/work/release-notes/9.9.9.md" 2>/dev/null \
    || fail38 "release-notes/9.9.9.md is not current.md from first '## '"
nh=$(wc -l < "$F/old_header" | tr -d ' ')
head -n "$nh" "$F/work/release-notes/current.md" > "$F/new_head" 2>/dev/null
cmp -s "$F/old_header" "$F/new_head" 2>/dev/null \
    || fail38 "fresh current.md lost the working-draft header"
grep -q '^## Changes' "$F/work/release-notes/current.md" 2>/dev/null \
    || fail38 "fresh current.md lacks '## Changes'"
[ -n "$(git -C "$F/work" ls-remote origin refs/heads/release/9.9.9 2>/dev/null)" ] \
    || fail38 "release/9.9.9 not pushed to origin"
[ ! -e "$F/work/.release-version" ] || fail38 ".release-version file created"
if [ "$t38" -eq 1 ]; then
    ok "T38 release-bump: happy path (branch, bumps, notes promoted, commit, push, no state file)"
else
    bad "T38 release-bump: happy path ($why)"
fi
# Restore the shared fixture origin (T39+ must see the unseeded master).
git -C "$F/origin.git" update-ref refs/heads/master "$(git -C "$root" rev-parse HEAD)"

# T39: release-tag with HEAD != origin/master → non-zero, names master.
fresh_work
git -C "$F/work" commit -q --allow-empty -m "test: ahead of origin"
run_tag
if [ "$T_RC" -ne 0 ] && printf '%s' "$T_OUT" | grep -qi 'master'; then
    ok "T39 release-tag: HEAD != origin/master → fails"
else
    bad "T39 release-tag: HEAD != origin/master → fails (rc=$T_RC: $(printf '%s' "$T_OUT" | head -1))"
fi

# T40: release-tag with release-notes/<v>.md missing (clean tree, HEAD in
# sync — the notes file is committed away) → non-zero, names notes.
# Uses a PRIVATE bare origin: the shared one must keep the notes file for
# T41/T42 (tests stay independent).
fresh_work
git clone -q --bare "$F/work" "$F/origin-t40.git" 2>/dev/null
git -C "$F/work" remote set-url origin "$F/origin-t40.git"
git -C "$F/work" rm -q "release-notes/$V.md"
git -C "$F/work" commit -q -m "test: drop notes"
git -C "$F/work" push -q origin master
run_tag
if [ "$T_RC" -ne 0 ] && printf '%s' "$T_OUT" | grep -qi 'notes'; then
    ok "T40 release-tag: missing notes → fails"
else
    bad "T40 release-tag: missing notes → fails (rc=$T_RC: $(printf '%s' "$T_OUT" | head -1))"
fi

# T41: release-tag when the tag already exists → non-zero.
fresh_work
git -C "$F/work" tag -a "$V" -m "existing"
run_tag
if [ "$T_RC" -ne 0 ] && printf '%s' "$T_OUT" | grep -qF "$V"; then
    ok "T41 release-tag: existing tag → fails"
else
    bad "T41 release-tag: existing tag → fails (rc=$T_RC: $(printf '%s' "$T_OUT" | head -1))"
fi

# T42: release-tag happy path — annotated tag whose message is the notes
# file, pushed to origin.
fresh_work
run_tag
t42=1; why=""
fail42() { t42=0; why="${why}${why:+; }$1"; }
[ "$T_RC" -eq 0 ] || fail42 "rc=$T_RC: $(printf '%s' "$T_OUT" | grep '❌' | head -1)"
tt=$(git -C "$F/work" cat-file -t "refs/tags/$V" 2>/dev/null)
[ "$tt" = "tag" ] || fail42 "tag object type '$tt', want annotated (tag)"
git -C "$F/work" tag -l --format='%(contents)' "$V" 2>/dev/null | tr -d '\n' > "$F/tagmsg"
tr -d '\n' < "$F/work/release-notes/$V.md" 2>/dev/null > "$F/notesflat"
cmp -s "$F/tagmsg" "$F/notesflat" 2>/dev/null \
    || fail42 "tag message != release-notes/$V.md"
[ -n "$(git -C "$F/work" ls-remote origin "refs/tags/$V" 2>/dev/null)" ] \
    || fail42 "tag not pushed to origin"
if [ "$t42" -eq 1 ]; then
    ok "T42 release-tag: happy path (annotated tag, notes as message, pushed)"
else
    bad "T42 release-tag: happy path ($why)"
fi

# ── H: release.yml GITEA_TOKEN preflight (fail fast on empty secret) ───────

# T43: an empty RELEASE_TOKEN secret must not burn 1-2 h of docker builds
# before failing in release-publish. Statically against release.yml:
#   1. a non-empty test on GITEA_TOKEN exists — `-n`/`-z` with the quoted
#      variable, `$GITEA_TOKEN` or `${GITEA_TOKEN}` (the unquoted form is
#      deliberately NOT accepted: `[ -n $VAR ]` with an empty VAR is the
#      one-argument form and tests TRUE, so quotes are part of the contract);
#   2. that test's line number is strictly BEFORE the `make release` line;
#   3. the workflow never echoes/prints the token value.
tok_re='-[nz][[:space:]]+"\$\{?GITEA_TOKEN\}?"'
t43=1; why=""
fail43() { t43=0; why="${why}${why:+; }$1"; }
tok_line=$(grep -nE -- "$tok_re" "$RY" 2>/dev/null | head -n 1 | cut -d: -f1)
mr_line=$(grep -nF -- 'make release' "$RY" 2>/dev/null | head -n 1 | cut -d: -f1)
[ -n "$tok_line" ] || fail43 'no non-empty test on GITEA_TOKEN (want e.g. [ -n "$GITEA_TOKEN" ] || exit 1)'
[ -n "$mr_line" ] || fail43 'no make-release step found in release.yml'
if [ -n "$tok_line" ] && [ -n "$mr_line" ] && [ "$tok_line" -ge "$mr_line" ]; then
    fail43 "token test (line $tok_line) is not before make release (line $mr_line)"
fi
grep -qE -- '(echo|printf)[[:space:]].*\$\{?GITEA_TOKEN\}?' "$RY" 2>/dev/null \
    && fail43 'workflow echoes/prints the GITEA_TOKEN value'
if [ "$t43" -eq 1 ]; then
    ok "T43 release.yml: GITEA_TOKEN preflight (non-empty test before make release, value never printed)"
else
    bad "T43 release.yml: GITEA_TOKEN preflight ($why)"
fi

# ── I: auto-updater pipeline (10 assets, release-publish-github) ───────────

# T44: the Makefile defines a shared RELEASE_ASSETS with EXACTLY 10 entries:
# the 8 legacy + PeakBot-Desktop-<v>-macos-universal.app.tar.gz + latest.json.
ra_raw=$(awk '
    /^RELEASE_ASSETS[ \t]*[:?]?=/ { p = 1 }
    p {
        l = $0
        if (l ~ /\\[ \t]*$/) { sub(/\\[ \t]*$/, "", l); buf = buf " " l; next }
        print buf " " l; exit
    }' "$MK" 2>/dev/null)
ra_n=$(printf '%s' "$ra_raw" | wc -w | tr -d ' ')
t44=1; why=""
[ "${ra_n:-0}" = "10" ] || { t44=0; why="RELEASE_ASSETS has ${ra_n:-0} entries, want 10"; }
for n in "PeakBot-Desktop-\$\$v-macos-universal.app.tar.gz" "latest.json"; do
    printf '%s' "$ra_raw" | grep -qF -- "$n" \
        || { t44=0; why="${why}${why:+; }missing $n"; }
done
if [ "$t44" -eq 1 ]; then
    ok "T44 Makefile RELEASE_ASSETS: exactly 10 entries incl. the mac tarball + latest.json"
else
    bad "T44 Makefile RELEASE_ASSETS: exactly 10 entries incl. the mac tarball + latest.json ($why)"
fi

# T45: release-publish-github with an EMPTY GH_RELEASE_TOKEN → non-zero
# BEFORE any HTTP request, message names GH_RELEASE_TOKEN.
make_assets "$D/out"
run_gh "$root" "$D/log-gh-notoken" "$V" ""
if [ "$G_RC" -ne 0 ] && [ ! -s "$D/log-gh-notoken" ] \
    && printf '%s' "$G_OUT" | grep -q 'GH_RELEASE_TOKEN'; then
    ok "T45 release-publish-github: empty token → fails before any request, names it"
else
    bad "T45 release-publish-github: empty token → fails before any request, names it (rc=$G_RC: $(printf '%s' "$G_OUT" | head -1))"
fi

# T46: release-publish-github happy path (stable tag, DEFAULT GH_MIRROR_REPO):
# polls the tag ref (fake: 404, 404, 200 → proceeds after exactly 3 GETs),
# looks up releases/tags/<v>, creates a DRAFT (draft:true, tag_name,
# prerelease:false) on api.github.com, uploads ALL 10 assets to
# uploads.github.com (201 each), PATCHes draft:false + make_latest:"true",
# no DELETE. Auth is "Authorization: Bearer <token>" and the token never
# reaches stdout/stderr. The controlled stable tag $ST runs in the T08
# fixture clone (the worktree version may be a prerelease).
run_gh "$D/stb" "$D/log-gh-happy" "$ST"
L=$D/log-gh-happy
GHAPI="https://api.github.com/repos/thepeak99/peakbot"
GHUP="https://uploads.github.com/repos/thepeak99/peakbot"
t46=1; why=""
fail46() { t46=0; why="${why}${why:+; }$1"; }
[ "$G_RC" -eq 0 ] || fail46 "rc=$G_RC: $(printf '%s' "$G_OUT" | grep '❌' | head -1)"
refn=$(grep -cF "GET $GHAPI/git/ref/tags/$ST" "$L" 2>/dev/null || true)
[ "$refn" = "3" ] || fail46 "tag-ref polls: $refn, want 3 (404,404,200)"
grep -qF "GET $GHAPI/releases/tags/$ST" "$L" 2>/dev/null \
    || fail46 "no GET releases/tags/$ST"
grep -qF "POST $GHAPI/releases " "$L" 2>/dev/null \
    || fail46 "no draft-create POST to $GHAPI/releases (default GH_MIRROR_REPO)"
grep -qF '"draft":true' "$L" 2>/dev/null || fail46 "create body lacks draft:true"
grep -qF "\"tag_name\":\"$ST\"" "$L" 2>/dev/null || fail46 "create body lacks tag_name=$ST"
grep -qF '"prerelease":false' "$L" 2>/dev/null || fail46 "create body lacks prerelease:false"
upln=$(grep -cF "POST $GHUP/releases/77/assets?name=" "$L" 2>/dev/null || true)
[ "$upln" = "10" ] || fail46 "$upln GitHub uploads, want 10"
for n in $ASSET10_ST; do
    c=$(grep -cF "name=$n " "$L" 2>/dev/null || true)
    [ "$c" = "1" ] || fail46 "upload of $n: $c occurrence(s), want 1"
done
# GitHub's asset endpoint takes the RAW file body — every upload must be
# --data-binary @<file>; multipart (-F/--form) must never appear.
ghmp=$(grep -F "POST $GHUP/" "$L" 2>/dev/null | grep -cF 'FILE=' || true)
[ "$ghmp" = "0" ] || fail46 "$ghmp GitHub upload(s) use multipart -F"
for n in $ASSET10_ST; do
    case $n in peakbot-*) p="$D/out/$n" ;; *) p="$D/out/desktop/$n" ;; esac
    grep -F "name=$n " "$L" 2>/dev/null | grep -qF "DATA=@$p" \
        || fail46 "GitHub upload of $n is not raw (--data-binary @$p)"
done
grep -q '^DELETE' "$L" 2>/dev/null && fail46 "DELETE present on happy path"
grep -qF "PATCH $GHAPI/releases/77" "$L" 2>/dev/null \
    && grep -F 'PATCH ' "$L" | grep -qF '"draft":false' \
    && grep -F 'PATCH ' "$L" | grep -qF '"make_latest":"true"' \
    || fail46 "no final PATCH draft:false + make_latest:\"true\""
grep -qF 'Authorization: Bearer ghsecret-token-123' "$L" 2>/dev/null \
    || fail46 "no 'Authorization: Bearer <token>' header in the request log"
printf '%s' "$G_OUT" | grep -qF 'ghsecret-token-123' \
    && fail46 "token leaked into stdout/stderr"
if [ "$t46" -eq 1 ]; then
    ok "T46 release-publish-github: happy path (poll, draft, 10 uploads, PATCH make_latest:true, Bearer auth, no token leak)"
else
    bad "T46 release-publish-github: happy path ($why)"
fi

# T47: release-publish-github with a '-' TAG: create body prerelease:true and
# final PATCH make_latest:"false" (same 10-asset path otherwise). Runs in the
# T08 clone, which has release-notes/0.9.0-rc.1.md.
make_assets "$D/out"
# The rc run looks for TAG-named assets — create the 0.9.0-rc.1 set too.
make_assets "$D/out" 0.9.0-rc.1
run_gh "$D/pre" "$D/log-gh-pre" 0.9.0-rc.1
L=$D/log-gh-pre
t47=1; why=""
fail47() { t47=0; why="${why}${why:+; }$1"; }
[ "$G_RC" -eq 0 ] || fail47 "rc=$G_RC: $(printf '%s' "$G_OUT" | grep '❌' | head -1)"
grep -qF '"prerelease":true' "$L" 2>/dev/null || fail47 "create body lacks prerelease:true"
grep -qF '"prerelease":false' "$L" 2>/dev/null && fail47 "create body has prerelease:false"
upln=$(grep -cF "POST $GHUP/releases/77/assets?name=" "$L" 2>/dev/null || true)
[ "$upln" = "10" ] || fail47 "$upln GitHub uploads, want 10"
ghmp=$(grep -F "POST $GHUP/" "$L" 2>/dev/null | grep -cF 'FILE=' || true)
[ "$ghmp" = "0" ] || fail47 "$ghmp GitHub upload(s) use multipart -F"
rawn=$(grep -F "POST $GHUP/" "$L" 2>/dev/null | grep -cF 'DATA=@' || true)
[ "$rawn" = "10" ] || fail47 "$rawn raw GitHub uploads (--data-binary), want 10"
grep -qF "PATCH $GHAPI/releases/77" "$L" 2>/dev/null \
    && grep -F 'PATCH ' "$L" | grep -qF '"make_latest":"false"' \
    || fail47 "no final PATCH with make_latest:\"false\""
if [ "$t47" -eq 1 ]; then
    ok "T47 release-publish-github: '-' tag → prerelease:true, make_latest:\"false\""
else
    bad "T47 release-publish-github: '-' tag → prerelease:true, make_latest:\"false\" ($why)"
fi

# T48: the tag never appears on GitHub (push-mirror lag): the fake always
# 404s, and GH_TAG_WAIT_TRIES=2 + GH_TAG_WAIT_SECS=0 cap the wait →
# non-zero after exactly 2 ref GETs, and NO release created.
make_assets "$D/out"
GH_REF_ALWAYS_404=1 GH_TAG_WAIT_TRIES=2 run_gh "$root" "$D/log-gh-404" "$V"
L=$D/log-gh-404
t48=1; why=""
[ "$G_RC" -ne 0 ] || { t48=0; why="rc=0 despite the tag never appearing"; }
refn=$(grep -cF "GET $GHAPI/git/ref/tags/$V" "$L" 2>/dev/null || true)
[ "$refn" = "2" ] || { t48=0; why="${why}${why:+; }tag-ref polls: $refn, want 2 (GH_TAG_WAIT_TRIES=2)"; }
cren=$(grep -cF "POST $GHAPI/releases " "$L" 2>/dev/null || true)
[ "$cren" = "0" ] || { t48=0; why="${why}${why:+; }$cren release create(s) despite missing tag"; }
if [ "$t48" -eq 1 ]; then
    ok "T48 release-publish-github: tag never appears → fails after GH_TAG_WAIT_TRIES, no release created"
else
    bad "T48 release-publish-github: tag never appears → fails after GH_TAG_WAIT_TRIES, no release created ($why)"
fi

# T49: idempotent — a published release for the tag already exists with all
# 10 asset names AND sizes → exit 0 with NO create, NO uploads, NO PATCH.
# doneassets holds one JSON object per line {"name":…,"size":…} (the shape
# real GitHub returns in .assets[]); the sizes match the 1-byte fixture
# files, so the size check passes.
make_assets "$D/out"
printf '%s\n' "$ASSET10" | jq -Rc '{name:., size:1}' > "$D/log-gh-idem.doneassets"
GH_REL_MODE=done run_gh "$root" "$D/log-gh-idem" "$V"
L=$D/log-gh-idem
t49=1; why=""
[ "$G_RC" -eq 0 ] || { t49=0; why="rc=$G_RC: $(printf '%s' "$G_OUT" | grep '❌' | head -1)"; }
grep -qF "GET $GHAPI/releases/tags/$V" "$L" 2>/dev/null \
    || { t49=0; why="${why}${why:+; }no GET releases/tags/$V"; }
cren=$(grep -cF "POST $GHAPI/releases " "$L" 2>/dev/null || true)
[ "$cren" = "0" ] || { t49=0; why="${why}${why:+; }$cren release create(s), want 0"; }
upln=$(grep -cF "POST $GHUP/" "$L" 2>/dev/null || true)
[ "$upln" = "0" ] || { t49=0; why="${why}${why:+; }$upln upload(s), want 0"; }
grep -q '^PATCH' "$L" 2>/dev/null && { t49=0; why="${why}${why:+; }PATCH present, want 0"; }
if [ "$t49" -eq 1 ]; then
    ok "T49 release-publish-github: existing complete release → exit 0, no create/uploads/PATCH (idempotent)"
else
    bad "T49 release-publish-github: existing complete release → exit 0, no create/uploads/PATCH ($why)"
fi

# T50: a leftover DRAFT for the tag → deleted, then recreated (draft →
# 10 uploads → PATCH draft:false + make_latest:"true"), rc 0.
make_assets "$D/out"
GH_REL_MODE=draft run_gh "$root" "$D/log-gh-draft" "$V"
L=$D/log-gh-draft
t50=1; why=""
fail50() { t50=0; why="${why}${why:+; }$1"; }
[ "$G_RC" -eq 0 ] || fail50 "rc=$G_RC: $(printf '%s' "$G_OUT" | grep '❌' | head -1)"
deln=$(grep -cF "DELETE $GHAPI/releases/77" "$L" 2>/dev/null || true)
[ "$deln" -ge 1 ] 2>/dev/null || fail50 "no DELETE of the leftover draft"
cren=$(grep -cF "POST $GHAPI/releases " "$L" 2>/dev/null || true)
[ "$cren" = "1" ] || fail50 "$cren release create(s), want 1 (recreate)"
upln=$(grep -cF "POST $GHUP/releases/77/assets?name=" "$L" 2>/dev/null || true)
[ "$upln" = "10" ] || fail50 "$upln uploads, want 10"
grep -qF "PATCH $GHAPI/releases/77" "$L" 2>/dev/null \
    && grep -F 'PATCH ' "$L" | grep -qF '"draft":false' \
    || fail50 "no final PATCH draft:false"
if [ "$t50" -eq 1 ]; then
    ok "T50 release-publish-github: leftover draft → deleted, recreated, published"
else
    bad "T50 release-publish-github: leftover draft → deleted, recreated, published ($why)"
fi

# T51: any upload failure → the draft is DELETEd and the target exits
# non-zero with NO final PATCH.
make_assets "$D/out"
GH_FAIL_UPLOAD=latest.json run_gh "$root" "$D/log-gh-fail" "$V"
L=$D/log-gh-fail
t51=1; why=""
fail51() { t51=0; why="${why}${why:+; }$1"; }
[ "$G_RC" -ne 0 ] || fail51 "rc=0 despite a failed upload"
grep -qF "name=latest.json " "$L" 2>/dev/null || fail51 "failing upload (latest.json) never attempted"
grep -qF "DELETE $GHAPI/releases/77" "$L" 2>/dev/null || fail51 "no DELETE of the draft"
grep -q '^PATCH' "$L" 2>/dev/null && fail51 "PATCH draft:false after failure"
if [ "$t51" -eq 1 ]; then
    ok "T51 release-publish-github: upload failure → draft DELETEd, non-zero, no PATCH"
else
    bad "T51 release-publish-github: upload failure → draft DELETEd, non-zero, no PATCH ($why)"
fi

# T52: `make -n release` shows the order: desktop-updater BEFORE
# release-publish BEFORE release-publish-github.
EO2=$(make -n release TAG="$V" 2>&1)
l_up=$(printf '%s\n' "$EO2" | grep -nF 'desktop-updater' | awk -F: 'NR==1 {print $1; exit}')
l_pub=$(printf '%s\n' "$EO2" | grep -nF 'release-publish' | awk -F: 'NR==1 {print $1; exit}')
l_gh=$(printf '%s\n' "$EO2" | grep -nF 'release-publish-github' | awk -F: 'NR==1 {print $1; exit}')
if [ -n "$l_up" ] && [ -n "$l_pub" ] && [ -n "$l_gh" ] \
    && [ "$l_up" -lt "$l_pub" ] && [ "$l_pub" -lt "$l_gh" ]; then
    ok "T52 make -n release: desktop-updater → release-publish → release-publish-github"
else
    bad "T52 make -n release: desktop-updater → release-publish → release-publish-github (up=${l_up:-?} pub=${l_pub:-?} gh=${l_gh:-?})"
fi

# T53: release-check fails when tauri.conf.json plugins.updater endpoints do
# NOT contain GH_MIRROR_REPO (the updater would fetch from the wrong host).
# Same clone-mutation pattern as T24/T25 — the worktree stays untouched.
git clone -q --depth 1 "$root" "$FIX/repo4" 2>/dev/null \
    || { echo "❌ fixture clone repo4 failed" >&2; exit 1; }
sed -i 's#thepeak99/peakbot#wrongorg/wrongrepo#' "$FIX/repo4/desktop/src-tauri/tauri.conf.json"
out=$(make -C "$FIX/repo4" release-check TAG="$V" 2>&1); rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qiE 'endpoint|mirror'; then
    ok "T53 release-check: updater endpoint without GH_MIRROR_REPO → fails, names it"
else
    bad "T53 release-check: updater endpoint without GH_MIRROR_REPO → fails, names it (rc=$rc: $(printf '%s' "$out" | head -1))"
fi

# T54: release.yml maps the new secrets into env (GH_RELEASE_TOKEN,
# TAURI_SIGNING_PRIVATE_KEY, TAURI_SIGNING_PRIVATE_KEY_PASSWORD), each with
# a non-empty preflight BEFORE `make release` (the T43 pattern for
# GITEA_TOKEN).
t54=1; why=""
fail54() { t54=0; why="${why}${why:+; }$1"; }
grep -qF 'secrets.GH_RELEASE_TOKEN' "$RY" 2>/dev/null \
    || fail54 "no secrets.GH_RELEASE_TOKEN mapping"
grep -qF 'secrets.TAURI_SIGNING_PRIVATE_KEY }' "$RY" 2>/dev/null \
    || fail54 "no secrets.TAURI_SIGNING_PRIVATE_KEY mapping"
grep -qF 'secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD' "$RY" 2>/dev/null \
    || fail54 "no secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD mapping"
mr_line=$(grep -nF -- 'make release' "$RY" 2>/dev/null | head -n 1 | cut -d: -f1)
for var in GH_RELEASE_TOKEN TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD; do
    # Single-quoted like T43's tok_re: the pattern must keep \$ (escaped
    # dollar) and \{? / \}? — a double-quoted pattern would turn \$ into a
    # bare mid-pattern $ (an anchor in grep's ERE) and never match.
    pre='-[nz][[:space:]]+"\$\{?'
    suf='\}?"'
    re="${pre}${var}${suf}"
    vl=$(grep -nE -- "$re" "$RY" 2>/dev/null | head -n 1 | cut -d: -f1)
    [ -n "$vl" ] || { fail54 "no non-empty test on $var"; continue; }
    if [ -n "$mr_line" ] && [ "$vl" -ge "$mr_line" ]; then
        fail54 "$var non-empty test is not before make release"
    fi
done
if [ "$t54" -eq 1 ]; then
    ok "T54 release.yml: GH_RELEASE_TOKEN + TAURI_SIGNING* secrets → env, non-empty preflight before make release"
else
    bad "T54 release.yml: GH_RELEASE_TOKEN + TAURI_SIGNING* secrets → env, non-empty preflight before make release ($why)"
fi

# ── J: .dockerignore — the updater signing build context ───────────────────

# T55: every path Dockerfile.desktop-updater COPYs must survive
# .dockerignore. 0.25.0-rc.1 died in the desktop-updater step with
# "release-notes/0.25.0-rc.1.md: not found" — the root .dockerignore
# excluded output/* (only the 3 unversioned sidecars re-included),
# release-notes/ and *.md, so the 3 updater artifacts + the notes file
# never reached the build context. Simulated with git check-ignore (same
# pattern semantics as docker/podman): parse the COPY sources, substitute
# the ARGs (VERSION / NOTES), and require each path NOT to be ignored —
# while stale output/ and root *.md stay excluded.
T55V=0.25.0-rc.1
T55D=$(mktemp -d); workdirs="$workdirs $T55D"
git init -q "$T55D"
cp .dockerignore "$T55D/.gitignore"
t55_srcs=$(grep -E '^[[:space:]]*COPY' Dockerfile.desktop-updater \
    | grep -vF -- '--from=' | awk '
        { for (i = 1; i <= NF; i++)
            if ($i != "COPY" && $i !~ /^--/) { print $i; break } }')
t55_paths=$(printf '%s\n' "$t55_srcs" \
    | sed -e "s/\${VERSION}/$T55V/g" -e "s/\${NOTES}/release-notes\/$T55V.md/g")
t55_expected="output/desktop/PeakBot-Desktop-$T55V-linux-x86_64.AppImage
output/desktop/PeakBot-Desktop-$T55V-windows-x86_64-setup.exe
output/desktop/PeakBot-Desktop-$T55V-macos-universal.app.tar.gz
desktop/src-tauri/tauri.conf.json
desktop/scripts/updater-manifest.sh
release-notes/$T55V.md"
t55=1; why=""
fail55() { t55=0; why="${why}${why:+; }$1"; }
[ -n "$t55_srcs" ] || fail55 "no COPY sources parsed from Dockerfile.desktop-updater"
[ "$t55_paths" = "$t55_expected" ] \
    || fail55 "parsed COPY sources differ from the 6 expected (Dockerfile changed?)"
for p in $t55_expected; do
    if git -C "$T55D" check-ignore --no-index --quiet -- "$p" 2>/dev/null; then
        fail55 "$p is excluded by .dockerignore"
    fi
done
if ! git -C "$T55D" check-ignore --no-index --quiet -- output/stale-peakbot 2>/dev/null; then
    fail55 "output/stale-peakbot NOT excluded (output/* broken?)"
fi
if ! git -C "$T55D" check-ignore --no-index --quiet -- README.md 2>/dev/null; then
    fail55 "README.md NOT excluded (*.md broken?)"
fi
if [ "$t55" -eq 1 ]; then
    ok "T55 .dockerignore: all Dockerfile.desktop-updater COPY sources reach the build context"
else
    bad "T55 .dockerignore: updater build context ($why)"
fi

# T56: release-bump with a PRERELEASE VERSION (9.9.10-rc.1) — branch
# release/<v>, both Cargo.tomls bumped, release-notes/<v>.md created from
# current.md's first `## ` section, and current.md left UNTOUCHED (notes
# keep accumulating for the stable release). T38-style fixture.
fresh_work
printf -- '- T56 sentinel draft entry.\n' >> "$F/work/release-notes/current.md"
git -C "$F/work" commit -qam "test: seed notes draft"
git -C "$F/work" push -q origin master
fresh_work
cp "$F/work/release-notes/current.md" "$F/t56_current_before"
awk '/^## /{f=1} f' "$F/work/release-notes/current.md" > "$F/t56_expected_notes"
run_bump "9.9.10-rc.1"
t56=1; why=""
fail56() { t56=0; why="${why}${why:+; }$1"; }
[ "$B_RC" -eq 0 ] || fail56 "rc=$B_RC: $(printf '%s' "$B_OUT" | grep '❌' | head -1)"
branch=$(git -C "$F/work" rev-parse --abbrev-ref HEAD 2>/dev/null)
[ "$branch" = "release/9.9.10-rc.1" ] || fail56 "on branch '$branch', want release/9.9.10-rc.1"
rv=$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' "$F/work/Cargo.toml")
[ "$rv" = "9.9.10-rc.1" ] || fail56 "Cargo.toml version $rv"
dv=$(awk '/^\[workspace\.package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' "$F/work/desktop/Cargo.toml")
[ "$dv" = "9.9.10-rc.1" ] || fail56 "desktop/Cargo.toml version $dv"
committed=$(git -C "$F/work" show --name-only --format= HEAD 2>/dev/null)
for f in Cargo.toml desktop/Cargo.toml release-notes/9.9.10-rc.1.md; do
    printf '%s\n' "$committed" | grep -qx "$f" || fail56 "HEAD commit does not touch $f"
done
printf '%s\n' "$committed" | grep -qx 'release-notes/current.md' \
    && fail56 "HEAD commit touches release-notes/current.md (RC must leave it untouched)"
cmp -s "$F/t56_expected_notes" "$F/work/release-notes/9.9.10-rc.1.md" 2>/dev/null \
    || fail56 "release-notes/9.9.10-rc.1.md is not current.md from first '## '"
cmp -s "$F/t56_current_before" "$F/work/release-notes/current.md" 2>/dev/null \
    || fail56 "release-notes/current.md was modified (RC must keep accumulating)"
[ -n "$(git -C "$F/work" ls-remote origin refs/heads/release/9.9.10-rc.1 2>/dev/null)" ] \
    || fail56 "release/9.9.10-rc.1 not pushed to origin"
if [ "$t56" -eq 1 ]; then
    ok "T56 release-bump: prerelease (RC) keeps current.md, creates the notes copy"
else
    bad "T56 release-bump: prerelease (RC) keeps current.md, creates the notes copy ($why)"
fi
# Restore the shared fixture origin (tests stay independent).
git -C "$F/origin.git" update-ref refs/heads/master "$(git -C "$root" rev-parse HEAD)"

# T57: release-bump still rejects non-semver VERSIONs: '1.2' (too short)
# and '1.2.3_x' (underscore is not a semver identifier char).
fresh_work
run_bump "1.2"
t57=1; why=""
fail57() { t57=0; why="${why}${why:+; }$1"; }
if [ "$B_RC" -ne 0 ] && printf '%s' "$B_OUT" | grep -qi 'invalid'; then
    :
else
    fail57 "VERSION=1.2 not rejected (rc=$B_RC: $(printf '%s' "$B_OUT" | head -1))"
fi
fresh_work
run_bump "1.2.3_x"
if [ "$B_RC" -ne 0 ] && printf '%s' "$B_OUT" | grep -qi 'invalid'; then
    :
else
    fail57 "VERSION=1.2.3_x not rejected (rc=$B_RC: $(printf '%s' "$B_OUT" | head -1))"
fi
if [ "$t57" -eq 1 ]; then
    ok "T57 release-bump: '1.2' and '1.2.3_x' still rejected"
else
    bad "T57 release-bump: '1.2' and '1.2.3_x' still rejected ($why)"
fi

# T58: a published release with all 10 asset NAMES but a WRONG size for one
# asset (the multipart-wrapped rc.2 bug shape: same name, bigger bytes) →
# non-zero, NO uploads, NO create, NO PATCH, message names the asset and
# the size problem.
make_assets "$D/out"
printf '%s\n' "$ASSET10" \
    | jq -Rc 'if . == "latest.json" then {name:., size:999} else {name:., size:1} end' \
    > "$D/log-gh-size.doneassets"
GH_REL_MODE=done run_gh "$root" "$D/log-gh-size" "$V"
L=$D/log-gh-size
t58=1; why=""
[ "$G_RC" -ne 0 ] || { t58=0; why="rc=0 despite a size mismatch"; }
upln=$(grep -cF "POST $GHUP/" "$L" 2>/dev/null || true)
[ "$upln" = "0" ] || { t58=0; why="${why}${why:+; }$upln upload(s), want 0"; }
cren=$(grep -cF "POST $GHAPI/releases " "$L" 2>/dev/null || true)
[ "$cren" = "0" ] || { t58=0; why="${why}${why:+; }$cren release create(s), want 0"; }
grep -q '^PATCH' "$L" 2>/dev/null && { t58=0; why="${why}${why:+; }PATCH present, want 0"; }
printf '%s' "$G_OUT" | grep -qF 'latest.json' \
    || { t58=0; why="${why}${why:+; }message does not name latest.json"; }
printf '%s' "$G_OUT" | grep -qi 'size' \
    || { t58=0; why="${why}${why:+; }message does not mention size"; }
if [ "$t58" -eq 1 ]; then
    ok "T58 release-publish-github: published release with a size mismatch → fails, names the asset, no uploads"
else
    bad "T58 release-publish-github: published release with a size mismatch → fails, names the asset, no uploads ($why)"
fi

# ── Summary ────────────────────────────────────────────────────────────────

echo ""
echo "────────────────────────────────────────────────────────────"
echo "test-release: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
    printf 'Failing checks:\n%b' "$FAILED"
    echo "❌ RED — the CI release-pipeline implementation is missing (expected pre-implementation)"
    exit 1
fi
echo "✅ GREEN — the CI release-pipeline contract is satisfied"
exit 0
