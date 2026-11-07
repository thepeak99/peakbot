#!/bin/sh
#
# smoke-android.sh <apk> — PeakBot Mobile APK assertions (M8, plan §3.1).
#
# Runs INSIDE the Dockerfile.mobile-android toolchain container (aapt2,
# apksigner, zipalign from build-tools 36.0.0). Fails the build (exit 1)
# on any violation. Wired into build-android.sh (M7); also usable standalone:
#   sh mobile/scripts/smoke-android.sh output/mobile/PeakBot-Mobile-<v>-android-arm64.apk
#
# Assertions:
#   1. aapt2 dump badging: package sh.shifu.mobile, versionName = the
#      tauri.conf.json version, minSdkVersion='24', targetSdkVersion='37',
#      native-code exactly 'arm64-v8a' (no other ABIs)
#   2. apksigner verify --print-certs (always). A release-signed APK (any
#      name other than *-debug.apk) additionally pins its cert SHA-256
#      against mobile/release-cert.sha256 (one fingerprint line, the
#      apksigner format). REL-1 has not created that file yet: when it is
#      absent, print a warning for release builds and skip the pin.
#   3. zipalign -c -P 16 -v 4 — 16 KB page alignment (S10 / R11)
#   4. merged manifest (aapt2 dump xmltree): networkSecurityConfig present
#      + allowBackup="false" (no cloud backup of the token/config)
#   5. the NSC resource (extracted from the APK): src="user" trust anchors
#      (user-installed PeakBot CA) + the 127.0.0.1 domain-config (cleartext
#      for the in-process Local core only)
#   6. size < 40 MB (41943040 bytes; plan §3.1 — spike: 22.1 MB). Release
#      builds only: debug APKs are never released (unstripped .so, debug
#      symbols) and skip the cap.
#
# L10 hook (Phase 3, plan §4.3 test 6): the symbol check (llvm-nm -D /
# strings on the unstripped .so — no portable_pty, TokioChildProcess,
# BashTool, ratatui) lands there as the body of run_l10_symbol_check().

set -u

apk=${1:-}
[ -n "$apk" ] && [ -f "$apk" ] \
    || { echo "usage: smoke-android.sh <apk>" >&2; exit 2; }

script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
mobile_root=$(cd "$script_dir/.." >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)
conf="$mobile_root/src-tauri/tauri.conf.json"
[ -f "$conf" ] || { echo "❌ smoke: missing $conf" >&2; exit 1; }
VERSION=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$conf" | head -n 1)
[ -n "$VERSION" ] || { echo "❌ smoke: could not read version from tauri.conf.json" >&2; exit 1; }

fail() { echo "❌ smoke: $1" >&2; exit 1; }
ok()   { echo "✅ smoke: $1"; }

# ── 1. badging: identity, version, SDKs, ABIs ────────────────────────────
badging=$(aapt2 dump badging "$apk") || fail "aapt2 dump badging $apk"
ok "aapt2 dump badging"

printf '%s\n' "$badging" | grep -qF "package: name='sh.shifu.mobile'" \
    || fail "package is not sh.shifu.mobile"
ok "package sh.shifu.mobile"

printf '%s\n' "$badging" | grep -qF "versionName='$VERSION'" \
    || fail "versionName is not '$VERSION' (tauri.conf.json)"
ok "versionName $VERSION"

# aapt2 (build-tools 36) prints `minSdkVersion:'24'` / `targetSdkVersion:'37'`
# (aapt1 used `uses-sdk:minSdkVersion='24'`).
min_sdk=$(printf '%s\n' "$badging" | grep -F 'minSdkVersion:' | sed "s/^minSdkVersion:'//; s/'$//")
[ "$min_sdk" = "24" ] \
    || fail "minSdkVersion='24' missing (got '${min_sdk:-<none>}')"
ok "minSdkVersion='24'"

target_sdk=$(printf '%s\n' "$badging" | grep -F 'targetSdkVersion:' | sed "s/^targetSdkVersion:'//; s/'$//")
[ "$target_sdk" = "37" ] \
    || fail "targetSdkVersion='37' missing (got '${target_sdk:-<none>}')"
