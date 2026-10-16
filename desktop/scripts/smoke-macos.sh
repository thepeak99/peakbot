#!/bin/sh
#
# smoke-macos.sh <zip> <version>
#
# macOS artifact smoke checks — desktop/DESIGN.md §9.2 (S-M1..S-M6) plus
# S-M7 (UI assets embedded in the desktop binary). Runs inside the build
# container, where unzip, file and rcodesign are available.
#
# Any check failure → exit 1 (the docker build fails with it).

set -eu

[ "$#" -eq 2 ] || { echo "usage: smoke-macos.sh <zip> <version>" >&2; exit 2; }
ZIP=$1
VERSION=$2

fail() { echo "❌ S-M$1: $2" >&2; exit 1; }
pass() { echo "✅ S-M$1 $2"; }

[ -f "$ZIP" ] || fail 0 "artifact not found: $ZIP"

# S-M6: the zip filename matches the artifact contract.
base=$(basename "$ZIP")
[ "$base" = "PeakBot-Desktop-${VERSION}-macos-universal.zip" ] \
    || fail 6 "zip is '$base', expected 'PeakBot-Desktop-${VERSION}-macos-universal.zip'"
pass 6 "zip name is $base"

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
unzip -q "$ZIP" -d "$workdir" || fail 0 "unzip failed"
APP="$workdir/PeakBot Desktop.app"
[ -d "$APP/Contents" ] || fail 0 "extracted archive has no 'PeakBot Desktop.app/Contents'"

# S-M1: the zip lists EXACTLY the ten expected entries — nothing more.
expected=$(printf '%s\n' \
    "PeakBot Desktop.app/" \
    "PeakBot Desktop.app/Contents/" \
    "PeakBot Desktop.app/Contents/Info.plist" \
    "PeakBot Desktop.app/Contents/MacOS/" \
    "PeakBot Desktop.app/Contents/MacOS/peakbot-desktop" \
    "PeakBot Desktop.app/Contents/MacOS/peakbot" \
    "PeakBot Desktop.app/Contents/Resources/" \
    "PeakBot Desktop.app/Contents/Resources/icon.icns" \
    "PeakBot Desktop.app/Contents/_CodeSignature/" \
    "PeakBot Desktop.app/Contents/_CodeSignature/CodeResources" \
    | LC_ALL=C sort)
actual=$(unzip -Z1 "$ZIP" | LC_ALL=C sort)
[ "$actual" = "$expected" ] || fail 1 "zip entry list mismatch:
--- actual ---
$actual
--- expected ---
$expected"
pass 1 "zip lists exactly the ten expected entries"

# S-M2: both MacOS binaries are mode 755 after extraction.
for b in peakbot-desktop peakbot; do
    mode=$(stat -c '%a' "$APP/Contents/MacOS/$b")
    [ "$mode" = "755" ] || fail 2 "Contents/MacOS/$b mode is $mode, expected 755"
done
pass 2 "both MacOS binaries are mode 755"

# S-M3: both binaries are universal Mach-O with exactly 2 architectures
# (x86_64 + arm64; `file` appends per-slice details inside the brackets).
for b in peakbot-desktop peakbot; do
    desc=$(file -b "$APP/Contents/MacOS/$b")
    case "$desc" in
        *"Mach-O universal binary with 2 architectures"*) : ;;
        *) fail 3 "Contents/MacOS/$b is not a 2-arch universal Mach-O: $desc" ;;
    esac
    printf '%s' "$desc" | grep -q 'x86_64' || fail 3 "Contents/MacOS/$b has no x86_64 slice: $desc"
    printf '%s' "$desc" | grep -q 'arm64' || fail 3 "Contents/MacOS/$b has no arm64 slice: $desc"
done
pass 3 "both binaries are Mach-O universal (x86_64 + arm64)"

# S-M4: Info.plist keys.
plist="$APP/Contents/Info.plist"
plist_val() {
    grep -A1 "<key>$1</key>" "$plist" \
        | grep -o '<string>[^<]*</string>' | head -1 | sed 's/<[^>]*>//g'
}
[ "$(plist_val CFBundleExecutable)" = "peakbot-desktop" ] \
    || fail 4 "CFBundleExecutable != peakbot-desktop"
