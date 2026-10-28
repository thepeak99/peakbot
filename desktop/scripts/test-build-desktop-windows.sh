#!/bin/sh
#
# test-build-desktop-windows.sh — host-level TDD tests for the Windows
# desktop pipeline (DESIGN §5, amendment windows-nsis). The Windows release
# ships two desktop artifacts: the self-extracting portable exe plus an NSIS
# installer, both built in Dockerfile.desktop-windows.
#
# Written BEFORE the implementation: expect RED until the developer lands
#   - build-desktop.sh asserting BOTH the -portable.exe and the -setup.exe
#     outputs
#   - Dockerfile.desktop-windows on rust:1.91-trixie (nsis + 7zip + Node 22),
#     portable cp BEFORE the pinned `npx @tauri-apps/cli` NSIS step
#   - the msvc + static-CRT path in Dockerfile.windows
#   - the Makefile desktop-windows target (portable exe + NSIS installer)
#   - the source guards: main.rs windows_subsystem (T29), sys.rs
#     CREATE_NO_WINDOW + one command() (T30), build.rs PEAKBOT_SIDECAR_ID (T31)
#
# Runs NO docker builds: everything is a static check, plus smoke-windows.sh
# unit-tested against fake artifacts (a crafted minimal PE32+ header so
# file(1) classifies it as a Windows x86-64 executable).
#
# GUARD DEVIATION (spike): Debian's `llvm-rc` has NO `--version` (it exits 1),
# so the Dockerfile.desktop-windows guard is `command -v llvm-rc` — T21
# therefore requires it and forbids `llvm-rc --version`.
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
TC=desktop/src-tauri/tauri.conf.json
CI=.gitea/workflows/ci.yml
DFW_LINUX=Dockerfile.desktop-linux

PASS=0
FAIL=0
FAILED=""

ok()  { PASS=$((PASS+1)); echo "✅ $1"; }
bad() { FAIL=$((FAIL+1)); FAILED="${FAILED}  - $1\n"; echo "❌ $1"; }

# grep_ok <name> <fixed-string> <file>
grep_ok() {
    if grep -qF -- "$2" "$3" 2>/dev/null; then ok "$1"; else bad "$1 (missing: $2 in $3)"; fi
}

# stmt_of <fixed-pattern> <file> — the (continuation-joined) statement that
# first contains <fixed-pattern>, printed as "<start-line>\t<statement>".
# Backslash-continued lines are joined with spaces, so a wrapped Dockerfile
# RUN reads as one line. Empty output when the pattern is absent.
stmt_of() {
    awk -v pat="$1" '
        {
            if (buf == "") start = NR
            l = $0
            if (l ~ /\\[ \t]*$/) { sub(/\\[ \t]*$/, "", l); buf = buf " " l; next }
            stmt = buf " " l
            buf = ""
            if (index(stmt, pat) > 0) { printf "%d\t%s\n", start, stmt; exit }
        }
    ' "$2" 2>/dev/null
}
stmt_text() { printf '%s' "$1" | cut -f2-; }
stmt_line() { printf '%s' "$1" | cut -f1; }

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

# T09: build-desktop.sh asserts BOTH Windows artifacts with a `[ -f` existence
# check each, and the old bare name is gone.
t09_ok=1
t09_why=""
for name in 'PeakBot-Desktop-$VERSION-windows-x86_64-portable.exe' \
            'PeakBot-Desktop-$VERSION-windows-x86_64-setup.exe'; do
    grep -qF -- "$name" "$BD" 2>/dev/null || { t09_ok=0; t09_why="${t09_why} missing $name; "; }
    grep -F -- "$name" "$BD" 2>/dev/null | grep -qF '[ -f' \
        || { t09_ok=0; t09_why="${t09_why} $name not in a [ -f check; "; }
done
grep -qF 'windows-x86_64.exe' "$BD" 2>/dev/null \
    && { t09_ok=0; t09_why="${t09_why}bare name windows-x86_64.exe present; "; }
if [ "$t09_ok" -eq 1 ]; then
    ok "T09 build-desktop.sh checks [ -f ] for -portable.exe AND -setup.exe (no bare name)"
else
    bad "T09 build-desktop.sh checks [ -f ] for -portable.exe AND -setup.exe (no bare name) ($t09_why)"
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

