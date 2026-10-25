#!/bin/sh
#
# test-release.sh — host-level TDD tests for the CI release-pipeline rework.
#
# Written BEFORE the implementation: expect RED until the developer lands
#   - the Makefile contract: RELEASE_VERSION, release-check, explicit
#     sequential release recipe, 7-asset draft-first release-publish with
#     DELETE-on-failure, no-prompt release-bump with notes promotion,
#     dumb release-tag (no PR automation), and the removal of
#     .release-version / NOTES / ALLOW_DIRTY everywhere
#   - .gitea/workflows/release.yml (tag-push → make release TAG=...),
#     with desktop.yml + desktop/scripts/attach-release.sh deleted
#
# Runs NO docker builds, NO network: make release-publish is exercised with
# a fake `curl` on PATH (logs every invocation, returns canned JSON), and
# release-bump/release-tag run in throwaway local clones against a local
# bare `origin` (cargo stubbed to a no-op).
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

# T08: no prerelease flag logic.
grep_bad "T08 Makefile has no prerelease logic" 'prerelease' "$MK"

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
grep_ok "T13 release.yml: timeout-minutes: 180" 'timeout-minutes: 180' "$RY"
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

D=$(mktemp -d); workdirs="$workdirs $D"
mkdir -p "$D/bin" "$D/out/desktop"

# Fake curl: logs "METHOD URL [BODY=…] [FILE=…]" per invocation to $CURL_LOG,
# then answers by shape. Env knobs: CREATE_FAIL (1), FAIL_UPLOAD=<asset name>.
cat > "$D/bin/curl" <<'FAKECURL'
#!/bin/sh
: "${CURL_LOG:?fake curl needs CURL_LOG}"
method=GET; url=''; body=''; fdata=''
prev=''
for a in "$@"; do
  case "$prev" in
    -X|--request) method=$a; prev=''; continue ;;
    -d|--data)    body=$a; prev=''; continue ;;
    -F|--form)    fdata=$a; prev=''; continue ;;
  esac
  case "$a" in
    -*) prev=$a ;;
    *) prev=''; if [ -z "$url" ]; then url=$a; fi ;;
  esac
done
line="$method $url"
[ -n "$body" ]  && line="$line BODY=$body"
[ -n "$fdata" ] && line="$line FILE=$fdata"
printf '%s\n' "$line" >> "$CURL_LOG"
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
exit 0
FAKECURL
chmod +x "$D/bin/curl"

make_assets() { # $1 = output dir: create all 7 release assets
    mkdir -p "$1/desktop"
    printf x > "$1/peakbot-$V-linux-amd64"
    printf x > "$1/peakbot-$V-windows-amd64.exe"
    printf x > "$1/peakbot-$V-macos-universal2"
    printf x > "$1/peakbot-$V-android-arm64"
    printf x > "$1/desktop/PeakBot-Desktop-$V-linux-x86_64.AppImage"
    printf x > "$1/desktop/PeakBot-Desktop-$V-macos-universal.zip"
    printf x > "$1/desktop/PeakBot-Desktop-$V-windows-x86_64.exe"
}

# run_publish <logfile> [FAIL_UPLOAD] → sets P_RC / P_OUT
run_publish() {
    rm -f "$1"; : > "$1"
    P_OUT=$( cd "$root" && \
      if [ "$#" -ge 2 ] && [ -n "$2" ]; then FAIL_UPLOAD="$2"; fi; \
      export FAIL_UPLOAD CURL_LOG; \
      PATH="$D/bin:$PATH" CURL_LOG="$1" \
      make release-publish TAG="$V" OUTPUT_DIR="$D/out" \
      GITEA_URL="$GU" OWNER="$GO" REPO="$GR" GITEA_TOKEN=fake-token 2>&1 )
    P_RC=$?
}

# T26: one asset missing → non-zero, message names it, ZERO HTTP requests.
make_assets "$D/out"
rm -f "$D/out/peakbot-$V-android-arm64"
run_publish "$D/log-missing"
if [ "$P_RC" -ne 0 ] && [ ! -s "$D/log-missing" ] \
    && printf '%s' "$P_OUT" | grep -qF "peakbot-$V-android-arm64"; then
    ok "T26 release-publish: missing asset → fails, names it, zero HTTP"
else
    logstate=$([ -f "$D/log-missing" ] && wc -l < "$D/log-missing" || echo absent)
    bad "T26 release-publish: missing asset → fails, names it, zero HTTP (rc=$P_RC, log lines=$logstate)"
fi

# T27: happy path — draft create, 7 exact uploads, final PATCH, no DELETE,
# and the explicit GITEA_URL/OWNER/REPO vars are used (override origin).
make_assets "$D/out"
run_publish "$D/log-happy"
t27=1; why=""
fail27() { t27=0; why="${why}${why:+; }$1"; }
[ "$P_RC" -eq 0 ] || fail27 "rc=$P_RC: $(printf '%s' "$P_OUT" | grep '❌' | head -1)"
lines=$(wc -l < "$D/log-happy" | tr -d ' ')
[ "$lines" = "9" ] || fail27 "log has $lines lines, want 9 (create+7 uploads+patch)"
grep -qF "POST $API/releases " "$D/log-happy" 2>/dev/null || fail27 "no create POST to $API/releases"
grep -qF '"draft":true' "$D/log-happy" 2>/dev/null || fail27 "create body lacks draft:true"
grep -qF "\"tag_name\":\"$V\"" "$D/log-happy" 2>/dev/null || fail27 "create body lacks tag_name=$V"
grep -qF "\"name\":\"$V\"" "$D/log-happy" 2>/dev/null || fail27 "create body name is not exactly $V"
grep -qF 'Windows web cwd picker fix' "$D/log-happy" 2>/dev/null || fail27 "create body lacks release-notes/$V.md content"
grep -qF 'prerelease' "$D/log-happy" 2>/dev/null && fail27 "prerelease flag present"
upl=$(grep -cF 'assets?name=' "$D/log-happy" 2>/dev/null || true)
[ "$upl" = "7" ] || fail27 "$upl asset uploads, want 7"
for n in peakbot-$V-linux-amd64 peakbot-$V-windows-amd64.exe peakbot-$V-macos-universal2 \
         peakbot-$V-android-arm64 PeakBot-Desktop-$V-linux-x86_64.AppImage \
         PeakBot-Desktop-$V-macos-universal.zip PeakBot-Desktop-$V-windows-x86_64.exe; do
    c=$(grep -cF "name=$n " "$D/log-happy" 2>/dev/null || true)
    [ "$c" = "1" ] || fail27 "upload of $n: $c occurrence(s), want 1"
done
grep -qF "PATCH $API/releases/42" "$D/log-happy" 2>/dev/null \
    && grep -F 'PATCH ' "$D/log-happy" | grep -qF '"draft":false' \
    || fail27 "no final PATCH /releases/42 with draft:false"
grep -q '^DELETE' "$D/log-happy" 2>/dev/null && fail27 "DELETE present on happy path"
if [ "$t27" -eq 1 ]; then
    ok "T27 release-publish: happy path (draft create, 7 exact uploads, PATCH, no DELETE)"
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
