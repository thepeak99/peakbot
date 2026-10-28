#!/bin/sh
#
# smoke-windows.sh <portable-exe> <setup-exe> <version> <sidecar>
#
# Windows artifact smoke checks S-W1..S-W6 (DESIGN §9.2, amendment
# windows-nsis). Runs inside the build container, where file,
# llvm-objdump, sha256sum and 7z are available. Any check failure →
# exit 1 (the docker build fails with it).
#
# There are NO SKIP paths: a missing tool is a hard failure (DESIGN rule:
# container smoke checks never SKIP). 7z is required because S-W6 is the
# only proof that the installer carries no second peakbot.exe (I-B) and
# that the embedded exe carries the payload id (I-D).
#
#   <portable-exe>  the self-extracting portable exe
#                   (PeakBot-Desktop-<version>-windows-x86_64-portable.exe)
#   <setup-exe>     the NSIS installer
#                   (PeakBot-Desktop-<version>-windows-x86_64-setup.exe)
#   <version>       the desktop workspace version (e.g. 0.23.0)
#   <sidecar>       the staged peakbot sidecar whose bytes ARE the embedded
#                   payload (src-tauri/binaries/peakbot-x86_64-pc-windows-msvc.exe)

set -eu

[ "$#" -eq 4 ] || { echo "usage: smoke-windows.sh <portable-exe> <setup-exe> <version> <sidecar>" >&2; exit 2; }
PORTABLE=$1
SETUP=$2
VERSION=$3
SIDECAR=$4

# Required tools — a missing tool fails the build, it is never a skip.
for tool in file llvm-objdump sha256sum 7z; do
    command -v "$tool" >/dev/null 2>&1 \
        || { echo "❌ missing required tool: $tool" >&2; exit 1; }
done

fail() { echo "❌ S-W$1: $2" >&2; exit 1; }
pass() { echo "✅ S-W$1: $2"; }

# contains <haystack> <needle> — order-independent substring test.
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

[ -f "$PORTABLE" ] || { echo "❌ portable not found: $PORTABLE" >&2; exit 1; }
[ -f "$SETUP" ] || { echo "❌ setup not found: $SETUP" >&2; exit 1; }
[ -f "$SIDECAR" ] || { echo "❌ sidecar not found: $SIDECAR" >&2; exit 1; }
pbase=$(basename "$PORTABLE")
sbase=$(basename "$SETUP")

# S-W1: both filenames match the artifact contract exactly. Checked FIRST,
# before anything that needs a real PE, so callers cannot ship a
# misnamed file and discover it three checks later.
[ "$pbase" = "PeakBot-Desktop-${VERSION}-windows-x86_64-portable.exe" ] \
    || fail 1 "portable is '$pbase', expected 'PeakBot-Desktop-${VERSION}-windows-x86_64-portable.exe'"
[ "$sbase" = "PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe" ] \
    || fail 1 "setup is '$sbase', expected 'PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe'"
pass 1 "portable is $pbase; setup is $sbase"

# S-W2: the portable is a PE32+ x86-64 (GUI) MS Windows executable; the
# sidecar is a PE32+ x86-64 executable. Substring checks are
# order-independent so both file versions match: 5.38 says "PE32+
# executable (GUI) x86-64, MS Windows, …" while 5.46 says "PE32+
# executable for MS Windows 0.00 (GUI), x86-64".
desc=$(file -b "$PORTABLE")
contains "$desc" "PE32+ executable" && contains "$desc" "x86-64" \
    && contains "$desc" "MS Windows" && contains "$desc" "GUI" \
    || fail 2 "portable is not a PE32+ x86-64 (GUI) MS Windows executable: $desc"
sdesc=$(file -b "$SIDECAR")
contains "$sdesc" "PE32+ executable" && contains "$sdesc" "x86-64" \
    || fail 2 "sidecar is not a PE32+ x86-64 executable: $sdesc"
pass 2 "portable is PE32+ x86-64 (GUI) MS Windows; sidecar is PE32+ x86-64"

# S-W3: import table — for BOTH the portable and the sidecar,
# `llvm-objdump -p` exits 0 and prints at least one `DLL Name:` line (so
# the check cannot pass on an empty or unparsed import table), and NO line
# is a VC++ runtime import (the static-CRT contract). The portable
# additionally must not import WebView2Loader.dll.
imports_of() {
    llvm-objdump -p "$1" 2>/dev/null \
        || fail 3 "llvm-objdump -p $2 exited non-zero"
}
imp=$(imports_of "$PORTABLE" "portable")
printf '%s\n' "$imp" | grep -q 'DLL Name:' \
    || fail 3 "portable import table is empty (no 'DLL Name:' lines)"
