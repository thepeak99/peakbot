#!/bin/sh
#
# test-build-desktop-windows.sh — host-level TDD tests for the Windows
# desktop pipeline (desktop/DESIGN.md §5.7 / §9.2, Windows extension).
#
# Written BEFORE the implementation: expect RED until the developer lands
#   - build-desktop.sh accepting `windows` (and all = linux+macos+windows)
#   - Dockerfile.desktop-windows (NSIS via cargo-xwin)
#   - the msvc/cargo-xwin path in Dockerfile.windows
#   - the Makefile desktop-windows target
#
# Runs NO docker builds: everything is a static check, plus smoke-windows.sh
# unit-tested against fake artifacts (a crafted minimal PE32+ header so
# file(1) classifies it as a Windows x86-64 executable).
#
# Run:  sh desktop/scripts/test-build-desktop-windows.sh
# Exit: 0 when every check passes (GREEN); 1 with a failing-check list
#       otherwise (RED — each failure names the missing implementation piece).

set -u

script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)
cd "$root"

BD=desktop/scripts/build-desktop.sh
SW=desktop/scripts/smoke-windows.sh
DFW=Dockerfile.desktop-windows
DFW_SIDE=Dockerfile.windows
MK=Makefile

PASS=0
FAIL=0
FAILED=""

ok()  { PASS=$((PASS+1)); echo "✅ $1"; }
bad() { FAIL=$((FAIL+1)); FAILED="${FAILED}  - $1\n"; echo "❌ $1"; }

# grep_ok <name> <pattern-flag-string> <file>
grep_ok() {
    if grep -qF -- "$2" "$3" 2>/dev/null; then ok "$1"; else bad "$1 (missing: $2 in $3)"; fi
}

# ── A: build-desktop.sh ────────────────────────────────────────────────────

# T01: usage text AND the arg-validation case both list the four targets
# (one occurrence would mean only one of the two was updated).
n=$(grep -cF -- 'linux|macos|windows|all' "$BD" 2>/dev/null || true)
if [ "${n:-0}" -ge 2 ]; then
    ok "T01 build-desktop.sh lists linux|macos|windows|all (usage + dispatch)"
else
    bad "T01 build-desktop.sh lists linux|macos|windows|all (usage + dispatch) — found $n occurrence(s)"
fi

# T02: a bogus arg still fails cleanly with the usage text (regression).
out=$(sh "$BD" bogus 2>&1); rc=$?
if [ "$rc" -eq 2 ] && printf '%s' "$out" | grep -q 'usage'; then
    ok "T02 build-desktop.sh bogus → exit 2 + usage"
else
    bad "T02 build-desktop.sh bogus → exit 2 + usage (got exit $rc: $(printf '%s' "$out" | head -1))"
fi

# T03: no arg fails with the usage text (not a crash).
out=$(sh "$BD" 2>&1); rc=$?
if [ "$rc" -eq 2 ] && printf '%s' "$out" | grep -q 'usage'; then
    ok "T03 build-desktop.sh (no arg) → exit 2 + usage"
else
    bad "T03 build-desktop.sh (no arg) → exit 2 + usage (got exit $rc: $(printf '%s' "$out" | head -1))"
fi

# T04: the dispatch case has a windows) arm.
if grep -qE '^[[:space:]]*windows\)' "$BD" 2>/dev/null; then
    ok "T04 build-desktop.sh dispatches a windows build"
else
    bad "T04 build-desktop.sh dispatches a windows build (no 'windows)' case arm)"
fi

# T05: `all` = linux + macos + windows — the all) arm must run windows too.
if grep -E '^[[:space:]]*all\)' "$BD" 2>/dev/null | grep -q 'windows'; then
    ok "T05 build-desktop.sh 'all' includes the windows build"
else
    bad "T05 build-desktop.sh 'all' includes the windows build (all) arm has no windows)"
fi

grep_ok "T06 build-desktop.sh references Dockerfile.desktop-windows" \
    'Dockerfile.desktop-windows' "$BD"
grep_ok "T07 build-desktop.sh passes TARGET=x86_64-pc-windows-msvc to the sidecar build" \
    'TARGET=x86_64-pc-windows-msvc' "$BD"
grep_ok "T08 build-desktop.sh expects the msvc sidecar output name" \
    'peakbot-windows-amd64-msvc.exe' "$BD"
grep_ok "T09 build-desktop.sh expects the NSIS installer artifact name" \
    'windows-x86_64-setup.exe' "$BD"

# ── B: Dockerfile.desktop-windows ──────────────────────────────────────────

if [ -f "$DFW" ]; then ok "T10 Dockerfile.desktop-windows exists"; else bad "T10 Dockerfile.desktop-windows exists"; fi