# T17: smoke-windows.sh is invoked with the 4-arg contract, in order:
# <portable-exe> <setup-exe> <version> <sidecar>. The order check is a
# SEQUENTIAL search — each token must appear in the remainder AFTER the
# previous one — because ${VERSION} is a substring of both /out/ paths,
# so a first-occurrence scan would always find it inside the portable
# path and fail.
t17_line=$(stmt_text "$(stmt_of 'sh scripts/smoke-windows.sh' "$DFW")")
t17_ok=1
t17_rest=$t17_line
for tok in '/out/PeakBot-Desktop-${VERSION}-windows-x86_64-portable.exe' \
           '/out/PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe' \
           '${VERSION}' \
           'src-tauri/binaries/peakbot-x86_64-pc-windows-msvc.exe'; do
    case "$t17_rest" in
        *"$tok"*) t17_rest=${t17_rest#*"$tok"} ;;
        *) t17_ok=0; break ;;
    esac
done
if [ "$t17_ok" -eq 1 ]; then
    ok "T17 Dockerfile.desktop-windows runs smoke-windows.sh portable setup \${VERSION} sidecar (in order)"
else
    bad "T17 Dockerfile.desktop-windows runs smoke-windows.sh portable setup \${VERSION} sidecar (in order) (line: ${t17_line:-not found})"
fi

grep_ok "T18 Dockerfile.desktop-windows stages the sidecar as peakbot-x86_64-pc-windows-msvc.exe" \
    'peakbot-x86_64-pc-windows-msvc.exe' "$DFW"
# T19: both artifact names are copied to /out/ (the scratch stage exports
# exactly these two files).
t19_ok=1
grep -qF '/out/PeakBot-Desktop-${VERSION}-windows-x86_64-portable.exe' "$DFW" 2>/dev/null || t19_ok=0
grep -qF '/out/PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe' "$DFW" 2>/dev/null || t19_ok=0
if [ "$t19_ok" -eq 1 ]; then
    ok "T19 Dockerfile.desktop-windows exports /out/…-portable.exe AND /out/…-setup.exe"
else
    bad "T19 Dockerfile.desktop-windows exports /out/…-portable.exe AND /out/…-setup.exe"
fi

grep_ok "T20 Dockerfile.desktop-windows exports via FROM scratch" 'FROM scratch' "$DFW"

# T21: the toolchain guard + the bare-name tripwire (one check). The guard
# requires `command -v llvm-rc`, `command -v makensis` and `command -v 7z`
# (spike: llvm-rc has NO --version, so `llvm-rc --version` must NOT appear).
# The image must be on trixie, and the old bare artifact name must be gone.
# R2 fired: Tauri CLI 2.12.3 panics on Linux hosts without an appindicator
# library, so `appindicator` is NO LONGER forbidden — but if
# libayatana-appindicator3-dev is installed it must ship with a comment
# quoting the error it prevents.
t21_violations=""
for tok in 'llvm-rc --version' 'rust:1.91-bookworm' 'windows-x86_64.exe'; do
    if grep -qF -- "$tok" "$DFW" 2>/dev/null; then
        t21_violations="${t21_violations} '$tok'"
    fi
done
if [ -n "$t21_violations" ]; then
    bad "T21 Dockerfile.desktop-windows guard + tripwire — forbidden present:$t21_violations"
else
    t21_ok=1
    for tok in 'command -v llvm-rc' 'command -v makensis' 'command -v 7z'; do
        grep -qF -- "$tok" "$DFW" 2>/dev/null || { t21_ok=0; t21_violations="${t21_violations} (missing: $tok)"; }
    done
    # R2: if the appindicator package is installed, a comment above the
    # package line must quote the panic it prevents. The quoted message
    # may wrap across comment lines, so strip the '#' markers and join
    # the context before matching.
    t21_ai_line=$(grep -nE '^[[:space:]]+libayatana-appindicator3-dev' "$DFW" 2>/dev/null | head -1 | cut -d: -f1)
    if [ -n "$t21_ai_line" ]; then
        t21_ai_from=$((t21_ai_line - 12)); [ "$t21_ai_from" -lt 1 ] && t21_ai_from=1
        t21_ai_ctx=$(sed -n "${t21_ai_from},$((t21_ai_line - 1))p" "$DFW" 2>/dev/null \
            | sed 's/^[[:space:]]*#[[:space:]]*//' | tr '\n' ' ')
        case "$t21_ai_ctx" in
            *"Can't detect any appindicator library"*) ;;
            *) t21_ok=0; t21_violations="${t21_violations} (appindicator package lacks the justification comment)" ;;
        esac
    fi
    if [ "$t21_ok" -eq 1 ]; then
        ok "T21 Dockerfile.desktop-windows guard: llvm-rc + makensis + 7z (no --version), no bookworm/bare-name"
    else
        bad "T21 Dockerfile.desktop-windows guard + tripwire$t21_violations"
    fi
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
    && grep -qF 'Build the Windows x86_64 portable exe + NSIS installer' "$MK" 2>/dev/null; then
    ok "T28 Makefile help line says 'portable exe + NSIS installer'"
