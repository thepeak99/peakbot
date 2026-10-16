#!/bin/sh
#
# assemble-app.sh <version> <desktop-bin> <sidecar-bin> <outdir>
#
# Hand-assemble the PeakBot Desktop .app bundle and export it as a zip
# (desktop/DESIGN.md §5.7). The Tauri CLI cannot bundle a .app on Linux,
# so the layout is built by hand:
#
#   PeakBot Desktop.app/Contents/Info.plist
#   PeakBot Desktop.app/Contents/MacOS/peakbot-desktop   (755)
#   PeakBot Desktop.app/Contents/MacOS/peakbot           (755)
#   PeakBot Desktop.app/Contents/Resources/icon.icns
#
# then ad-hoc signed with rcodesign (no cert args — it signs every nested
# Mach-O in both slices and writes Contents/_CodeSignature/CodeResources),
# then zipped.
#
# The zip entry list must stay EXACTLY the ten entries checked by
# scripts/smoke-macos.sh S-M1 (§9.2) — don't add files to the bundle
# without updating that check.

set -eu

[ "$#" -eq 4 ] || {
    echo "usage: assemble-app.sh <version> <desktop-bin> <sidecar-bin> <outdir>" >&2
    exit 2
}
VERSION=$1
DESKTOP_BIN=$2
SIDECAR_BIN=$3
OUTDIR=$4

command -v rcodesign >/dev/null 2>&1 || { echo "❌ rcodesign not found in PATH" >&2; exit 1; }
[ -f "$DESKTOP_BIN" ] || { echo "❌ desktop binary not found: $DESKTOP_BIN" >&2; exit 1; }
[ -f "$SIDECAR_BIN" ] || { echo "❌ sidecar binary not found: $SIDECAR_BIN" >&2; exit 1; }

# Resolve the desktop/ tree from this script's location (desktop/macos/).
script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
PLIST_IN="$script_dir/Info.plist.in"
ICON="$script_dir/../src-tauri/icons/icon.icns"
[ -f "$PLIST_IN" ] || { echo "❌ missing Info.plist template: $PLIST_IN" >&2; exit 1; }
[ -f "$ICON" ] || { echo "❌ missing icon: $ICON" >&2; exit 1; }

mkdir -p "$OUTDIR"
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

APP="$workdir/PeakBot Desktop.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# Info.plist with the version substituted.
sed "s/@VERSION@/$VERSION/g" "$PLIST_IN" > "$APP/Contents/Info.plist"

install -m 755 "$DESKTOP_BIN" "$APP/Contents/MacOS/peakbot-desktop"
install -m 755 "$SIDECAR_BIN" "$APP/Contents/MacOS/peakbot"
cp "$ICON" "$APP/Contents/Resources/icon.icns"

# Ad-hoc sign: no cert args. rcodesign signs both Contents/MacOS Mach-Os
# (every slice of the universal binaries) and the bundle itself.
rcodesign sign "$APP"

# Zip the bundle. -y: store symlinks as symlinks; Info-ZIP keeps unix modes,
# so the 755 binaries survive the round-trip (checked by smoke S-M2).
rm -f "$OUTDIR/PeakBot-Desktop-$VERSION-macos-universal.zip"
( cd "$workdir" && zip -qry "$OUTDIR/PeakBot-Desktop-$VERSION-macos-universal.zip" "PeakBot Desktop.app" )

echo "✅ $OUTDIR/PeakBot-Desktop-$VERSION-macos-universal.zip"