grep_ok "T11 Dockerfile.desktop-windows bundles nsis"        '--bundles nsis' "$DFW"
grep_ok "T12 Dockerfile.desktop-windows uses the cargo-xwin runner" '--runner cargo-xwin' "$DFW"
grep_ok "T13 Dockerfile.desktop-windows targets x86_64-pc-windows-msvc" '--target x86_64-pc-windows-msvc' "$DFW"
grep_ok "T14 Dockerfile.desktop-windows pins @tauri-apps/cli@2.12.3" '@tauri-apps/cli@2.12.3' "$DFW"
grep_ok "T15 Dockerfile.desktop-windows runs check-version.sh" 'check-version.sh' "$DFW"
grep_ok "T16 Dockerfile.desktop-windows runs smoke-windows.sh" 'smoke-windows.sh' "$DFW"
grep_ok "T17 Dockerfile.desktop-windows exports via FROM scratch" 'FROM scratch' "$DFW"
grep_ok "T18 Dockerfile.desktop-windows stages the sidecar as peakbot-x86_64-pc-windows-msvc.exe" 'peakbot-x86_64-pc-windows-msvc.exe' "$DFW"
grep_ok "T19 Dockerfile.desktop-windows names the exported installer" 'windows-x86_64-setup.exe' "$DFW"

# ── C: Dockerfile.windows (sidecar) ────────────────────────────────────────

grep_ok "T20 Dockerfile.windows has the msvc target x86_64-pc-windows-msvc" \
    'x86_64-pc-windows-msvc' "$DFW_SIDE"
grep_ok "T21 Dockerfile.windows cross-compiles with cargo-xwin" 'cargo-xwin' "$DFW_SIDE"
grep_ok "T22 Dockerfile.windows exports peakbot-windows-amd64-msvc.exe" \
    'peakbot-windows-amd64-msvc.exe' "$DFW_SIDE"

# ── D: Makefile ────────────────────────────────────────────────────────────

if grep -qE '^desktop-windows:' "$MK" 2>/dev/null; then
    ok "T23 Makefile has a desktop-windows target"
else
    bad "T23 Makefile has a desktop-windows target"
fi
if sed -n '/^\.PHONY:/,/^$/p' "$MK" 2>/dev/null | grep -q 'desktop-windows'; then
    ok "T24 Makefile .PHONY lists desktop-windows"
else
    bad "T24 Makefile .PHONY lists desktop-windows"
fi
if grep -qE '^##[[:space:]]*desktop-windows:' "$MK" 2>/dev/null; then
    ok "T25 Makefile help (##) line for desktop-windows"
else
    bad "T25 Makefile help (##) line for desktop-windows"
fi

# ── E: smoke-windows.sh unit tests (fake artifacts, no docker) ─────────────

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

# Same version extraction build-desktop.sh uses (root Cargo.toml).
VERSION=$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' Cargo.toml)
[ -n "$VERSION" ] || { echo "❌ could not read version from Cargo.toml" >&2; exit 1; }
GOODNAME="PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe"

# make_fake_pe <path> — minimal PE32+ (x86-64, GUI) header, 416 bytes.
# file(1) classifies it as "PE32+ executable … x86-64 … MS Windows", which is
# exactly what S-W2 asserts on a real NSIS installer.
make_fake_pe() {
    {
        printf 'MZ'
        head -c 58 /dev/zero
        printf '\200\000\000\000'   # e_lfanew = 0x80
        head -c 64 /dev/zero
        printf 'PE\000\000'         # PE signature
        printf '\144\206'           # Machine 0x8664 (x86-64)
        printf '\001\000'           # NumberOfSections = 1
        head -c 12 /dev/zero        # timestamp / symbol table / symbol count
        printf '\340\000'           # SizeOfOptionalHeader = 0xE0
        printf '\042\000'           # Characteristics: EXECUTABLE_IMAGE | LARGE_ADDRESS_AWARE
        printf '\013\002'           # OptionalHeader Magic 0x020B = PE32+
        head -c 66 /dev/zero
        printf '\002\000'           # Subsystem = 2 (Windows GUI)
        head -c 154 /dev/zero
        printf '.text\000\000\000'  # one section
        head -c 32 /dev/zero
    } > "$1"
}

# run_sw <artifact> <version> → sets SW_OUT / SW_RC for the assertions below.
run_sw() {
    SW_OUT=$(sh "$SW" "$1" "$2" 2>&1)
    SW_RC=$?
}
sw_fail_line() {  # the ❌ line of the last smoke run (for diagnostics)
    printf '%s\n' "$SW_OUT" | grep '❌' | head -1
}