else
    bad "T28 Makefile help line says 'portable exe + NSIS installer'"
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
# Artifact names per DESIGN §5.7: portable exe + NSIS installer.
GOODNAME="PeakBot-Desktop-${VERSION}-windows-x86_64-portable.exe"
GOODSETUP="PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe"

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

# run_sw <exe> <setup> <version> <sidecar> → sets SW_OUT / SW_RC for the
# assertions. S-W1 checks BOTH names first, so dummy files can reach it.
run_sw() {
    SW_OUT=$(sh "$SW" "$1" "$2" "$3" "$4" 2>&1)
    SW_RC=$?
}
sw_fail_line() {  # the ❌ line of the last smoke run (for diagnostics)
    printf '%s\n' "$SW_OUT" | grep '❌' | head -1
}

# Dummy sidecar + dummy setup so the smoke run gets past the file-existence
# checks; the setup is a plain file — S-W1 is the first check that sees it.
printf 'x' > "$workdir/fake-sidecar.exe"
printf 'dummy' > "$workdir/$GOODSETUP"

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

# T34: 0/3/5 args → usage, exit 2 (the contract takes exactly 4).
t34_ok=1
SW_OUT=$(sh "$SW" 2>&1);            [ "$?" -eq 2 ] && printf '%s' "$SW_OUT" | grep -q 'usage' || t34_ok=0
SW_OUT=$(sh "$SW" a b c 2>&1);      [ "$?" -eq 2 ] && printf '%s' "$SW_OUT" | grep -q 'usage' || t34_ok=0
SW_OUT=$(sh "$SW" a b c d e 2>&1);  [ "$?" -eq 2 ] && printf '%s' "$SW_OUT" | grep -q 'usage' || t34_ok=0
if [ "$t34_ok" -eq 1 ]; then
    ok "T34 smoke-windows.sh 0/3/5 args → exit 2 + usage"
else
    bad "T34 smoke-windows.sh 0/3/5 args → exit 2 + usage"
fi

# T35: missing portable → 'not found' (4-arg call, dummy setup + sidecar).
run_sw "$workdir/no-such-$GOODNAME" "$workdir/$GOODSETUP" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'not found'; then
    ok "T35 missing portable → 'not found'"
else
    bad "T35 missing portable → 'not found' (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T36: portable named with the OLD bare name → fails at S-W1 (the name
# contract is checked before anything that needs a real PE).
printf 'not a PE' > "$workdir/PeakBot-Desktop-${VERSION}-windows-x86_64.exe"
run_sw "$workdir/PeakBot-Desktop-${VERSION}-windows-x86_64.exe" "$workdir/$GOODSETUP" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W1'; then
    ok "T36 portable with the old bare name fails at S-W1 (filename contract)"
else
    bad "T36 portable with the old bare name fails at S-W1 (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T37: correctly named portable (text dummy) + dummy setup → FAIL at S-W2.
printf 'not a PE' > "$workdir/$GOODNAME"
run_sw "$workdir/$GOODNAME" "$workdir/$GOODSETUP" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W2'; then
    ok "T37 correctly-named text dummy fails at S-W2 (not a PE32+ executable)"
else
    bad "T37 correctly-named text dummy fails at S-W2 (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T38: correctly named fake PE32+ with subsystem 3 (console) → FAIL at S-W2
# (S-W2 requires the (GUI) subsystem for the portable).
make_fake_pe "$workdir/$GOODNAME" 3
run_sw "$workdir/$GOODNAME" "$workdir/$GOODSETUP" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W2'; then
    ok "T38 fake PE32+ console (subsystem 3) fails at S-W2 (not GUI)"
else
    bad "T38 fake PE32+ console (subsystem 3) fails at S-W2 (got exit $SW_RC: $(sw_fail_line))"
fi

