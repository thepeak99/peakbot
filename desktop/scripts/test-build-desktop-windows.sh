#!/bin/sh
#
# test-build-desktop-windows.sh — host-level TDD tests for the Windows
# desktop pipeline (WINDOWS-PLAN §5.2). Single self-extracting portable exe.
#
# Written BEFORE the implementation: expect RED until the developer lands
#   - build-desktop.sh accepting `windows` (and all = linux+macos+windows)
#   - Dockerfile.desktop-windows (cargo xwin, NO Tauri CLI / NSIS)
#   - the msvc + static-CRT path in Dockerfile.windows
#   - the Makefile desktop-windows target (portable exe)
#   - the source guards: main.rs windows_subsystem (T29), sys.rs
#     CREATE_NO_WINDOW + one command() (T30), build.rs PEAKBOT_SIDECAR_ID (T31)
#
# Runs NO docker builds: everything is a static check, plus smoke-windows.sh
# unit-tested against fake artifacts (a crafted minimal PE32+ header so
# file(1) classifies it as a Windows x86-64 executable).
#
# GUARD DEVIATION (spike): Debian's `llvm-rc` has NO `--version` (it exits 1),
# so the Dockerfile.desktop-windows guard is `command -v llvm-rc &&
# command -v llvm-objdump` — T21 therefore requires `command -v llvm-rc` and
# forbids `llvm-rc --version` (plus the whole abandoned NSIS/CLI stack).
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
MAIN=desktop/src-tauri/src/main.rs
SYS=desktop/core/src/sys.rs
BUILD_RS=desktop/src-tauri/build.rs

PASS=0
FAIL=0
FAILED=""

ok()  { PASS=$((PASS+1)); echo "✅ $1"; }
bad() { FAIL=$((FAIL+1)); FAILED="${FAILED}  - $1\n"; echo "❌ $1"; }

# grep_ok <name> <fixed-string> <file>
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

# T09: the artifact name drops -setup (portable exe, not an installer).
if grep -qF 'PeakBot-Desktop-$VERSION-windows-x86_64.exe' "$BD" 2>/dev/null \
    && ! grep -qF -- '-setup' "$BD" 2>/dev/null; then
    ok "T09 build-desktop.sh expects PeakBot-Desktop-\$VERSION-windows-x86_64.exe (no -setup)"
else
    bad "T09 build-desktop.sh expects PeakBot-Desktop-\$VERSION-windows-x86_64.exe (no -setup)"
fi

# ── B: Dockerfile.desktop-windows ──────────────────────────────────────────

if [ -f "$DFW" ]; then ok "T10 Dockerfile.desktop-windows exists"; else bad "T10 Dockerfile.desktop-windows exists"; fi

grep_ok "T11 Dockerfile.desktop-windows builds with cargo xwin build" \
    'cargo xwin build' "$DFW"
grep_ok "T12 Dockerfile.desktop-windows targets x86_64-pc-windows-msvc" \
    '--target x86_64-pc-windows-msvc' "$DFW"
grep_ok "T13 Dockerfile.desktop-windows enables tauri/custom-protocol" \
    '--features tauri/custom-protocol' "$DFW"
grep_ok "T14 Dockerfile.desktop-windows builds --locked" '--locked' "$DFW"
grep_ok "T15 Dockerfile.desktop-windows pins cargo-xwin@0.23.1" \
    'cargo-xwin@0.23.1' "$DFW"
grep_ok "T16 Dockerfile.desktop-windows runs check-version.sh" \
    'check-version.sh' "$DFW"

# T17: smoke-windows.sh is invoked WITH the staged sidecar path (the S-W4 id
# check needs the sidecar whose bytes are the embedded payload).
if grep -F 'smoke-windows.sh' "$DFW" 2>/dev/null | grep -qF 'peakbot-x86_64-pc-windows-msvc.exe'; then
    ok "T17 Dockerfile.desktop-windows runs smoke-windows.sh with the staged sidecar"
else
    bad "T17 Dockerfile.desktop-windows runs smoke-windows.sh with the staged sidecar"
fi

grep_ok "T18 Dockerfile.desktop-windows stages the sidecar as peakbot-x86_64-pc-windows-msvc.exe" \
    'peakbot-x86_64-pc-windows-msvc.exe' "$DFW"
grep_ok "T19 Dockerfile.desktop-windows exports the portable exe name" \
    'PeakBot-Desktop-${VERSION}-windows-x86_64.exe' "$DFW"
grep_ok "T20 Dockerfile.desktop-windows exports via FROM scratch" 'FROM scratch' "$DFW"

