#!/bin/sh
#
# smoke-windows.sh <installer> <version>
#
# Windows artifact smoke checks S-W1..S-W4 (desktop/DESIGN.md §9.2, Windows
# extension). Runs inside the build container, where file and stat are
# available. Any check failure → exit 1 (the docker build fails with it).
#
# S-W4 is best-effort by design: it needs 7z to list the NSIS payload. If
# 7z is missing from the container, the check prints a SKIP and passes —
# a container without 7z simply cannot verify the payload. If 7z IS
# present, a payload that does not list peakbot.exe is a hard failure
# (the installer must embed the sidecar). Dockerfile.desktop-windows
# should install 7z (e.g. p7zip-full) to make this check real.

set -eu

[ "$#" -eq 2 ] || { echo "usage: smoke-windows.sh <installer> <version>" >&2; exit 2; }
INSTALLER=$1
VERSION=$2

fail() { echo "❌ S-W$1: $2" >&2; exit 1; }
pass() { echo "✅ S-W$1: $2"; }

[ -f "$INSTALLER" ] || fail 0 "installer not found: $INSTALLER"
base=$(basename "$INSTALLER")

# S-W1: filename matches the artifact contract.
[ "$base" = "PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe" ] \
    || fail 1 "installer is '$base', expected 'PeakBot-Desktop-${VERSION}-windows-x86_64-setup.exe'"
pass 1 "installer name is $base"

# S-W2: file(1) reports a PE32+ x86-64 Windows executable.
# Unanchored substrings, two word orders: file 5.38 says
# "PE32+ executable (GUI) x86-64, MS Windows, …" while file 5.46 says
# "PE32+ executable for MS Windows 0.00 (GUI), x86-64".
desc=$(file -b "$INSTALLER")
case "$desc" in
    *"PE32+ executable"*"x86-64"*"MS Windows"*) ;;
    *"PE32+ executable"*"MS Windows"*"x86-64"*) ;;
    *) fail 2 "not a PE32+ x86-64 Windows executable: $desc" ;;
esac
pass 2 "PE32+ executable, x86-64 (MS Windows)"

# S-W3: size sanity — a real NSIS installer with the Tauri stack plus the
# peakbot sidecar is far bigger than 2 MB.
size=$(stat -c '%s' "$INSTALLER")
[ "$size" -gt 2097152 ] \
    || fail 3 "installer is $size bytes, expected > 2 MB"
pass 3 "size $size bytes (> 2 MB)"

# S-W4: the NSIS payload embeds the peakbot sidecar (best-effort, needs 7z).
# Tauri bundles externalBin as peakbot.exe (triple suffix stripped).
if command -v 7z >/dev/null 2>&1; then
    7z l "$INSTALLER" 2>/dev/null | grep -q 'peakbot\.exe' \
        || fail 4 "NSIS payload does not list peakbot.exe"
    pass 4 "NSIS payload lists peakbot.exe"
else
    echo "⚠️  S-W4: 7z not available in this container — skipping NSIS payload check (best effort)"
fi

echo "🎉 all Windows smoke checks passed: $base"