ok "targetSdkVersion='37'"

# Exactly one native-code line, and it is arm64-v8a (no armv7/i686/x86_64).
abis=$(printf '%s\n' "$badging" | grep -F 'native-code:' | sed "s/^native-code: '//; s/'$//")
[ "$abis" = "arm64-v8a" ] \
    || fail "native-code ABIs are '${abis:-<none>}', expected exactly 'arm64-v8a'"
ok "native-code arm64-v8a only"

# ── 2. signing: always verify; pin the release cert (REL-1) ─────────────
certs=$(apksigner verify --print-certs "$apk") || fail "apksigner verify --print-certs"
ok "apksigner verify"

kind=release
case $(basename "$apk") in
    *-debug.apk) kind=debug ;;
esac

if [ "$kind" = release ]; then
    # apksigner prints: `Signer #1 certificate SHA-256 digest: <hex>`.
    sha=$(printf '%s\n' "$certs" | grep -F 'SHA-256 digest:' | head -n 1 | sed 's/.*SHA-256 digest:[[:space:]]*//')
    [ -n "$sha" ] || fail "no SHA-256 fingerprint in the apksigner output"
    pin="$mobile_root/release-cert.sha256"
    if [ -f "$pin" ]; then
        # The pin file holds the SHA-256 fingerprint of the release
        # certificate — plain lowercase hex (apksigner format); colon
        # separators / upper case (keytool format) are accepted too.
        want=$(tr -d '[:space:]' < "$pin" | tr -d ':' | tr 'A-F' 'a-f')
        [ "$sha" = "$want" ] \
            || fail "release cert SHA-256 $sha != pinned $want ($pin)"
        ok "release cert SHA-256 matches the pinned fingerprint"
    else
        echo "⚠️  smoke: no pinned cert $pin (REL-1) — skipping the fingerprint pin; release cert SHA-256 is $sha"
    fi
fi

# ── 3. 16 KB page alignment (S10 / R11) ─────────────────────────────────
zipalign -c -P 16 -v 4 "$apk" >/dev/null 2>&1 \
    || fail "zipalign -c -P 16 -v 4 — the APK is not 16 KB-page aligned"
ok "zipalign -c -P 16 -v 4 (16 KB pages)"

# ── 4. merged manifest: NSC reference + no backup ───────────────────────
manifest=$(aapt2 dump xmltree --file AndroidManifest.xml "$apk") \
    || fail "aapt2 dump xmltree AndroidManifest.xml"
printf '%s\n' "$manifest" | grep -qF 'networkSecurityConfig' \
    || fail "the merged manifest has no android:networkSecurityConfig"
ok "manifest networkSecurityConfig"
# xmltree renders the boolean as `A: …:allowBackup(0x01010280)=false`; the
# expected value is allowBackup="false" (allowBackup="true" would let the
# cloud back up the token/config).
allow_backup=$(printf '%s\n' "$manifest" | grep -F 'allowBackup' | sed 's/.*allowBackup(0x[0-9a-f]*)=//')
[ "$allow_backup" = "false" ] \
    || fail "the merged manifest does not have allowBackup=\"false\" (got '${allow_backup:-<none>}')"
ok 'manifest allowBackup="false"'

# ── 5. the NSC resource: user CA anchors + 127.0.0.1 cleartext ──────────
# Release resource shrinking renames the file (debug: res/xml/
# network_security_config.xml, release: res/8G.xml) — look it up by
# resource name, not by path.
nsc_file=$(aapt2 dump resources "$apk" | awk '/xml\/network_security_config/{f=1} f && /\(file\) res\//{print $3; exit}')
[ -n "$nsc_file" ] || fail "no xml/network_security_config resource in the APK"
nsc=$(aapt2 dump xmltree --file "$nsc_file" "$apk") \
    || fail "aapt2 dump xmltree $nsc_file"
# The user-installed PeakBot CA is trusted: <certificates src="user" />
# renders as `A: src(…)=(Raw: "user")` in the xmltree dump.
printf '%s\n' "$nsc" | grep -F 'src' | grep -qF '(Raw: "user")' \
    || fail 'the NSC has no src="user" trust anchor'
