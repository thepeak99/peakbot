#!/bin/sh
#
# test-updater-manifest.sh — host-level TDD tests for
# desktop/scripts/updater-manifest.sh (the auto-updater manifest builder).
#
# Contract (written BEFORE the implementation — expect RED):
#   updater-manifest.sh <version> <dir> <base_url> <notes_file>
#   <dir> must contain the three signed artifacts:
#     PeakBot-Desktop-<v>-linux-x86_64.AppImage(.sig)
#     PeakBot-Desktop-<v>-windows-x86_64-setup.exe(.sig)
#     PeakBot-Desktop-<v>-macos-universal.app.tar.gz(.sig)
#   It WRITES <dir>/latest.json (decided: a file in <dir>, not stdout-only)
#   containing {version, notes, pub_date, platforms} where
#     .version  == <version>
#     .notes    == the notes file content
#     .pub_date == RFC3339 UTC (YYYY-MM-DDTHH:MM:SSZ, wall-clock)
#     .platforms keys are EXACTLY
#       linux-x86_64, windows-x86_64, darwin-x86_64, darwin-aarch64
#     each platform entry has .url == "<base_url>/<asset filename>" and
#     .signature == the matching .sig content trimmed of its trailing
#     newline. darwin-x86_64 and darwin-aarch64 are IDENTICAL (one
#     universal tarball + one signature).
#   Failure modes: missing artifact, missing .sig, or empty .sig →
#   non-zero exit AND no latest.json left behind; wrong arg count →
#   non-zero exit + usage text. Prerelease versions (0.25.0-rc.1) work
#   exactly like stable ones.
#
# Hermetic: temp dirs, no network, no docker. pub_date is wall-clock, so
# it is asserted by FORMAT only.
#
# Run:  sh desktop/scripts/test-updater-manifest.sh
# Exit: 0 when every check passes (GREEN); 1 with a failing-check list
#       otherwise (RED — each failure names the missing implementation piece).

set -u

script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)
cd "$root"

UM=desktop/scripts/updater-manifest.sh
CI=.gitea/workflows/ci.yml

PASS=0
FAIL=0
FAILED=""

ok()  { PASS=$((PASS+1)); echo "✅ $1"; }
bad() { FAIL=$((FAIL+1)); FAILED="${FAILED}  - $1\n"; echo "❌ $1"; }

workdirs=""
cleanup() { [ -n "$workdirs" ] && rm -rf $workdirs; }
trap cleanup EXIT

W=$(mktemp -d); workdirs="$workdirs $W"

# A version that is NOT the repo's current one: the contract must be
# version-independent (the script takes <version> as an argument).
V=0.25.0
BASE="https://github.com/thepeak99/peakbot/releases/download/$V"

# setup <dir> <version> — the three artifacts + three .sig files + notes.
setup() {
    d=$1; v=$2
    mkdir -p "$d"
    printf 'appimage-bytes'  > "$d/PeakBot-Desktop-$v-linux-x86_64.AppImage"
    printf 'setup-exe-bytes' > "$d/PeakBot-Desktop-$v-windows-x86_64-setup.exe"
    printf 'tarball-bytes'   > "$d/PeakBot-Desktop-$v-macos-universal.app.tar.gz"
    printf 'linux-sig-payload\n'   > "$d/PeakBot-Desktop-$v-linux-x86_64.AppImage.sig"
    printf 'windows-sig-payload\n' > "$d/PeakBot-Desktop-$v-windows-x86_64-setup.exe.sig"
    printf 'macos-sig-payload\n'   > "$d/PeakBot-Desktop-$v-macos-universal.app.tar.gz.sig"
    printf 'Release %s notes\nSecond line\n' "$v" > "$d/notes.md"
}

# run_um <dir> <version> <base_url> <notes_file> → sets U_RC / U_OUT
run_um() {
    U_OUT=$(sh "$UM" "$2" "$1" "$3" "$4" 2>&1)
    U_RC=$?
}

# ── A: script contract ─────────────────────────────────────────────────────

# T01: the script exists and is executable.
if [ -f "$UM" ] && [ -x "$UM" ]; then
    ok "T01 updater-manifest.sh exists and is executable"
else
    bad "T01 updater-manifest.sh exists and is executable"
fi

