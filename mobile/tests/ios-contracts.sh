#!/bin/sh
#
# ios-contracts.sh — static contract tests for the iOS build
# (pure file greps + awk,
#  runs on Linux CI — no Mac, no xcodebuild, no plutil).
#
# Written BEFORE the implementation: expect RED until the developer lands
#   Project  mobile/src-tauri/Info.ios.plist + committed gen/apple (tauri ios
#          init: bundle sh.shifu.mobile, team placeholder, Rust build phase
#          = `npm run -- tauri ios xcode-script`)
#   Glue     iOS glue in mobile/src-tauri/src/ (lib.rs or ios.rs):
#          #[cfg(target_os = "ios")] edge-swipe via with_webview +
#          setAllowsBackForwardNavigationGestures(true), and
#          NSURLIsExcludedFromBackupKey on the data root
#   Sim      mobile/scripts/ios-sim.sh
#   Build    mobile/scripts/build-ios.sh + `make mobile-ios`
# The rust-toolchain.toml and with_webview checks are GREEN guards
# (toolchain pin; the Android nav glue already uses with_webview).
#
# EXACT STRINGS THE BUILDER MUST HIT (the contract, check by check):
#
# Info.ios.plist (structural awk — no plutil on Linux):
#   * NSAppTransportSecurity dict CONTAINING NSAllowsLocalNetworking=<true/>
#   * NSCameraUsageDescription string EXACTLY
#     "Scan a pairing QR code, or take a photo to attach to a conversation."
#   * NSLocalNetworkUsageDescription string NON-EMPTY
#   * no NSAllowsArbitraryLoads anywhere
#
# build-ios.sh:
#   * xcodebuild -version  +  26.3            (pinned Xcode assertion)
#   * /usr/local/bin AND /opt/homebrew/bin    (brew on non-login ssh PATH)
#   * npm run tauri --                        (never a global `tauri`
#     line — any line STARTING with `tauri ` fails the contract)
#   * npm ci
#   * npm run build                           (web/dist, like build-android)
#   * --export-method app-store-connect
#   * APPLE_DEVELOPMENT_TEAM
#   * PeakBot-Mobile-  +  -ios.ipa  +  tauri.conf.json
#     (output/mobile/PeakBot-Mobile-<v>-ios.ipa, <v> from tauri.conf.json)
#   * no `set -x` (signing identities never echoed)
#
# ios-sim.sh:
#   * START_TS  +  `date +%s >`  +  -newer     (freshness reference file)
#   * DerivedData  +  debug-iphonesimulator/PeakBot.app
#   * npm run tauri -- ios build  +  --debug --ci
#   * x86_64 (Intel) AND aarch64-sim (Apple silicon) AND uname -m
#   * output/mobile/sim/PeakBot.app
#   * xcrun simctl install booted
#   * simctl launch booted sh.shifu.mobile  with & on the same line
#     (launch backgrounded: it can hang over ssh)
#   * 2>&1  and  build.log appearing >= 2 times (captured, then printed
#     on failure — a non-zero exit is tolerated ONLY for a fresh .app)
#
# gen/apple:
#   * directory exists, is COMMITTED (git ls-files non-empty)
#   * project.yml and/or *.pbxproj: `npm run -- tauri ios xcode-script`
#     (the Rust build phase) and bundle id sh.shifu.mobile
#   * no hardcoded team id: `DEVELOPMENT_TEAM` must be empty/placeholder,
#     never a 10-char [A-Z0-9] id
#
# rust-toolchain.toml (guard, GREEN): channel = "1.100.0" + the three
#   apple-ios targets (aarch64-apple-ios, aarch64-apple-ios-sim,
#   x86_64-apple-ios) — matched with the closing quote so the -sim
#   entry cannot satisfy the device entry.
#
# glue (in mobile/src-tauri/src/lib.rs OR ios.rs, LIVE lines only —
#   comments cannot satisfy these):
#   * #[cfg(target_os = "ios")]
#   * setAllowsBackForwardNavigationGestures set to true — the
#     objc2 form `...Gestures: true]` or a helper call `...(true)`
#   * NSURLIsExcludedFromBackupKey
#   * with_webview (guard — already GREEN from the Android nav glue)
#
# Makefile:
#   * a real `mobile-ios:` target (anchored — .PHONY alone is not enough)
#   * referencing mobile/scripts/build-ios.sh
#
# Run:  sh mobile/tests/ios-contracts.sh      (standalone, from anywhere)
#       sh mobile/tests/contracts.sh          (CI entry point — calls this)
# Exit: 0 when every check passes (GREEN); 1 with a failing-check list
#       otherwise (RED — each failure names the missing implementation piece).

