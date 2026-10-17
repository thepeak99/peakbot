#!/bin/sh
#
# smoke-windows.sh <exe> <version> <sidecar>
#
# Windows artifact smoke checks S-W1..S-W4 (WINDOWS-PLAN §3.4). Runs inside
# the build container, where file, llvm-objdump and sha256sum are available.
# Any check failure → exit 1 (the docker build fails with it).
#
# There are NO SKIP paths: a missing tool is a hard failure (DESIGN rule:
# container smoke checks never SKIP).
#
#   <exe>      the desktop exe (PeakBot-Desktop-<version>-windows-x86_64.exe)
#   <version>  the desktop workspace version (e.g. 0.23.0)
#   <sidecar>  the staged peakbot sidecar whose bytes ARE the embedded payload
#              (src-tauri/binaries/peakbot-x86_64-pc-windows-msvc.exe)

set -eu

[ "$#" -eq 3 ] || { echo "usage: smoke-windows.sh <exe> <version> <sidecar>" >&2; exit 2; }
EXE=$1
VERSION=$2
SIDECAR=$3

# Required tools — a missing tool fails the build, it is never a skip.
for tool in file llvm-objdump sha256sum; do
    command -v "$tool" >/dev/null 2>&1 \
        || { echo "❌ missing required tool: $tool" >&2; exit 1; }
done

fail() { echo "❌ S-W$1: $2" >&2; exit 1; }
pass() { echo "✅ S-W$1: $2"; }

# contains <haystack> <needle> — order-independent substring test.
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

[ -f "$EXE" ] || { echo "❌ exe not found: $EXE" >&2; exit 1; }
[ -f "$SIDECAR" ] || { echo "❌ sidecar not found: $SIDECAR" >&2; exit 1; }
base=$(basename "$EXE")

# S-W1: filename matches the artifact contract exactly.
[ "$base" = "PeakBot-Desktop-${VERSION}-windows-x86_64.exe" ] \
    || fail 1 "exe is '$base', expected 'PeakBot-Desktop-${VERSION}-windows-x86_64.exe'"
pass 1 "exe name is $base"

# S-W2: the exe is a PE32+ x86-64 (GUI) MS Windows executable; the sidecar is
# a PE32+ x86-64 executable. Substring checks are order-independent so both
# file versions match: 5.38 says "PE32+ executable (GUI) x86-64, MS Windows, …"
# while 5.46 says "PE32+ executable for MS Windows 0.00 (GUI), x86-64".
desc=$(file -b "$EXE")
contains "$desc" "PE32+ executable" && contains "$desc" "x86-64" \
    && contains "$desc" "MS Windows" && contains "$desc" "GUI" \
    || fail 2 "exe is not a PE32+ x86-64 (GUI) MS Windows executable: $desc"
sdesc=$(file -b "$SIDECAR")
contains "$sdesc" "PE32+ executable" && contains "$sdesc" "x86-64" \
    || fail 2 "sidecar is not a PE32+ x86-64 executable: $sdesc"
pass 2 "exe is PE32+ x86-64 (GUI) MS Windows; sidecar is PE32+ x86-64"

# S-W3: import table — for BOTH the exe and the sidecar, `llvm-objdump -p`
# exits 0 and prints at least one `DLL Name:` line (so the check cannot pass
# on an empty or unparsed import table), and NO line is a VC++ runtime import
# (the static-CRT contract). The exe additionally must not import
# WebView2Loader.dll.
imports_of() {
    llvm-objdump -p "$1" 2>/dev/null \
        || fail 3 "llvm-objdump -p $2 exited non-zero"
}
imp=$(imports_of "$EXE" "exe")
printf '%s\n' "$imp" | grep -q 'DLL Name:' \
    || fail 3 "exe import table is empty (no 'DLL Name:' lines)"
printf '%s\n' "$imp" | grep -qiE 'DLL Name: *(vcruntime|msvcp)' \
    && fail 3 "exe imports a VC++ runtime DLL (vcruntime/msvcp) — the CRT must be static"
printf '%s\n' "$imp" | grep -q 'WebView2Loader.dll' \
    && fail 3 "exe imports WebView2Loader.dll"
imp=$(imports_of "$SIDECAR" "sidecar")
printf '%s\n' "$imp" | grep -q 'DLL Name:' \
    || fail 3 "sidecar import table is empty (no 'DLL Name:' lines)"
printf '%s\n' "$imp" | grep -qiE 'DLL Name: *(vcruntime|msvcp)' \
    && fail 3 "sidecar imports a VC++ runtime DLL (vcruntime/msvcp) — the CRT must be static"
pass 3 "exe + sidecar parse, import ≥1 DLL, no vcruntime/msvcp, no WebView2Loader (exe)"

# S-W4: the payload is present — the exe is > 10 MB and embeds the payload id
# "<version>-<sha12>", where sha12 is the first 12 hex of the STAGED sidecar's
# SHA-256. Matching the id proves the staged sidecar is exactly the payload's
# content (WINDOWS-PLAN §2 I1).
size=$(stat -c '%s' "$EXE")
[ "$size" -gt 10485760 ] \
    || fail 4 "exe is $size bytes, expected > 10 MB (the payload must be embedded)"
sha12=$(sha256sum "$SIDECAR" | cut -c1-12)
grep -aqF "${VERSION}-${sha12}" "$EXE" \
    || fail 4 "exe does not embed the payload id '${VERSION}-${sha12}'"
pass 4 "exe is $size bytes (> 10 MB) and embeds payload id ${VERSION}-${sha12}"

echo "🎉 all Windows smoke checks passed: $base"