# T02: it parses as POSIX sh.
if [ -f "$UM" ] && sh -n "$UM" 2>/dev/null; then
    ok "T02 updater-manifest.sh is syntactically valid sh"
else
    bad "T02 updater-manifest.sh is syntactically valid sh"
fi

# T03: wrong arg count (0/2/3/5) → non-zero + usage text.
t03=1
for n in 0 2 3 5; do
    case $n in
        0) out=$(sh "$UM" 2>&1); rc=$? ;;
        2) out=$(sh "$UM" a b 2>&1); rc=$? ;;
        3) out=$(sh "$UM" a b c 2>&1); rc=$? ;;
        5) out=$(sh "$UM" a b c d e 2>&1); rc=$? ;;
    esac
    [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q 'usage' || t03=0
done
if [ "$t03" -eq 1 ]; then
    ok "T03 updater-manifest.sh 0/2/3/5 args → non-zero + usage"
else
    bad "T03 updater-manifest.sh 0/2/3/5 args → non-zero + usage"
fi

# ── B: happy path (stable version) ─────────────────────────────────────────

setup "$W/h" "$V"
run_um "$W/h" "$V" "$BASE" "$W/h/notes.md"
J="$W/h/latest.json"

# T04: rc 0; latest.json WRITTEN to <dir>/latest.json; valid JSON;
# .version == <v>.
t04=1; why=""
[ "$U_RC" -eq 0 ] || { t04=0; why="rc=$U_RC: $(printf '%s' "$U_OUT" | head -1)"; }
[ -f "$J" ] || { t04=0; why="${why}${why:+; }no <dir>/latest.json written"; }
if [ -f "$J" ]; then
    jq -e . >/dev/null 2>&1 < "$J" || { t04=0; why="${why}${why:+; }latest.json is not valid JSON"; }
    [ "$(jq -er '.version' "$J" 2>/dev/null)" = "$V" ] \
        || { t04=0; why="${why}${why:+; }.version != $V"; }
fi
if [ "$t04" -eq 1 ]; then
    ok "T04 happy path: rc 0, <dir>/latest.json written, valid JSON, .version == $V"
else
    bad "T04 happy path: rc 0, <dir>/latest.json written, valid JSON, .version == $V ($why)"
fi

# T05: .notes == the notes file content (exact, multi-line).
if [ -f "$J" ] && [ "$(jq -er '.notes' "$J" 2>/dev/null)" = "$(cat "$W/h/notes.md")" ]; then
    ok "T05 .notes equals the notes file content"
else
    bad "T05 .notes equals the notes file content"
fi

# T06: .pub_date is RFC3339 UTC (YYYY-MM-DDTHH:MM:SSZ).
pd=$(jq -er '.pub_date' "$J" 2>/dev/null)
if printf '%s' "$pd" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'; then
    ok "T06 .pub_date is RFC3339 UTC (got: $pd)"
else
    bad "T06 .pub_date is RFC3339 UTC (got: ${pd:-missing})"
fi

# T07: .platforms has EXACTLY the four expected keys.
pk=$(jq -er '.platforms | keys | join(",")' "$J" 2>/dev/null)
if [ "$pk" = "darwin-aarch64,darwin-x86_64,linux-x86_64,windows-x86_64" ]; then
    ok "T07 .platforms keys exactly linux/windows/darwin-x86_64/darwin-aarch64"
else
    bad "T07 .platforms keys exactly linux/windows/darwin-x86_64/darwin-aarch64 (got: ${pk:-missing})"
fi

# T08: every .url == "<base_url>/<asset filename>".
t08=1; why=""
[ -f "$J" ] || { t08=0; why="no latest.json"; }
if [ -f "$J" ]; then
    [ "$(jq -er '.platforms["linux-x86_64"].url' "$J" 2>/dev/null)" \
        = "$BASE/PeakBot-Desktop-$V-linux-x86_64.AppImage" ] \
        || { t08=0; why="linux url wrong: $(jq -er '.platforms["linux-x86_64"].url' "$J" 2>/dev/null)"; }
    [ "$(jq -er '.platforms["windows-x86_64"].url' "$J" 2>/dev/null)" \
        = "$BASE/PeakBot-Desktop-$V-windows-x86_64-setup.exe" ] \
        || { t08=0; why="${why}${why:+; }windows url wrong"; }
    [ "$(jq -er '.platforms["darwin-x86_64"].url' "$J" 2>/dev/null)" \
        = "$BASE/PeakBot-Desktop-$V-macos-universal.app.tar.gz" ] \
        || { t08=0; why="${why}${why:+; }darwin-x86_64 url wrong"; }
    [ "$(jq -er '.platforms["darwin-aarch64"].url' "$J" 2>/dev/null)" \
        = "$BASE/PeakBot-Desktop-$V-macos-universal.app.tar.gz" ] \
        || { t08=0; why="${why}${why:+; }darwin-aarch64 url wrong"; }
fi
if [ "$t08" -eq 1 ]; then
    ok "T08 every .url == <base_url>/<asset filename>"
else
    bad "T08 every .url == <base_url>/<asset filename> ($why)"
fi

# T09: every .signature == the matching .sig content, trimmed of its
# trailing newline.
t09=1; why=""
[ -f "$J" ] || { t09=0; why="no latest.json"; }
if [ -f "$J" ]; then
    [ "$(jq -er '.platforms["linux-x86_64"].signature' "$J" 2>/dev/null)" \
        = "$(cat "$W/h/PeakBot-Desktop-$V-linux-x86_64.AppImage.sig")" ] \
        || { t09=0; why="linux signature != .sig content (trimmed)"; }
    [ "$(jq -er '.platforms["windows-x86_64"].signature' "$J" 2>/dev/null)" \
        = "$(cat "$W/h/PeakBot-Desktop-$V-windows-x86_64-setup.exe.sig")" ] \
        || { t09=0; why="${why}${why:+; }windows signature != .sig content (trimmed)"; }
    [ "$(jq -er '.platforms["darwin-x86_64"].signature' "$J" 2>/dev/null)" \
        = "$(cat "$W/h/PeakBot-Desktop-$V-macos-universal.app.tar.gz.sig")" ] \
        || { t09=0; why="${why}${why:+; }darwin signature != .sig content (trimmed)"; }
fi
if [ "$t09" -eq 1 ]; then
    ok "T09 every .signature == matching .sig content (trailing newline trimmed)"
else
    bad "T09 every .signature == matching .sig content (trailing newline trimmed) ($why)"
fi

# T10: darwin-x86_64 and darwin-aarch64 entries are IDENTICAL.
if [ -f "$J" ] \
    && jq -e '.platforms["darwin-x86_64"] == .platforms["darwin-aarch64"]' "$J" >/dev/null 2>&1; then
    ok "T10 darwin-x86_64 and darwin-aarch64 entries identical"
else
    bad "T10 darwin-x86_64 and darwin-aarch64 entries identical"
fi

# ── C: failure modes (non-zero, no latest.json left behind) ────────────────

# T11: missing .sig → non-zero, no latest.json.
setup "$W/nosig" "$V"
rm -f "$W/nosig/PeakBot-Desktop-$V-linux-x86_64.AppImage.sig"
run_um "$W/nosig" "$V" "$BASE" "$W/nosig/notes.md"
if [ "$U_RC" -ne 0 ] && [ ! -e "$W/nosig/latest.json" ]; then
    ok "T11 missing .sig → non-zero, no latest.json"
else
    bad "T11 missing .sig → non-zero, no latest.json (rc=$U_RC)"
fi

# T12: empty .sig → non-zero, no latest.json.
setup "$W/emptysig" "$V"
: > "$W/emptysig/PeakBot-Desktop-$V-macos-universal.app.tar.gz.sig"
run_um "$W/emptysig" "$V" "$BASE" "$W/emptysig/notes.md"
if [ "$U_RC" -ne 0 ] && [ ! -e "$W/emptysig/latest.json" ]; then
    ok "T12 empty .sig → non-zero, no latest.json"
else
    bad "T12 empty .sig → non-zero, no latest.json (rc=$U_RC)"
fi

# T13: missing artifact → non-zero, no latest.json.
setup "$W/noart" "$V"
rm -f "$W/noart/PeakBot-Desktop-$V-macos-universal.app.tar.gz"
run_um "$W/noart" "$V" "$BASE" "$W/noart/notes.md"
if [ "$U_RC" -ne 0 ] && [ ! -e "$W/noart/latest.json" ]; then
    ok "T13 missing artifact → non-zero, no latest.json"
else
    bad "T13 missing artifact → non-zero, no latest.json (rc=$U_RC)"
fi

# ── D: edges ───────────────────────────────────────────────────────────────

# T14: a prerelease version (0.25.0-rc.1) works the same as stable.
PV=0.25.0-rc.1
PBASE="https://github.com/thepeak99/peakbot/releases/download/$PV"
setup "$W/pre" "$PV"
run_um "$W/pre" "$PV" "$PBASE" "$W/pre/notes.md"
PJ="$W/pre/latest.json"
t14=1; why=""
[ "$U_RC" -eq 0 ] || { t14=0; why="rc=$U_RC: $(printf '%s' "$U_OUT" | head -1)"; }
[ -f "$PJ" ] || { t14=0; why="${why}${why:+; }no latest.json"; }
if [ -f "$PJ" ]; then
    [ "$(jq -er '.version' "$PJ" 2>/dev/null)" = "$PV" ] \
        || { t14=0; why="${why}${why:+; }.version != $PV"; }
    pk=$(jq -er '.platforms | keys | join(",")' "$PJ" 2>/dev/null)
    [ "$pk" = "darwin-aarch64,darwin-x86_64,linux-x86_64,windows-x86_64" ] \
        || { t14=0; why="${why}${why:+; }platforms keys wrong: $pk"; }
    [ "$(jq -er '.platforms["linux-x86_64"].url' "$PJ" 2>/dev/null)" \
        = "$PBASE/PeakBot-Desktop-$PV-linux-x86_64.AppImage" ] \
        || { t14=0; why="${why}${why:+; }linux url not under the rc base_url"; }
    [ "$(jq -er '.platforms["darwin-x86_64"].signature' "$PJ" 2>/dev/null)" \
        = "$(cat "$W/pre/PeakBot-Desktop-$PV-macos-universal.app.tar.gz.sig")" ] \
        || { t14=0; why="${why}${why:+; }darwin signature != .sig content"; }
fi
if [ "$t14" -eq 1 ]; then
    ok "T14 prerelease 0.25.0-rc.1 works the same (version/keys/url/signature)"
else
    bad "T14 prerelease 0.25.0-rc.1 works the same ($why)"
fi

# T15: notes with unicode, quotes and multiple lines round-trip exactly.
setup "$W/uni" "$V"
printf 'Ünïcödé — 日本語 ✅\n"quoted" and a 2nd line\n' > "$W/uni/notes.md"
run_um "$W/uni" "$V" "$BASE" "$W/uni/notes.md"
if [ "$U_RC" -eq 0 ] \
    && [ "$(jq -er '.notes' "$W/uni/latest.json" 2>/dev/null)" = "$(cat "$W/uni/notes.md")" ]; then
    ok "T15 unicode/quoted/multi-line notes round-trip exactly"
else
    bad "T15 unicode/quoted/multi-line notes round-trip exactly (rc=$U_RC)"
fi

# T16: re-running overwrites a stale/corrupt latest.json.
setup "$W/over" "$V"
run_um "$W/over" "$V" "$BASE" "$W/over/notes.md"
printf 'corrupted' > "$W/over/latest.json"
run_um "$W/over" "$V" "$BASE" "$W/over/notes.md"
if [ "$U_RC" -eq 0 ] \
    && [ "$(jq -er '.version' "$W/over/latest.json" 2>/dev/null)" = "$V" ]; then
    ok "T16 re-run overwrites a stale latest.json"
else
    bad "T16 re-run overwrites a stale latest.json (rc=$U_RC)"
fi

# ── E: CI wiring ───────────────────────────────────────────────────────────

# T17: ci.yml invokes this test in the desktop job (test wiring).
if grep -qF 'sh desktop/scripts/test-updater-manifest.sh' "$CI" 2>/dev/null; then
    ok "T17 ci.yml runs test-updater-manifest.sh"
else
    bad "T17 ci.yml runs test-updater-manifest.sh"
fi

# ── Summary ────────────────────────────────────────────────────────────────

echo ""
echo "────────────────────────────────────────────────────────────"
echo "test-updater-manifest: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
    printf 'Failing checks:\n%b' "$FAILED"
    echo "❌ RED — the updater-manifest.sh implementation is missing (expected pre-implementation)"
    exit 1
fi
echo "✅ GREEN — the updater-manifest.sh contract is satisfied"
exit 0
