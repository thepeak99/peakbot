#!/bin/sh
#
# updater-manifest.sh <version> <dir> <base_url> <notes_file>
#
# Build the auto-updater manifest <dir>/latest.json for tauri-plugin-updater
# from the three SIGNED update artifacts in <dir>:
#   PeakBot-Desktop-<v>-linux-x86_64.AppImage(.sig)
#   PeakBot-Desktop-<v>-windows-x86_64-setup.exe(.sig)
#   PeakBot-Desktop-<v>-macos-universal.app.tar.gz(.sig)
#
# latest.json = {version, notes, pub_date, platforms} where platforms has
# exactly linux-x86_64, windows-x86_64, darwin-x86_64 and darwin-aarch64
# (the darwin pair is IDENTICAL — one universal tarball + one signature).
# Each entry: .url = <base_url>/<asset filename>, .signature = the .sig
# content trimmed of its trailing newline.
#
# Written atomically (temp file + mv): any failure exits non-zero and
# leaves NO latest.json behind.
#
# Run:  sh desktop/scripts/updater-manifest.sh <version> <dir> <base_url> <notes_file>

set -eu

[ "$#" -eq 4 ] || {
    echo "usage: updater-manifest.sh <version> <dir> <base_url> <notes_file>" >&2
    exit 2
}
VERSION=$1
DIR=$2
BASE_URL=$3
NOTES=$4

command -v jq >/dev/null 2>&1 || { echo "❌ jq not found in PATH" >&2; exit 1; }
[ -d "$DIR" ] || { echo "❌ artifact dir not found: $DIR" >&2; exit 1; }
[ -f "$NOTES" ] || { echo "❌ notes file not found: $NOTES" >&2; exit 1; }

# sig_of <file> — the .sig content, trailing newlines trimmed (the manifest
# stores the signature as a single line).
sig_of() { cat "$1"; }

# Check all six inputs BEFORE writing anything (fail → no latest.json).
check_artifact() {
    [ -f "$1" ] || { echo "❌ missing artifact: $1" >&2; exit 1; }
    [ -f "$1.sig" ] || { echo "❌ missing signature: $1.sig" >&2; exit 1; }
    [ -s "$1.sig" ] || { echo "❌ empty signature: $1.sig" >&2; exit 1; }
}

LINUX="$DIR/PeakBot-Desktop-$VERSION-linux-x86_64.AppImage"
WINDOWS="$DIR/PeakBot-Desktop-$VERSION-windows-x86_64-setup.exe"
MACOS="$DIR/PeakBot-Desktop-$VERSION-macos-universal.app.tar.gz"
check_artifact "$LINUX"
check_artifact "$WINDOWS"
check_artifact "$MACOS"

TMP="$DIR/.latest.json.tmp"
trap 'rm -f "$TMP"' EXIT

# Full artifact URLs (built in the shell — works with jq >= 1.5; the
# `+` string operator needs jq 1.7 and the Ubuntu 22.04 image has 1.6).
LINUX_URL="$BASE_URL/PeakBot-Desktop-$VERSION-linux-x86_64.AppImage"
WINDOWS_URL="$BASE_URL/PeakBot-Desktop-$VERSION-windows-x86_64-setup.exe"
MACOS_URL="$BASE_URL/PeakBot-Desktop-$VERSION-macos-universal.app.tar.gz"

jq -n \
    --arg version "$VERSION" \
    --arg notes "$(cat "$NOTES")" \
    --arg pub_date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg linux_url "$LINUX_URL" \
    --arg windows_url "$WINDOWS_URL" \
    --arg macos_url "$MACOS_URL" \
    --arg linux_sig "$(sig_of "$LINUX.sig")" \
    --arg windows_sig "$(sig_of "$WINDOWS.sig")" \
    --arg macos_sig "$(sig_of "$MACOS.sig")" \
    '{
        version: $version,
        notes: $notes,
        pub_date: $pub_date,
        platforms: {
            "linux-x86_64": { url: $linux_url, signature: $linux_sig },
            "windows-x86_64": { url: $windows_url, signature: $windows_sig },
            "darwin-x86_64": { url: $macos_url, signature: $macos_sig },
            "darwin-aarch64": { url: $macos_url, signature: $macos_sig }
        }
    }' > "$TMP"
mv "$TMP" "$DIR/latest.json"

echo "✅ $DIR/latest.json (version $VERSION)"