[ "$(plist_val CFBundleIdentifier)" = "com.peakbot.desktop" ] \
    || fail 4 "CFBundleIdentifier != com.peakbot.desktop"
[ "$(plist_val CFBundleShortVersionString)" = "$VERSION" ] \
    || fail 4 "CFBundleShortVersionString != $VERSION"
[ "$(plist_val LSMinimumSystemVersion)" = "11.0" ] \
    || fail 4 "LSMinimumSystemVersion != 11.0"
grep -A1 '<key>NSAllowsLocalNetworking</key>' "$plist" | grep -q '<true/>' \
    || fail 4 "NSAllowsLocalNetworking is not <true/>"
pass 4 "Info.plist keys are correct"

# S-M5: ad-hoc signatures present and valid.
# `rcodesign print-signature-info` emits one YAML entity per Mach-O slice
# (sub_path: macho-index:N), each with its code_directory — a universal
# binary must report two slices, each with a code directory.
rcodesign print-signature-info "$APP" > "$workdir/bundle-sig.yaml" 2>&1 \
    || fail 5 "print-signature-info failed on the .app: $(tail -n 3 "$workdir/bundle-sig.yaml" | tr '\n' ' ')"
rcodesign print-signature-info "$APP/Contents/MacOS/peakbot" > "$workdir/sidecar-sig.yaml" 2>&1 \
    || fail 5 "print-signature-info failed on Contents/MacOS/peakbot: $(tail -n 3 "$workdir/sidecar-sig.yaml" | tr '\n' ' ')"
for f in "$workdir/bundle-sig.yaml" "$workdir/sidecar-sig.yaml"; do
    grep -q 'sub_path: macho-index:0' "$f" || fail 5 "$f: missing slice macho-index:0"
    grep -q 'sub_path: macho-index:1' "$f" || fail 5 "$f: missing slice macho-index:1"
done
n_dirs=$(grep -c 'code_directory:' "$workdir/sidecar-sig.yaml" || true)
[ "$n_dirs" -ge 2 ] || fail 5 "sidecar signature info has $n_dirs code_directories, expected >= 2 (one per slice)"
# `rcodesign verify` cross-checks the code directory digests against the
# binary content (the real crypto check). Known upstream bug in
# apple-codesign 0.29.0: for ad-hoc signatures it ALSO reports
# "CMS error: missing further values" for the intentionally empty CMS slot
# that rcodesign's own ad-hoc signing writes (macho_signing.rs
# create_empty_cms_signature) — the tool itself warns that verify is buggy.
# So: run verify, filter out that exact false positive (and the tool's
# warning/success boilerplate); any line that remains is a real failure.
for b in peakbot-desktop peakbot; do
    vout=$({ rcodesign verify "$APP/Contents/MacOS/$b" 2>&1 || true; } \
        | grep -v 'CMS error: missing further values' \
        | grep -v 'known to be buggy' \
        | grep -v 'no problems detected' \
        | grep -v 'we do not verify everything' \
        | grep -v 'problems reported during verification' \
        | grep -v '^[[:space:]]*$' || true)
    [ -z "$vout" ] || fail 5 "rcodesign verify found real problems on Contents/MacOS/$b:
$vout"
done
pass 5 "signatures present (code directory on both slices) and rcodesign verify passes"

# S-M7 (extra, P4.2): prove the ui/ status page was embedded into the
# desktop binary via tauri/custom-protocol. The marker string comes from
# ui/index.html. Tauri compresses embedded assets only when its `compression`
# cargo feature is enabled (it is not), so the plaintext marker is expected;
# if it is missing we note it and skip rather than fail (per task spec).
if grep -aq 'peakbot-desktop-status-page' "$APP/Contents/MacOS/peakbot-desktop"; then
    pass 7 "UI marker 'peakbot-desktop-status-page' found in peakbot-desktop (ui/ assets embedded)"
else
    echo "⚠️  S-M7: UI marker 'peakbot-desktop-status-page' not found in plaintext — assets may be compressed; skipping (informational only)"
fi

echo "🎉 all macOS smoke checks passed: $base"