set -u

script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)

PL="$root/mobile/src-tauri/Info.ios.plist"
BI="$root/mobile/scripts/build-ios.sh"
SI="$root/mobile/scripts/ios-sim.sh"
GA="$root/mobile/src-tauri/gen/apple"
RT="$root/mobile/rust-toolchain.toml"
MK="$root/Makefile"
# The glue may live in lib.rs or a dedicated ios.rs module.
GLUE="$root/mobile/src-tauri/src/lib.rs $root/mobile/src-tauri/src/ios.rs"

PASS=0
FAIL=0
FAILED=""

ok()  { PASS=$((PASS+1)); echo "✅ $1"; }
bad() { FAIL=$((FAIL+1)); FAILED="${FAILED}  - $1\n"; echo "❌ $1"; }

# grep_ok <name> <fixed-string> <file...>
grep_ok() {
    name=$1; str=$2; shift 2
    if grep -qF -- "$str" "$@" 2>/dev/null; then ok "$name"; else bad "$name (missing: $str)"; fi
}

# grep_bad <name> <fixed-string> <file...> — passes when the string is ABSENT.
grep_bad() {
    name=$1; str=$2; shift 2
    if grep -qF -- "$str" "$@" 2>/dev/null; then bad "$name (forbidden: $str)"; else ok "$name"; fi
}

# grep_ok_live <name> <fixed-string> <file...> — passes only when the string
# occurs on a NON-COMMENT line (header comments naming the symbols must not
# satisfy the contract; same live-line rule as contracts.sh). Shell-style `#` only.
grep_ok_live() {
    name=$1; str=$2; shift 2
    if grep -v '^[[:space:]]*#' "$@" 2>/dev/null | grep -qF -- "$str"; then ok "$name"; else bad "$name (missing live: $str)"; fi
}

# grep_ok_live_rust <name> <fixed-string> <file...> — like grep_ok_live, but
# drops Rust `//` comment lines. NOTE: a leading `#` is an ATTRIBUTE in Rust
# (#[cfg(...)]), not a comment — it must not be filtered out.
grep_ok_live_rust() {
    name=$1; str=$2; shift 2
    if grep -vE '^[[:space:]]*//' "$@" 2>/dev/null | grep -qF -- "$str"; then ok "$name"; else bad "$name (missing live: $str)"; fi
}

# grep_ok_re <name> <extended-regex> <file...>
grep_ok_re() {
    name=$1; re=$2; shift 2
    if grep -qE -- "$re" "$@" 2>/dev/null; then ok "$name"; else bad "$name (missing pattern: $re)"; fi
}

# grep_ok_re_live <name> <extended-regex> <file...> — regex on LIVE lines
# only (drops Rust `//` comment lines; `#` attributes stay — see above).
grep_ok_re_live() {
    name=$1; re=$2; shift 2
    if grep -vE '^[[:space:]]*//' "$@" 2>/dev/null | grep -qE -- "$re"; then ok "$name"; else bad "$name (missing live pattern: $re)"; fi
}

# grep_bad_re <name> <extended-regex> <file...> — passes when NO line matches.
# Missing files pass (nothing forbidden can live in a file that is absent).
grep_bad_re() {
    name=$1; re=$2; shift 2
    if grep -qE -- "$re" "$@" 2>/dev/null; then bad "$name (forbidden pattern: $re)"; else ok "$name"; fi
}