ok 'NSC src="user"'
printf '%s\n' "$nsc" | grep -qF '127.0.0.1' \
    || fail "the NSC has no 127.0.0.1 domain-config (Local cleartext)"
ok "NSC 127.0.0.1 domain-config"

# ── 6. size cap (release only) ───────────────────────────────────────────
# 40 MB = 41943040 bytes (plan §3.1; the spike measured 22.1 MB and the
# core link is ~2× the remote-only build). Debug APKs are never released
# and carry unstripped symbols, so they skip the release cap.
MAX_BYTES=41943040
size=$(wc -c < "$apk" | tr -d '[:space:]')
if [ "$kind" = release ]; then
    [ "$size" -lt "$MAX_BYTES" ] \
        || fail "APK size $size bytes >= the 40 MB cap ($MAX_BYTES bytes)"
    ok "size $size bytes < 40 MB"
else
    echo "ℹ️  smoke: debug build — the 40 MB release size cap does not apply (debug is never released)"
fi

# ── L10 (Phase 3, plan §4.3 test 6): host-only symbol leak check ─────────
# The unstripped mobile lib must not link host-only code (the shell/PTY,
# BashTool, the ratatui TUI). Rust internals are mostly not exported as
# dynamic symbols, so `llvm-nm -D` alone isn't enough — we also scan
# `strings`, where type names and panic paths survive. Either scan catching
# one of the four names fails the build. The NDK's llvm is not on PATH by
# default, so its prebuilt bin is prepended and `llvm-nm -D` is the exact
# tool the APK was linked with.
run_l10_symbol_check() {
    # Optional second arg: an explicit binary to scan (used to prove the
    # check can catch a leak). When absent, locate the unstripped mobile lib.
    so="${2:-}"

    # NDK's llvm prebuilt bin (toolchains/llvm/prebuilt/linux-x86_64/bin),
    # not on PATH by default; prepend it so `llvm-nm -D` resolves to the
    # linker tool the APK was actually built with.
    if [ -n "${NDK_HOME:-}" ] \
        && [ -x "${NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm" ]; then
        export PATH="${NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH"
    fi
    command -v llvm-nm >/dev/null 2>&1 \
        || fail "L10: llvm-nm not found (NDK llvm toolchain missing?)"

    # Locate the unstripped lib the APK links (when not given explicitly). The
    # debug build is unstripped (release is stripped by
    # CARGO_PROFILE_RELEASE_STRIP=true). Prefer the cargo target tree; fall
    # back to a find over the build dirs.
    if [ -z "$so" ]; then
        for base in "$root/target" "${CARGO_TARGET_DIR:-$root/target}" "$root/mobile/src-tauri/target"; do
            for profile in debug release; do
                [ -f "$base/aarch64-linux-android/$profile/libpeakbot_mobile_lib.so" ] \
                    && so="$base/aarch64-linux-android/$profile/libpeakbot_mobile_lib.so"
                [ -n "$so" ] && break
            done
            [ -n "$so" ] && break
        done
        [ -n "$so" ] || so=$(find "$root" -type f -name 'libpeakbot_mobile_lib.so' 2>/dev/null | head -n 1)
    fi
    [ -n "$so" ] || fail "L10: no libpeakbot_mobile_lib.so to symbol-check (build incomplete?)"

    # Checked names (host-only code that must not be in the mobile build).
    bad=""
    for sym in portable_pty TokioChildProcess BashTool ratatui; do
        if llvm-nm -D "$so" 2>/dev/null | grep -Fq -- "$sym" \
            || strings "$so" 2>/dev/null | grep -Fq -- "$sym"; then
            bad="$sym"
            break
        fi
    done
    if [ -n "$bad" ]; then
        fail "L10: host-only symbol '$bad' found in $so — host code leaked into the mobile build"
    fi
    ok "L10: no host-only symbols (portable_pty, TokioChildProcess, BashTool, ratatui) in $so"
}
run_l10_symbol_check "$apk"

echo "✅ smoke: all checks passed — $apk ($kind)"
exit 0