# T26: the smoke script exists and is executable.
if [ -f "$SW" ] && [ -x "$SW" ]; then
    ok "T26 smoke-windows.sh exists and is executable"
else
    bad "T26 smoke-windows.sh exists and is executable"
fi

# T27: it parses as POSIX sh.
if sh -n "$SW" 2>/dev/null; then
    ok "T27 smoke-windows.sh is syntactically valid sh"
else
    bad "T27 smoke-windows.sh is syntactically valid sh"
fi

# T28: 0 args → usage, exit 2.
SW_OUT=$(sh "$SW" 2>&1); SW_RC=$?
if [ "$SW_RC" -eq 2 ] && printf '%s' "$SW_OUT" | grep -q 'usage'; then
    ok "T28 smoke-windows.sh (no args) → exit 2 + usage"
else
    bad "T28 smoke-windows.sh (no args) → exit 2 + usage (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T29: 3 args → usage, exit 2.
SW_OUT=$(sh "$SW" a b c 2>&1); SW_RC=$?
if [ "$SW_RC" -eq 2 ] && printf '%s' "$SW_OUT" | grep -q 'usage'; then
    ok "T29 smoke-windows.sh (3 args) → exit 2 + usage"
else
    bad "T29 smoke-windows.sh (3 args) → exit 2 + usage (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T30: correctly named but not a PE (plain text) → FAIL at S-W2.
printf 'not a PE' > "$workdir/$GOODNAME"
run_sw "$workdir/$GOODNAME" "$VERSION"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W2'; then
    ok "T30 correctly-named text dummy fails at S-W2 (not a PE32+ executable)"
else
    bad "T30 correctly-named text dummy fails at S-W2 (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T31: wrong name (missing -setup) → FAIL at S-W1, before anything else.
printf 'not a PE' > "$workdir/PeakBot-Desktop-${VERSION}-windows-x86_64.exe"
run_sw "$workdir/PeakBot-Desktop-${VERSION}-windows-x86_64.exe" "$VERSION"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W1'; then
    ok "T31 wrong-name installer fails at S-W1 (filename contract)"
else
    bad "T31 wrong-name installer fails at S-W1 (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T32: missing artifact → FAIL with 'not found'.
run_sw "$workdir/no-such-$GOODNAME" "$VERSION"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'not found'; then
    ok "T32 missing installer fails with 'not found'"
else
    bad "T32 missing installer fails with 'not found' (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T33: correctly named 416-byte fake PE32+ → S-W1 + S-W2 pass, FAIL at S-W3
# (size below the 2 MB floor) — proves S-W2 accepts a real PE32+ header.
make_fake_pe "$workdir/$GOODNAME"
run_sw "$workdir/$GOODNAME" "$VERSION"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W3'; then
    ok "T33 small fake PE32+ passes S-W1/S-W2, fails at S-W3 (size > 2 MB)"
else
    bad "T33 small fake PE32+ passes S-W1/S-W2, fails at S-W3 (got exit $SW_RC: $(sw_fail_line))"
fi

# T34: correctly named fake PE32+ padded to ~2.5 MB → S-W1..S-W3 pass, then
# S-W4 behaves per the documented best-effort contract:
#   - 7z present: the fake is not a real NSIS archive → FAIL at S-W4
#   - 7z absent:  clear SKIP message, exit 0
make_fake_pe "$workdir/$GOODNAME"
dd if=/dev/zero bs=1024 count=2560 >> "$workdir/$GOODNAME" 2>/dev/null
run_sw "$workdir/$GOODNAME" "$VERSION"
if command -v 7z >/dev/null 2>&1; then
    if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W4'; then
        ok "T34 big fake PE32+: with 7z present, S-W4 fails (no peakbot.exe in payload)"
    else
        bad "T34 big fake PE32+: with 7z present, S-W4 fails (got exit $SW_RC: $(sw_fail_line))"
    fi
else
    if [ "$SW_RC" -eq 0 ] && printf '%s' "$SW_OUT" | grep -q 'S-W4'; then
        ok "T34 big fake PE32+: without 7z, S-W4 SKIPs and the run passes"
    else
        bad "T34 big fake PE32+: without 7z, S-W4 SKIPs and the run passes (got exit $SW_RC: $(sw_fail_line))"
    fi
fi

# ── Summary ────────────────────────────────────────────────────────────────

echo ""
echo "────────────────────────────────────────────────────────────"
echo "test-build-desktop-windows: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
    printf 'Failing checks:\n%b' "$FAILED"
    echo "❌ RED — the Windows desktop pipeline implementation is missing (expected pre-implementation)"
    exit 1
fi
echo "✅ GREEN — the Windows desktop pipeline contract is satisfied"
exit 0