# grep_ok_count <name> <fixed-string> <min-count> <file>
grep_ok_count() {
    n=$(grep -oF -- "$2" "$4" 2>/dev/null | wc -l)
    if [ "$n" -ge "$3" ]; then ok "$1"; else bad "$1 (found $n, need >= $3: $2)"; fi
}

# ── Info.ios.plist ───────────────────────────────────────────────────────

if [ ! -f "$PL" ]; then
    bad "Info.ios.plist exists"
else
    ok "Info.ios.plist exists"

    # NSAllowsLocalNetworking = true, INSIDE the NSAppTransportSecurity dict
    # (a true elsewhere in the plist must not count).
    if awk '
        /<key>NSAppTransportSecurity<\/key>/ { ats = 1; next }
        ats == 1 && /<dict>/ { ats = 2; next }
        ats == 2 && /<\/dict>/ { exit }
        ats == 2 && /<key>NSAllowsLocalNetworking<\/key>/ { ln = 1; next }
        ln == 1 && /<true\/>/ { ok = 1; exit }
        END { exit ok ? 0 : 1 }
    ' "$PL"; then
        ok "plist: NSAllowsLocalNetworking=true inside NSAppTransportSecurity"
    else
        bad "plist: NSAllowsLocalNetworking=true inside NSAppTransportSecurity"
    fi

    # Exact camera string (word-for-word — the App Store review string).
    if awk '
        /<key>NSCameraUsageDescription<\/key>/ { c = 1; next }
        c == 1 && /<string>/ {
            s = $0
            sub(/^[[:space:]]*<string>/, "", s)
            sub(/<\/string>[[:space:]]*$/, "", s)
            if (s == "Scan a pairing QR code, or take a photo to attach to a conversation.") ok = 1
            exit
        }
        END { exit ok ? 0 : 1 }
    ' "$PL"; then
        ok "plist: NSCameraUsageDescription = \"Scan a pairing QR code, or take a photo to attach to a conversation.\""
    else
        bad "plist: NSCameraUsageDescription = \"Scan a pairing QR code, or take a photo to attach to a conversation.\""
    fi

    # LAN providers need the local-network prompt string (non-empty).
    if awk '
        /<key>NSLocalNetworkUsageDescription<\/key>/ { c = 1; next }
        c == 1 && /<string>/ {
            s = $0
            sub(/^[[:space:]]*<string>/, "", s)
            sub(/<\/string>[[:space:]]*$/, "", s)
            if (s !~ /^[[:space:]]*$/) ok = 1
            exit
        }
        END { exit ok ? 0 : 1 }
    ' "$PL"; then
        ok "plist: NSLocalNetworkUsageDescription non-empty"
    else
        bad "plist: NSLocalNetworkUsageDescription non-empty"
    fi

    # ATS: local networking only — arbitrary loads are a release-blocker.
    grep_bad "plist: no NSAllowsArbitraryLoads" "NSAllowsArbitraryLoads" "$PL"
fi

# ── build-ios.sh ─────────────────────────────────────────────────────────

if [ ! -f "$BI" ]; then
    bad "mobile/scripts/build-ios.sh exists"
