#!/bin/sh
#
# ios-sim.sh — PeakBot Mobile simulator build.
#
# Host script on the Mac (Xcode can't run in a container). Tauri 2.12's
# `ios build` always archives + signs after building, and without a team it
# exits 1 at the archive step — by then the simulator .app is already
# in DerivedData. So the script:
#   1. records a start timestamp (the freshness reference)
#   2. web/dist (always rebuilt — the Local core embeds the SPA) +
#      cd mobile && npm ci, then
#      `npm run tauri -- ios build --target <x86_64|aarch64-sim>
#      --debug --ci` (the CLI is a devDependency; Intel = x86_64,
#      Apple silicon = aarch64-sim)
#   3. tolerates the non-zero exit ONLY if
#      DerivedData/peakbot-mobile-*/Build/Products/debug-iphonesimulator/
#      PeakBot.app exists and is NEWER than the timestamp (else it fails
#      and prints the build log)
#   4. copies it to output/mobile/sim/PeakBot.app, then
#      `xcrun simctl install booted …` and `simctl launch booted
#      sh.shifu.mobile` on the BOOTED simulator (launch backgrounded: it
#      can hang over ssh)
#
# With a team set, the same script works (the archive then succeeds).
# Sim results on loopback are approximate: the sim shares the Mac's
# network namespace.

set -u

# The worktree root (this script lives at mobile/scripts/).
root=$(cd "$(dirname "$0")/../.." && pwd)
mobile="$root/mobile"

# brew is not on a non-login ssh PATH: extend for BOTH prefixes
# (Intel /usr/local, Apple silicon /opt/homebrew) + the pinned node/cargo.
# ~/.cargo/bin goes FIRST: brew may carry its own `rust` formula (axlotl:
# 1.99.1) whose cargo ignores rust-toolchain.toml and has no iOS std.
export PATH="$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:$HOME/.local/opt/node-v22.23.4-darwin-x64/bin:$PATH"
# The pinned Xcode (axlotl: 26.3) — xcode-select may point elsewhere.
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode-26.3.0.app/Contents/Developer}"

# Target by arch: Intel x86_64, Apple silicon aarch64-sim (the tauri flag
# — `tauri ios build --help` — not the rust triple).
case "$(uname -m)" in
  x86_64) TARGET=x86_64 ;;
  arm64)  TARGET=aarch64-sim ;;
  *) echo "❌ unknown arch: $(uname -m)" >&2; exit 1 ;;
esac

# 1. Freshness reference: the .app must be NEWER than this file.
mkdir -p "$root/output/mobile"
START_TS_FILE="$root/output/mobile/.sim-build-start"
date +%s > "$START_TS_FILE"

# 2. The Local core embeds the SPA from web/dist: ALWAYS build it (a
#    "build if absent" check silently shipped a stale SPA; the `make web`
#    equivalent). The CLI is a devDependency in mobile/package.json.
echo "🕸️  building the SPA (make web equivalent)..."
(cd "$root/web" && npm ci --no-audit --no-fund && npm run build) || { echo "❌ web build failed" >&2; exit 1; }
touch "$root/web/dist/.gitkeep"
(cd "$mobile" && npm ci --no-audit --no-fund) || { echo "❌ npm ci failed" >&2; exit 1; }

echo "🔨 npm run tauri -- ios build --target $TARGET --debug --ci"
(cd "$mobile" && npm run tauri -- ios build --target "$TARGET" --debug --ci \
    > "$root/output/mobile/build.log" 2>&1)
rc=$?
if [ "$rc" -ne 0 ]; then
    echo "ℹ️  ios build exited $rc (expected without a team); checking DerivedData for a fresh .app..."
fi

# 3. The deliverable .app: it must exist AND be fresh — else fail with
#    the log (a non-zero exit is tolerated ONLY for a fresh .app).
app=$(ls -dt "$HOME"/Library/Developer/Xcode/DerivedData/peakbot-mobile-*/Build/Products/debug-iphonesimulator/PeakBot.app 2>/dev/null | head -n 1)
fresh=""
[ -n "$app" ] && [ -d "$app" ] \
    && fresh=$(find "$app" -newer "$START_TS_FILE" -print 2>/dev/null | head -n 1)
if [ -z "$fresh" ]; then
    echo "❌ no fresh PeakBot.app in DerivedData (build rc=$rc) — build log:" >&2
    cat "$root/output/mobile/build.log" >&2
    exit 1
fi
echo "📦 fresh .app: $app"

# 4. Deliverable + install + launch on the booted simulator.
mkdir -p "$root/output/mobile/sim"
rm -rf "$root/output/mobile/sim/PeakBot.app"
cp -R "$app" "$root/output/mobile/sim/PeakBot.app"
echo "✅ $root/output/mobile/sim/PeakBot.app"

if ! xcrun simctl list devices booted | grep -q "(Booted)"; then
    echo "❌ no booted simulator — the .app is ready at output/mobile/sim/PeakBot.app." >&2
    echo "   Boot one (e.g. xcrun simctl boot <udid>; list: xcrun simctl list devices available) and re-run." >&2
    exit 2
fi
xcrun simctl install booted "$root/output/mobile/sim/PeakBot.app" \
    || { echo "❌ simctl install failed" >&2; exit 1; }
# Launch BACKGROUNDED: simctl launch can hang over ssh.
(xcrun simctl launch booted sh.shifu.mobile &)
echo "🚀 launched sh.shifu.mobile on the booted simulator"