# T21: the guard + the anti-NSIS/CLI tripwire (one check). The guard uses
# `command -v llvm-rc` (spike: llvm-rc has NO --version, so `llvm-rc
# --version` must NOT appear). None of the abandoned NSIS/CLI stack may be
# present, and the artifact name must not carry -setup.
t21_violations=""
for tok in '@tauri-apps/cli' 'nsis' 'appindicator' 'nodesource' '-setup' 'llvm-rc --version'; do
    if grep -qF -- "$tok" "$DFW" 2>/dev/null; then
        t21_violations="${t21_violations} '$tok'"
    fi
done
if [ -n "$t21_violations" ]; then
    bad "T21 Dockerfile.desktop-windows must not contain:$t21_violations"
elif grep -qF 'command -v llvm-rc' "$DFW" 2>/dev/null; then
    ok "T21 Dockerfile.desktop-windows guard: command -v llvm-rc (no --version), no NSIS/CLI stack"
else
    bad "T21 Dockerfile.desktop-windows guard must use 'command -v llvm-rc'"
fi

# ── C: Dockerfile.windows (sidecar) ────────────────────────────────────────

grep_ok "T22 Dockerfile.windows has the msvc target x86_64-pc-windows-msvc" \
    'x86_64-pc-windows-msvc' "$DFW_SIDE"
grep_ok "T23 Dockerfile.windows pins cargo-xwin@0.23.1" \
    'cargo-xwin@0.23.1' "$DFW_SIDE"
grep_ok "T24 Dockerfile.windows exports peakbot-windows-amd64-msvc.exe" \
    'peakbot-windows-amd64-msvc.exe' "$DFW_SIDE"

# T25: the static-CRT env (target-scoped, so the gnu path ignores it).
if grep -qF 'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS' "$DFW_SIDE" 2>/dev/null \
    && grep -qF '+crt-static' "$DFW_SIDE" 2>/dev/null; then
    ok "T25 Dockerfile.windows sets CARGO_TARGET_…_MSVC_RUSTFLAGS with +crt-static"
else
    bad "T25 Dockerfile.windows sets CARGO_TARGET_…_MSVC_RUSTFLAGS with +crt-static"
fi

# ── D: Makefile ────────────────────────────────────────────────────────────

if grep -qE '^desktop-windows:' "$MK" 2>/dev/null; then
    ok "T26 Makefile has a desktop-windows target"
else
    bad "T26 Makefile has a desktop-windows target"
fi
if sed -n '/^\.PHONY:/,/^$/p' "$MK" 2>/dev/null | grep -q 'desktop-windows'; then
    ok "T27 Makefile .PHONY lists desktop-windows"
else
    bad "T27 Makefile .PHONY lists desktop-windows"
fi
if grep -qE '^##[[:space:]]*desktop-windows:' "$MK" 2>/dev/null \
    && grep -qF 'Build the Windows x86_64 portable exe' "$MK" 2>/dev/null; then
    ok "T28 Makefile help line says portable exe (not NSIS installer)"
else
    bad "T28 Makefile help line says portable exe (not NSIS installer)"
fi

# ── E: source guards (defects 1–3 have no Windows CI; these are the
#        regression tripwires) ──────────────────────────────────────────────

grep_ok "T29 main.rs has windows_subsystem = \"windows\"" \
    'windows_subsystem = "windows"' "$MAIN"

# T30: sys.rs — one private command() constructor that, on Windows, sets
# CREATE_NO_WINDOW via creation_flags; EVERY Command::new goes through it, so
# exactly one Command::new remains in non-test code.
t30_ok=1
grep -qF 'CREATE_NO_WINDOW' "$SYS" 2>/dev/null || t30_ok=0
grep -qF 'creation_flags' "$SYS" 2>/dev/null || t30_ok=0
non_test_cmd_new=$(awk '/#\[cfg\(test\)\]/{exit} {print}' "$SYS" 2>/dev/null | grep -cF 'Command::new' || true)
[ "${non_test_cmd_new:-0}" -eq 1 ] || t30_ok=0
if [ "$t30_ok" -eq 1 ]; then
    ok "T30 sys.rs: CREATE_NO_WINDOW + creation_flags, exactly one Command::new in non-test code"
else
    bad "T30 sys.rs: CREATE_NO_WINDOW + creation_flags, exactly one Command::new in non-test code (non-test Command::new count: ${non_test_cmd_new:-?})"
fi

grep_ok "T31 build.rs emits PEAKBOT_SIDECAR_ID" 'PEAKBOT_SIDECAR_ID' "$BUILD_RS"

# ── F: smoke-windows.sh unit tests (fake artifacts, no docker) ─────────────

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

# Same version extraction build-desktop.sh uses (root Cargo.toml).
VERSION=$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' Cargo.toml)
[ -n "$VERSION" ] || { echo "❌ could not read version from Cargo.toml" >&2; exit 1; }
GOODNAME="PeakBot-Desktop-${VERSION}-windows-x86_64.exe"