else
    # Pinned Xcode (axlotl: 26.3 — the current floor).
    grep_ok "build-ios asserts xcodebuild -version" "xcodebuild -version" "$BI"
    grep_ok "build-ios pins Xcode 26.3" "26.3" "$BI"
    # brew is not on a non-login ssh PATH: extend for BOTH prefixes.
    grep_ok "build-ios PATH /usr/local/bin (Intel brew)" "/usr/local/bin" "$BI"
    grep_ok "build-ios PATH /opt/homebrew/bin (Apple-silicon brew)" "/opt/homebrew/bin" "$BI"
    # The CLI is a devDependency — always `npm run tauri --`; a line
    # starting with a bare `tauri` is the global-CLI anti-pattern.
    grep_ok "build-ios uses 'npm run tauri --'" "npm run tauri --" "$BI"
    grep_bad_re "build-ios never calls a global tauri" '^[[:space:]]*tauri[[:space:]]' "$BI"
    grep_ok "build-ios runs npm ci" "npm ci" "$BI"
    # web/dist: the Local core embeds the SPA (same step as build-android).
    grep_ok "build-ios builds the web bundle (npm run build)" "npm run build" "$BI"
    # The app-store archive path + the team from the human's environment.
    grep_ok "build-ios --export-method app-store-connect" "--export-method app-store-connect" "$BI"
    grep_ok "build-ios requires APPLE_DEVELOPMENT_TEAM" "APPLE_DEVELOPMENT_TEAM" "$BI"
    # Deliverable: output/mobile/PeakBot-Mobile-<v>-ios.ipa, <v> from
    # tauri.conf.json (the single source of truth, like build-android).
    grep_ok "build-ios output name PeakBot-Mobile-" "PeakBot-Mobile-" "$BI"
    grep_ok "build-ios output suffix -ios.ipa" "-ios.ipa" "$BI"
    grep_ok "build-ios reads version from tauri.conf.json" "tauri.conf.json" "$BI"
    # Signing identities never go to the agent: no xtrace.
    grep_bad "build-ios has no 'set -x' (secrets never echoed)" "set -x" "$BI"
fi

# ── ios-sim.sh ───────────────────────────────────────────────────────────

if [ ! -f "$SI" ]; then
    bad "mobile/scripts/ios-sim.sh exists"
else
    # `ios build` exits 1 at the archive step (no team) — the sim .app
    # is already in DerivedData by then. So: record a start timestamp, and
    # tolerate the non-zero exit ONLY if the .app is NEWER than it.
    grep_ok "ios-sim records start timestamp (START_TS)" "START_TS" "$SI"
    grep_ok "ios-sim writes the timestamp reference file (date +%s >)" "date +%s >" "$SI"
    grep_ok "ios-sim freshness check uses -newer" "-newer" "$SI"
    grep_ok "ios-sim looks under DerivedData" "DerivedData" "$SI"
    grep_ok "ios-sim app path debug-iphonesimulator/PeakBot.app" "debug-iphonesimulator/PeakBot.app" "$SI"
    # The build command with the sim flags.
    grep_ok "ios-sim uses 'npm run tauri -- ios build'" "npm run tauri -- ios build" "$SI"
    grep_ok "ios-sim flags --debug --ci" "--debug --ci" "$SI"
    # Target chosen by arch: Intel x86_64, Apple silicon aarch64-sim.
    grep_ok "ios-sim Intel target x86_64" "x86_64" "$SI"
    grep_ok "ios-sim Apple-silicon target aarch64-sim" "aarch64-sim" "$SI"
    grep_ok "ios-sim detects arch via uname -m" "uname -m" "$SI"
    # Deliverable + install + launch on the booted simulator.
    grep_ok "ios-sim copies to output/mobile/sim/PeakBot.app" "output/mobile/sim/PeakBot.app" "$SI"
    grep_ok "ios-sim installs: xcrun simctl install booted" "xcrun simctl install booted" "$SI"
    grep_ok "ios-sim launches sh.shifu.mobile" "simctl launch booted sh.shifu.mobile" "$SI"
    # The launch must be BACKGROUNDED (simctl launch can hang over ssh).
    grep_ok_re "ios-sim launch is backgrounded (&)" 'simctl launch booted sh\.shifu\.mobile.*&' "$SI"
    # … else fail printing the log: the build output is captured (2>&1) and
    # the log file is written then printed (build.log >= 2 occurrences).
    grep_ok "ios-sim captures build output (2>&1)" "2>&1" "$SI"
    grep_ok_count "ios-sim build.log written AND printed on failure" "build.log" 2 "$SI"
fi

# ── gen/apple ────────────────────────────────────────────────────────────

if [ ! -d "$GA" ]; then
    bad "gen/apple directory exists (tauri ios init)"