# T39: correctly named fake PE32+ GUI (subsystem 2) + a fake PE32+ sidecar →
# S-W2 passes. The later checks (S-W3..S-W6) need a real PE with an import
# table, an embedded payload and a real NSIS archive, which are proven for
# real in the container; here we only assert the run REACHED and passed S-W2.
make_fake_pe "$workdir/$GOODNAME" 2
make_fake_pe "$workdir/fake-sidecar.exe" 3
run_sw "$workdir/$GOODNAME" "$workdir/$GOODSETUP" "$VERSION" "$workdir/fake-sidecar.exe"
if printf '%s' "$SW_OUT" | grep -q '✅ S-W2'; then
    ok "T39 fake PE32+ GUI (subsystem 2) + fake sidecar → S-W2 passes"
else
    bad "T39 fake PE32+ GUI (subsystem 2) + fake sidecar → S-W2 passes (got: $(sw_fail_line))"
fi

# T40: missing setup → 'not found' (portable present, correctly named).
make_fake_pe "$workdir/$GOODNAME" 2
run_sw "$workdir/$GOODNAME" "$workdir/no-such-$GOODSETUP" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'not found'; then
    ok "T40 missing setup → 'not found'"
else
    bad "T40 missing setup → 'not found' (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T41: setup under Tauri's raw NSIS name (PeakBot Desktop_…_x64-setup.exe)
# → fails at S-W1: the installer must ship under the contract name.
make_fake_pe "$workdir/$GOODNAME" 2
printf 'dummy' > "$workdir/PeakBot Desktop_${VERSION}_x64-setup.exe"
run_sw "$workdir/$GOODNAME" "$workdir/PeakBot Desktop_${VERSION}_x64-setup.exe" "$VERSION" "$workdir/fake-sidecar.exe"
if [ "$SW_RC" -eq 1 ] && printf '%s' "$SW_OUT" | grep -q 'S-W1'; then
    ok "T41 setup named 'PeakBot Desktop_${VERSION}_x64-setup.exe' fails at S-W1"
else
    bad "T41 setup named 'PeakBot Desktop_${VERSION}_x64-setup.exe' fails at S-W1 (got exit $SW_RC: $(printf '%s' "$SW_OUT" | head -1))"
fi

# T42: the tool guard requires 7z (S-W6 inspects the NSIS archive; repo rule:
# smoke checks never SKIP).
if grep -qF 'for tool in' "$SW" 2>/dev/null \
    && grep -F 'for tool in' "$SW" 2>/dev/null | grep -q '7z'; then
    ok "T42 smoke-windows.sh tool guard includes 7z"
else
    bad "T42 smoke-windows.sh tool guard includes 7z"
fi

# T43: the image is on trixie (its NSIS 3.11 ships RestartManager.nsh).
grep_ok "T43 Dockerfile.desktop-windows is FROM ghcr.io/rust-lang/rust:1.91-trixie AS build" \
    'FROM ghcr.io/rust-lang/rust:1.91-trixie AS build' "$DFW"

# T44: NSIS + 7zip installed, the RestartManager.nsh header guard is present,
# and the @tauri-apps/cli pin EQUALS the pin in Dockerfile.desktop-linux
# (extracted, not hard-coded).
t44_ok=1; t44_why=""
grep -qF 'nsis' "$DFW" 2>/dev/null || { t44_ok=0; t44_why="${t44_why} no nsis; "; }
grep -qF '7zip' "$DFW" 2>/dev/null || { t44_ok=0; t44_why="${t44_why} no 7zip; "; }
grep -qF 'test -f /usr/share/nsis/Include/Win/RestartManager.nsh' "$DFW" 2>/dev/null \
    || { t44_ok=0; t44_why="${t44_why} no RestartManager.nsh guard; "; }
pin_lin=$(grep -oE '@tauri-apps/cli@[0-9]+\.[0-9]+\.[0-9]+' "$DFW_LINUX" 2>/dev/null | head -1)
[ -n "$pin_lin" ] || { t44_ok=0; t44_why="${t44_why}no CLI pin found in $DFW_LINUX; "; }
pin_win=$(grep -oE '@tauri-apps/cli@[0-9]+\.[0-9]+\.[0-9]+' "$DFW" 2>/dev/null | head -1)
[ -n "$pin_win" ] || { t44_ok=0; t44_why="${t44_why}no CLI pin in $DFW; "; }
[ -z "$pin_win" ] || [ "$pin_win" = "$pin_lin" ] \
    || { t44_ok=0; t44_why="${t44_why}CLI pin drift: windows=$pin_win linux=$pin_lin; "; }
if [ "$t44_ok" -eq 1 ]; then
    ok "T44 Dockerfile.desktop-windows installs nsis + 7zip, guards RestartManager.nsh, CLI pin == $pin_lin (matches $DFW_LINUX)"
