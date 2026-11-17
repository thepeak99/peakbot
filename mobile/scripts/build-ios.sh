#!/bin/sh
#
# build-ios.sh — PeakBot Mobile iOS .ipa build.
#
# Host script on the Mac (Xcode can't run in a container). The single
# entry point for `make mobile-ios`.
#
# Steps:
#   1. assert the pinned Xcode (axlotl floor: 26.3)
#   2. APPLE_DEVELOPMENT_TEAM from the human's environment (never the repo)
#   3. web/dist (always rebuilt — the Local core embeds the SPA)
#   4. cd mobile && npm ci (@tauri-apps/cli 2.12.4 devDependency)
#   5. npm run tauri -- ios build --target aarch64
#      --export-method app-store-connect
#   6. output/mobile/PeakBot-Mobile-<v>-ios.ipa (<v> from tauri.conf.json)
#      → uploaded by the human via Transporter / xcrun altool
#
# Signing identities never go to the agent: no xtrace, and the team
# arrives as the human's env var, never on argv.

set -eu

# The worktree root (this script lives at mobile/scripts/).
root=$(cd "$(dirname "$0")/../.." && pwd)
mobile="$root/mobile"

# brew is not on a non-login ssh PATH: extend for BOTH prefixes
# (Intel /usr/local, Apple silicon /opt/homebrew) + the pinned node/cargo.
# ~/.cargo/bin goes FIRST: brew may carry its own `rust` formula (axlotl:
# 1.100.0) whose cargo ignores rust-toolchain.toml and has no iOS std.
export PATH="$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:$HOME/.local/opt/node-v22.23.4-darwin-x64/bin:$PATH"
# The pinned Xcode (axlotl: 26.3) — xcode-select may point elsewhere.
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode-26.3.0.app/Contents/Developer}"

# 1. Pinned Xcode: the current upload floor (Xcode 27 needs Tahoe
#    26.6, which axlotl can't run).
xcode_line=$(xcodebuild -version | head -n 1)
case "$xcode_line" in
  "Xcode 26.3") ;;
  *) echo "❌ expected the pinned Xcode 26.3, found: $xcode_line" >&2; exit 1 ;;
esac
echo "🛠️  $xcode_line"

# 2. The team comes from the human's environment — never from the repo.
[ -n "${APPLE_DEVELOPMENT_TEAM:-}" ] \
    || { echo "❌ APPLE_DEVELOPMENT_TEAM is not set (the human's team)" >&2; exit 1; }
export APPLE_DEVELOPMENT_TEAM

# Version: mobile/src-tauri/tauri.conf.json (the single source of truth;
# the Makefile's release-bump keeps it in sync with the root Cargo.toml).
VERSION=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$mobile/src-tauri/tauri.conf.json" | head -n 1)
[ -n "$VERSION" ] \
    || { echo "❌ could not read version from mobile/src-tauri/tauri.conf.json" >&2; exit 1; }
echo "📱 PeakBot Mobile $VERSION (iOS)"

# 3. The Local core embeds the SPA from web/dist (like build-android).
echo "🕸️  building the SPA (make web equivalent)..."
(cd "$root/web" && npm ci --no-audit --no-fund && npm run build)
touch "$root/web/dist/.gitkeep"

# 4. The CLI is a devDependency in mobile/package.json — always go
#    through `npm run tauri --`, never a global tauri.
(cd "$mobile" && npm ci --no-audit --no-fund)

# 5. The app-store archive + export. Tauri 2.12's `ios build`
#    archives and signs for the device; with the team it exports the .ipa.
find "$mobile/src-tauri/gen/apple/build" -name '*.ipa' -exec rm -f {} + 2>/dev/null || true
echo "🔨 npm run tauri -- ios build --target aarch64 --export-method app-store-connect"
(cd "$mobile" && npm run tauri -- ios build --target aarch64 \
    --export-method app-store-connect --ci)

# 6. Deliverable: output/mobile/PeakBot-Mobile-<v>-ios.ipa.
# Tauri exports it under gen/apple/build/<arch>/ (cargo-mobile2 export_dir).
ipa_src=$(find "$mobile/src-tauri/gen/apple/build" -name '*.ipa' 2>/dev/null | head -n 1)
[ -n "$ipa_src" ] && [ -f "$ipa_src" ] \
    || { echo "❌ no .ipa found under $mobile/src-tauri/gen/apple/build" >&2; exit 1; }
mkdir -p "$root/output/mobile"
cp "$ipa_src" "$root/output/mobile/PeakBot-Mobile-$VERSION-ios.ipa"
ls -lh "$root/output/mobile/PeakBot-Mobile-$VERSION-ios.ipa"
echo "✅ $root/output/mobile/PeakBot-Mobile-$VERSION-ios.ipa (the human uploads it: Transporter / xcrun altool)"