else
    ok "gen/apple directory exists"
    # The Rust build phase: the Xcode phase must run the CLI through
    # npm, in project.yml (xcodegen) and/or the generated .pbxproj.
    grep_ok "gen/apple Rust phase 'npm run -- tauri ios xcode-script'" \
        "npm run -- tauri ios xcode-script" "$GA/project.yml" "$GA"/*.pbxproj
    grep_ok "gen/apple bundle id sh.shifu.mobile" \
        "sh.shifu.mobile" "$GA/project.yml" "$GA"/*.pbxproj
    # No team id baked into the committed project: DEVELOPMENT_TEAM must be
    # empty or a placeholder, never a 10-char [A-Z0-9] identifier.
    grep_bad_re "gen/apple has no hardcoded DEVELOPMENT_TEAM id" \
        'DEVELOPMENT_TEAM[[:space:]]*[:=][[:space:]]*["'"'"']?[A-Z0-9]{10}' \
        "$GA/project.yml" "$GA"/*.pbxproj
    # Committed (not just present on a Mac's disk): git tracks it.
    if git -C "$root" ls-files -- "mobile/src-tauri/gen/apple" 2>/dev/null | grep -q .; then
        ok "gen/apple is committed (git ls-files non-empty)"
    else
        bad "gen/apple is committed (git ls-files non-empty)"
    fi
fi

# ── rust-toolchain.toml ─────────────────────────────

# The Xcode Rust phase ignores RUSTUP_TOOLCHAIN; rustup reads this file from
# cargo's cwd. The closing quote in each target string keeps the -sim entry
# from satisfying the device entry (and vice versa).
grep_ok "toolchain pins channel = \"1.100.0\"" 'channel = "1.100.0"' "$RT"
grep_ok "toolchain lists aarch64-apple-ios" '"aarch64-apple-ios",' "$RT"
grep_ok "toolchain lists aarch64-apple-ios-sim" '"aarch64-apple-ios-sim",' "$RT"
grep_ok "toolchain lists x86_64-apple-ios" '"x86_64-apple-ios",' "$RT"

# ── iOS glue ─────────────────────────────────────────────────────────────

# lib.rs or a dedicated ios.rs module — LIVE lines only, so a
# comment naming the API cannot satisfy the contract.
grep_ok_live_rust "glue: #[cfg(target_os = \"ios\")] block" '#[cfg(target_os = "ios")]' $GLUE
# The edge-swipe selector, set ON. Both canonical forms are accepted:
# the objc2 call `msg_send![wk, setAllowsBackForwardNavigationGestures: true]`
# and a wrapped helper call `setAllowsBackForwardNavigationGestures(true)`.
grep_ok_re_live "glue: setAllowsBackForwardNavigationGestures set to true" \
    'setAllowsBackForwardNavigationGestures[[:space:]]*\([[:space:]]*true[[:space:]]*\)|setAllowsBackForwardNavigationGestures[[:space:]]*:[[:space:]]*true' \
    $GLUE
grep_ok_live_rust "glue: NSURLIsExcludedFromBackupKey on the data root" "NSURLIsExcludedFromBackupKey" $GLUE
# Guard (already GREEN): the edge-swipe goes through the existing with_webview
# seam (lib.rs nav glue); the iOS branch sits next to it.
grep_ok_live_rust "glue: with_webview seam (guard)" "with_webview" $GLUE

# ── Makefile ──────────────────────────────────────────────────────────────

# A real target (anchored — a .PHONY mention alone must not count).
grep_ok_re "Makefile has a mobile-ios: target" '^mobile-ios[[:space:]]*:' "$MK"
grep_ok "Makefile mobile-ios calls mobile/scripts/build-ios.sh" "mobile/scripts/build-ios.sh" "$MK"

# ── verdict ───────────────────────────────────────────────────────────────

echo
echo "ios-contracts: $PASS passed, $FAIL failed"
if [ "$FAIL" -ne 0 ]; then
    printf 'failing checks:\n%s\n' "$FAILED"
    exit 1
fi
exit 0