# make_fake_pe <path> <subsystem> — minimal PE32+ (x86-64) header, 416 bytes.
# <subsystem>: 2 = Windows GUI, 3 = Windows console. file(1) classifies it as
# "PE32+ executable … x86-64 … MS Windows" (+ (GUI) or (console)), which is
# exactly what S-W2 asserts.
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
        printf "\\$(printf '%03o' "$2")\000"   # Subsystem = $2 (2=GUI, 3=console)
        head -c 154 /dev/zero
        printf '.text\000\000\000'  # one section
        head -c 32 /dev/zero
    } > "$1"
}

# run_sw <exe> <version> <sidecar> → sets SW_OUT / SW_RC for the assertions.
run_sw() {
    SW_OUT=$(sh "$SW" "$1" "$2" "$3" 2>&1)
    SW_RC=$?
}
sw_fail_line() {  # the ❌ line of the last smoke run (for diagnostics)
    printf '%s\n' "$SW_OUT" | grep '❌' | head -1
}

# A dummy sidecar so the smoke run gets past the file-existence checks.
printf 'x' > "$workdir/fake-sidecar.exe"

# T32: the smoke script exists and is executable.
if [ -f "$SW" ] && [ -x "$SW" ]; then
    ok "T32 smoke-windows.sh exists and is executable"
else
    bad "T32 smoke-windows.sh exists and is executable"
fi

# T33: it parses as POSIX sh.
if sh -n "$SW" 2>/dev/null; then
    ok "T33 smoke-windows.sh is syntactically valid sh"
else
    bad "T33 smoke-windows.sh is syntactically valid sh"
fi

# T34: 0/2/4 args → usage, exit 2.
t34_ok=1
SW_OUT=$(sh "$SW" 2>&1);            [ "$?" -eq 2 ] && printf '%s' "$SW_OUT" | grep -q 'usage' || t34_ok=0
SW_OUT=$(sh "$SW" a b 2>&1);        [ "$?" -eq 2 ] && printf '%s' "$SW_OUT" | grep -q 'usage' || t34_ok=0
SW_OUT=$(sh "$SW" a b c d 2>&1);    [ "$?" -eq 2 ] && printf '%s' "$SW_OUT" | grep -q 'usage' || t34_ok=0
if [ "$t34_ok" -eq 1 ]; then
    ok "T34 smoke-windows.sh 0/2/4 args → exit 2 + usage"
else
    bad "T34 smoke-windows.sh 0/2/4 args → exit 2 + usage"
fi

# T35: missing exe → 'not found'.
run_sw "$workdir/no-such-$GOODNAME" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'not found'; then
    ok "T35 missing exe → 'not found'"
else
    bad "T35 missing exe → 'not found' (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T36: wrong name (still carries -setup) → FAIL at S-W1, before anything else.
printf 'not a PE' > "$workdir/PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe"
run_sw "$workdir/PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W1'; then
    ok "T36 wrong name (-setup) fails at S-W1 (filename contract)"
else
    bad "T36 wrong name (-setup) fails at S-W1 (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T37: correctly named but not a PE (plain text) → FAIL at S-W2.
printf 'not a PE' > "$workdir/$GOODNAME"
run_sw "$workdir/$GOODNAME" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W2'; then
    ok "T37 correctly-named text dummy fails at S-W2 (not a PE32+ executable)"
else
    bad "T37 correctly-named text dummy fails at S-W2 (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T38: correctly named fake PE32+ with subsystem 3 (console) → FAIL at S-W2
# (S-W2 requires the (GUI) subsystem for the exe).
make_fake_pe "$workdir/$GOODNAME" 3
run_sw "$workdir/$GOODNAME" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W2'; then
    ok "T38 fake PE32+ console (subsystem 3) fails at S-W2 (not GUI)"
else
    bad "T38 fake PE32+ console (subsystem 3) fails at S-W2 (got exit $SW_RC: $(sw_fail_line))"
fi

# T39: correctly named fake PE32+ GUI (subsystem 2) + a fake PE32+ sidecar →
# S-W2 passes. The later checks (S-W3/S-W4) need a real PE with an import
# table and an embedded payload, which are proven for real in the container;
# here we only assert the run REACHED and passed S-W2.
make_fake_pe "$workdir/$GOODNAME" 2
make_fake_pe "$workdir/fake-sidecar.exe" 3
run_sw "$workdir/$GOODNAME" "$VERSION" "$workdir/fake-sidecar.exe"
if printf '%s' "$SW_OUT" | grep -q '✅ S-W2'; then
    ok "T39 fake PE32+ GUI (subsystem 2) + fake sidecar → S-W2 passes"
else
    bad "T39 fake PE32+ GUI (subsystem 2) + fake sidecar → S-W2 passes (got: $(sw_fail_line))"
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
