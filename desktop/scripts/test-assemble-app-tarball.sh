#!/bin/sh
#
# test-assemble-app-tarball.sh — host-level TDD tests for the macOS
# .app.tar.gz export in desktop/macos/assemble-app.sh (auto-updater
# pipeline: tauri-plugin-updater extracts the tarball with the first
# path component stripped, so the entry layout is part of the contract).
#
# Contract (written BEFORE the implementation — expect RED):
#   assemble-app.sh <version> <desktop-bin> <sidecar-bin> <outdir>
#   writes BOTH
#     PeakBot-Desktop-<v>-macos-universal.zip            (existing)
#     PeakBot-Desktop-<v>-macos-universal.app.tar.gz     (NEW)
#   The tarball's `tar -tzf` listing must START with "PeakBot Desktop.app/"
#   and NO entry may start with "./" (a ./ prefix would break the
#   updater's tar extraction — plan spike S2).
#
# Hermetic: rcodesign is stubbed (a no-op is put on PATH by this test),
# the two Mach-O inputs are dummies; the real Info.plist.in + icon.icns
# from the repo are used. No docker, no network, no real binaries.
#
# Run:  sh desktop/scripts/test-assemble-app-tarball.sh
# Exit: 0 when every check passes (GREEN); 1 with a failing-check list
#       otherwise (RED — each failure names the missing implementation piece).

set -u

script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)
cd "$root"

AA=desktop/macos/assemble-app.sh

PASS=0
FAIL=0
FAILED=""

ok()  { PASS=$((PASS+1)); echo "✅ $1"; }
bad() { FAIL=$((FAIL+1)); FAILED="${FAILED}  - $1\n"; echo "❌ $1"; }

workdirs=""
cleanup() { [ -n "$workdirs" ] && rm -rf $workdirs; }
trap cleanup EXIT

W=$(mktemp -d); workdirs="$workdirs $W"
mkdir -p "$W/bin" "$W/out"

# Version under test: arbitrary — assemble-app.sh takes it as an argument
# and only substitutes it into Info.plist + the artifact names.
V=9.9.9
TARBALL="PeakBot-Desktop-$V-macos-universal.app.tar.gz"
ZIP="PeakBot-Desktop-$V-macos-universal.zip"

# rcodesign stub: the real one needs Mach-O inputs + writes _CodeSignature;
# the tarball-layout contract does not depend on it, so a no-op suffices.
printf '#!/bin/sh\nexit 0\n' > "$W/bin/rcodesign"
chmod +x "$W/bin/rcodesign"

# Dummy Mach-O inputs (any bytes — the layout, not the code, is under test).
printf 'fake-desktop-bin' > "$W/desktop-bin"
printf 'fake-sidecar-bin' > "$W/sidecar-bin"

# run_aa [desktop-bin] → sets AA_RC / AA_OUT
run_aa() {
    AA_OUT=$(PATH="$W/bin:$PATH" sh "$AA" "$V" "${1:-$W/desktop-bin}" \
        "$W/sidecar-bin" "$W/out" 2>&1)
    AA_RC=$?
}

# T01: assemble-app.sh exists and is executable.
if [ -f "$AA" ] && [ -x "$AA" ]; then
    ok "T01 assemble-app.sh exists and is executable"
else
    bad "T01 assemble-app.sh exists and is executable"
fi

# T02: a hermetic run (fake rcodesign, dummy bins) succeeds.
run_aa
if [ "$AA_RC" -eq 0 ]; then
    ok "T02 assemble-app.sh succeeds with a stubbed rcodesign"
else
    bad "T02 assemble-app.sh succeeds with a stubbed rcodesign (rc=$AA_RC: $(printf '%s' "$AA_OUT" | head -1))"
fi

T="$W/out/$TARBALL"

# T03: the tarball exists under the contract name.
if [ -f "$T" ]; then
    ok "T03 $TARBALL is written to <outdir>"
else
    bad "T03 $TARBALL is written to <outdir>"
fi

# T04: the tarball is a valid gzip tar (tar -tzf succeeds).
if [ -f "$T" ] && tar -tzf "$T" >/dev/null 2>&1; then
    ok "T04 the tarball is a valid gzip tar"
else
    bad "T04 the tarball is a valid gzip tar"
fi

# T05: the FIRST entry is "PeakBot Desktop.app/" (the updater strips the
# first path component — it must be the bundle dir, not a "./" prefix).
if [ -f "$T" ] && [ "$(tar -tzf "$T" 2>/dev/null | head -1)" = "PeakBot Desktop.app/" ]; then
    ok "T05 tar first entry is 'PeakBot Desktop.app/'"
else
    bad "T05 tar first entry is 'PeakBot Desktop.app/' (got: $(tar -tzf "$T" 2>/dev/null | head -1))"
fi

# T06: NO entry starts with "./".
if [ -f "$T" ] && [ "$(tar -tzf "$T" 2>/dev/null | grep -c '^\./' || true)" = "0" ]; then
    ok "T06 no tar entry starts with './'"
else
    bad "T06 no tar entry starts with './' (got: $(tar -tzf "$T" 2>/dev/null | grep '^\./' | head -3 | tr '\n' ' '))"
fi

# T07: both artifacts are produced (the zip must not regress).
if [ -f "$T" ] && [ -f "$W/out/$ZIP" ]; then
    ok "T07 both the .zip AND the .app.tar.gz are produced"
else
    bad "T07 both the .zip AND the .app.tar.gz are produced"
fi

# T08: the bundle's MacOS binaries are inside the tarball.
t08=1; why=""
[ -f "$T" ] || { t08=0; why="no tarball"; }
if [ -f "$T" ]; then
    tar -tzf "$T" 2>/dev/null | grep -qx 'PeakBot Desktop.app/Contents/MacOS/peakbot-desktop' \
        || { t08=0; why="missing Contents/MacOS/peakbot-desktop entry"; }
    tar -tzf "$T" 2>/dev/null | grep -qx 'PeakBot Desktop.app/Contents/MacOS/peakbot' \
        || { t08=0; why="${why}${why:+; }missing Contents/MacOS/peakbot entry"; }
fi
if [ "$t08" -eq 1 ]; then
    ok "T08 tarball contains Contents/MacOS/peakbot-desktop and peakbot"
else
    bad "T08 tarball contains Contents/MacOS/peakbot-desktop and peakbot ($why)"
fi

# T09: missing desktop binary → non-zero (existing behaviour, regression
# guard — the tarball work must not loosen the input checks).
run_aa "$W/no-such-desktop-bin"
if [ "$AA_RC" -ne 0 ]; then
    ok "T09 missing desktop binary → non-zero"
else
    bad "T09 missing desktop binary → non-zero (rc=0)"
fi

# ── Summary ────────────────────────────────────────────────────────────────

echo ""
echo "────────────────────────────────────────────────────────────"
echo "test-assemble-app-tarball: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
    printf 'Failing checks:\n%b' "$FAILED"
    echo "❌ RED — the .app.tar.gz export is missing from assemble-app.sh (expected pre-implementation)"
    exit 1
fi
echo "✅ GREEN — the .app.tar.gz contract is satisfied"
exit 0