else
    bad "T44 Dockerfile.desktop-windows nsis + 7zip + RestartManager.nsh guard + CLI pin ($t44_why)"
fi

# T45: the NSIS build step — the pinned CLI invocation (every flag present)
# and the exactly-one *-setup.exe glob.
t45_ok=1; t45_why=""
cli_line=$(stmt_text "$(stmt_of '@tauri-apps/cli' "$DFW")")
for tok in 'build --ci' '--bundles nsis' '--runner cargo-xwin' \
           '--target x86_64-pc-windows-msvc' \
           "--config '{\"bundle\":{\"externalBin\":[]}}'" '-- --locked'; do
    case "$cli_line" in
        *"$tok"*) ;;
        *) t45_ok=0; t45_why="${t45_why}CLI line lacks $tok; " ;;
    esac
done
grep -qF 'bundle/nsis/*-setup.exe' "$DFW" 2>/dev/null \
    || { t45_ok=0; t45_why="${t45_why}no bundle/nsis/*-setup.exe glob; "; }
grep -F 'bundle/nsis/*-setup.exe' "$DFW" 2>/dev/null | grep -qF '"$#" -eq 1' \
    || { t45_ok=0; t45_why="${t45_why}glob lacks the exactly-one (\"#\" -eq 1) check; "; }
if [ "$t45_ok" -eq 1 ]; then
    ok "T45 Dockerfile NSIS step: pinned CLI with all flags + exactly-one *-setup.exe glob"
else
    bad "T45 Dockerfile NSIS step: pinned CLI with all flags + exactly-one *-setup.exe glob ($t45_why)"
fi

# T46: I-C ordering — the portable cp to /out/ must land BEFORE the Tauri CLI
# step (the CLI patches target/…/peakbot-desktop.exe in place).
cp_data=$(stmt_of 'cp target/x86_64-pc-windows-msvc/release/peakbot-desktop.exe' "$DFW")
cp_line=$(stmt_line "$cp_data")
cp_text=$(stmt_text "$cp_data")
cli_nline=$(stmt_line "$(stmt_of '@tauri-apps/cli' "$DFW")")
case "$cp_text" in
    *'-portable.exe'*) ;;
    *) cp_line="" ;;
esac
if [ -n "$cp_line" ] && [ -n "$cli_nline" ] && [ "$cp_line" -lt "$cli_nline" ]; then
    ok "T46 I-C: portable cp (line $cp_line) precedes the @tauri-apps/cli step (line $cli_nline)"
else
    bad "T46 I-C: portable cp (line ${cp_line:-not found}) must precede the @tauri-apps/cli step (line ${cli_nline:-not found})"
fi

# T47: tauri.conf.json — targets/externalBin invariants (I-A, I-B) plus the
# explicit Windows bundle decisions (grep -F, no jq dependency).
t47_ok=1; t47_why=""
for tok in '"targets": ["appimage"]' '"externalBin": ["binaries/peakbot"]' \
           '"installMode": "currentUser"' '"type": "downloadBootstrapper"'; do
    grep -qF -- "$tok" "$TC" 2>/dev/null || { t47_ok=0; t47_why="${t47_why} missing $tok; "; }
done
if [ "$t47_ok" -eq 1 ]; then
    ok "T47 tauri.conf.json: appimage targets, externalBin kept, installMode currentUser, downloadBootstrapper"
else
    bad "T47 tauri.conf.json: appimage targets, externalBin kept, installMode currentUser, downloadBootstrapper ($t47_why)"
fi

# T48: ci.yml — the desktop job's apt list includes 7zip (the smoke unit
# tests now run S-W6's tooling; the word check ignores 'llvm-7zip').
apt_lines=$(awk '
    {
        l = $0
        if (l ~ /\\[ \t]*$/) { sub(/\\[ \t]*$/, "", l); buf = buf " " l; next }
        stmt = buf " " l
        buf = ""
        if (index(stmt, "apt-get install") > 0) print stmt
    }
' "$CI" 2>/dev/null)
apt_line=$(printf '%s\n' "$apt_lines" | grep -F 'file llvm' | head -1)
if [ -n "$apt_line" ] && printf '%s\n' "$apt_line" | grep -q '7zip'; then
    ok "T48 ci.yml desktop job apt list includes 7zip"
else
    bad "T48 ci.yml desktop job apt list includes 7zip (apt line: ${apt_line:-not found})"
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