printf '%s\n' "$imp" | grep -qiE 'DLL Name: *(vcruntime|msvcp)' \
    && fail 3 "portable imports a VC++ runtime DLL (vcruntime/msvcp) — the CRT must be static"
printf '%s\n' "$imp" | grep -q 'WebView2Loader.dll' \
    && fail 3 "portable imports WebView2Loader.dll"
imp=$(imports_of "$SIDECAR" "sidecar")
printf '%s\n' "$imp" | grep -q 'DLL Name:' \
    || fail 3 "sidecar import table is empty (no 'DLL Name:' lines)"
printf '%s\n' "$imp" | grep -qiE 'DLL Name: *(vcruntime|msvcp)' \
    && fail 3 "sidecar imports a VC++ runtime DLL (vcruntime/msvcp) — the CRT must be static"
pass 3 "portable + sidecar parse, import ≥1 DLL, no vcruntime/msvcp, no WebView2Loader (portable)"

# S-W4: the payload is present — the portable is > 10 MB and embeds the
# payload id "<version>-<sha12>", where sha12 is the first 12 hex of the
# STAGED sidecar's SHA-256. Matching the id proves the staged sidecar is
# exactly the payload's content (DESIGN I1: identity = content).
size=$(stat -c '%s' "$PORTABLE")
[ "$size" -gt 10485760 ] \
    || fail 4 "portable is $size bytes, expected > 10 MB (the payload must be embedded)"
sha12=$(sha256sum "$SIDECAR" | cut -c1-12)
grep -aqF "${VERSION}-${sha12}" "$PORTABLE" \
    || fail 4 "portable does not embed the payload id '${VERSION}-${sha12}'"
pass 4 "portable is $size bytes (> 10 MB) and embeds payload id ${VERSION}-${sha12}"

# S-W5: the installer is a Nullsoft (NSIS) PE32 self-extracting archive
# and big enough to carry the app. file 5.46 says "PE32 executable (GUI)
# Intel 80386 … Nullsoft Installer self-extracting archive" — "PE32
# executable" (not "PE32+") pins the 32-bit NSIS stub, "Nullsoft" the
# archive family.
ssize=$(stat -c '%s' "$SETUP")
[ "$ssize" -gt 10485760 ] \
    || fail 5 "setup is $ssize bytes, expected > 10 MB (the app must be embedded)"
sdesc=$(file -b "$SETUP")
contains "$sdesc" "PE32 executable" && contains "$sdesc" "Nullsoft" \
    || fail 5 "setup is not a Nullsoft (NSIS) PE32 executable: $sdesc"
pass 5 "setup is $ssize bytes (> 10 MB), PE32 + Nullsoft: $sdesc"

# S-W6: the installer's contents. (1) it lists peakbot-desktop.exe (the
# suffix-anchored match tolerates a 7z path prefix such as $INSTDIR\);
# (2) it lists NO entry that is exactly peakbot.exe — proves the
# externalBin override (I-B: no second, unused sidecar in the bundle);
# (3) the extracted peakbot-desktop.exe embeds the SAME payload id as the
# portable (I-D: one identity in both artifacts).
listing=$(7z l -ba "$SETUP" 2>/dev/null) \
    || fail 6 "7z l failed on $SETUP"
printf '%s\n' "$listing" | grep -q 'peakbot-desktop.exe' \
    || fail 6 "installer does not list peakbot-desktop.exe"
printf '%s\n' "$listing" | grep -qE '(^|[ /\\])peakbot\.exe$' \
    && fail 6 "installer carries a second peakbot.exe (externalBin override failed)"
7z e -so "$SETUP" -r peakbot-desktop.exe 2>/dev/null | grep -aqF "${VERSION}-${sha12}" \
    || fail 6 "embedded peakbot-desktop.exe does not carry the payload id '${VERSION}-${sha12}'"
pass 6 "installer lists peakbot-desktop.exe, no peakbot.exe, embedded exe carries payload id ${VERSION}-${sha12}"

echo "🎉 all Windows smoke checks passed: $pbase + $sbase"
