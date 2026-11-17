#!/bin/sh
#
# build-android.sh [apk|aab] — PeakBot Mobile Android APK / AAB build.
#
# Format (first argument, default apk; `make mobile-android` passes
# $(ANDROID_FORMAT)):
#   apk  release APK when the secrets are present, else the debug APK.
#   aab  Google Play bundle. REQUIRES all four secrets (an unsigned/debug
#        AAB cannot go to Play): exits 1 BEFORE npm ci when one is missing.
#        One gradle run (`--aab --apk`) yields the release-signed
#        output/mobile/PeakBot-Mobile-<v>-android-arm64.aab (checked with
#        jarsigner -verify) plus the sibling release APK, which goes
#        through smoke-android.sh.
# Every mode injects the Play versionCode from scripts/play-version-code.sh
# through Tauri's `--config` merge (no committed state, gradle untouched).
#
# The single entry point for `make mobile-android` and CI. Runs INSIDE the
# Dockerfile.mobile-android toolchain container with the worktree mounted at
# /w. `npm run tauri --` (a global CLI breaks the gradle
# callback), the four CARGO_PROFILE_RELEASE_* size env vars, `--locked`
# (the root workspace lock is the only lock), the named volume for
# /root/.android (mounted by the Makefile, so debug APKs upgrade over each
# other), and release signing via BuildKit secrets → tmpfs keystore.properties
# → the committed gradle signingConfig.
#
# Steps:
#   1. web/dist present — the mobile app bundles only mobile/ui, but the
#      Local core embeds the SPA from web/dist; build it (the `make web`
#      equivalent) when absent
#   2. cd mobile && npm ci (@tauri-apps/cli 2.12.4 devDependency)
#   3. npm run tauri -- android build --apk --target aarch64 --ci -- --locked
#   4. /run/secrets/ANDROID_KEYSTORE_B64 + ANDROID_KEYSTORE_PASSWORD +
#      ANDROID_KEY_ALIAS + ANDROID_KEY_PASSWORD present (tmpfs, written by
#      the Makefile) → keystore.jks + keystore.properties on the same tmpfs
#      → release build → output/mobile/PeakBot-Mobile-<v>-android-arm64.apk
#      absent → debug build (AGP's default debug keystore from the
#      /root/.android volume) → output/mobile/PeakBot-Mobile-<v>-android-
#      arm64-debug.apk — a debug APK is never released
#   5. smoke-android.sh on the result — fails the build on any violation
#
# The secrets live only in the tmpfs /run/secrets mount; they are never
# echoed and never land in the worktree.

set -eu

format=${1:-apk}
case "$format" in
    apk|aab) ;;
    *) echo "usage: build-android.sh [apk|aab]" >&2; exit 2 ;;
esac

root=/w
[ -d "$root/mobile" ] \
    || { echo "❌ $root/mobile not found — mount the worktree at /w" >&2; exit 1; }

# Version: mobile/src-tauri/tauri.conf.json (the single source of truth;
# the Makefile's release-bump keeps it in sync with the root Cargo.toml).
VERSION=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$root/mobile/src-tauri/tauri.conf.json" | head -n 1)
[ -n "$VERSION" ] \
    || { echo "❌ could not read version from mobile/src-tauri/tauri.conf.json" >&2; exit 1; }
echo "📱 PeakBot Mobile $VERSION (Android arm64)"

# Signing: the four BuildKit secrets arrive on the tmpfs /run/secrets
#      mount (the Makefile writes them there — never argv, never env of the
#      build container). Decode the keystore and write keystore.properties
#      to the same tmpfs; the committed gradle signingConfig
#      (gen/android/app/build.gradle.kts) reads it via
#      ANDROID_KEYSTORE_PROPERTIES. No secrets ⇒ the debug build
#      (apk only; aab has no debug fallback).
secrets_dir=/run/secrets
have_secrets=0
if [ -f "$secrets_dir/ANDROID_KEYSTORE_B64" ] \
    && [ -f "$secrets_dir/ANDROID_KEYSTORE_PASSWORD" ] \
    && [ -f "$secrets_dir/ANDROID_KEY_ALIAS" ] \
    && [ -f "$secrets_dir/ANDROID_KEY_PASSWORD" ]; then
    have_secrets=1
fi

# aab never falls back to debug: fail BEFORE the slow steps (npm ci, build).
if [ "$format" = aab ] && [ "$have_secrets" = 0 ]; then
    echo "❌ aab needs the ANDROID_KEYSTORE_* secrets (an unsigned/debug AAB cannot go to Play)" >&2
    exit 1
fi

# Play versionCode: a pure function of the version (scripts/play-version-code.sh,
# the single source of truth), injected via Tauri's --config merge in every
# mode so each build of a version carries the same code.
version_code=$(sh "$root/scripts/play-version-code.sh" "$VERSION") \
    || { echo "❌ no Play versionCode for version '$VERSION'" >&2; exit 1; }
cfg='{"bundle":{"android":{"versionCode":'"$version_code"'}}}'

# 1. The Local core embeds the SPA from web/dist, so it must match web/src:
#    ALWAYS build it (a "build if absent" check silently shipped a stale SPA).
#    This is the `make web` equivalent; restore the tracked .gitkeep Vite wipes.
echo "🕸️  building the SPA (make web equivalent)..."
(cd "$root/web" && npm ci --no-audit --no-fund && npm run build)
touch "$root/web/dist/.gitkeep"

# 2. The CLI is a devDependency in mobile/package.json — always go
#    through `npm run tauri --`, never a global tauri.
(cd "$root/mobile" && npm ci --no-audit --no-fund)

# Release size settings come from env: the workspace profiles
# live at the root and must not change the CLI build.
export CARGO_PROFILE_RELEASE_OPT_LEVEL=s
export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_STRIP=true
# The container is memory-capped by the Makefile; keep the parallelism in
# step with it (a cold LTO release build at full nproc OOMs the container).
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-8}

if [ "$have_secrets" = 1 ]; then
    trap 'rm -f "$secrets_dir/keystore.jks" "$secrets_dir/keystore.properties"' EXIT
    base64 -d < "$secrets_dir/ANDROID_KEYSTORE_B64" > "$secrets_dir/keystore.jks"
    {
        printf 'storePassword=%s\n' "$(cat "$secrets_dir/ANDROID_KEYSTORE_PASSWORD")"
        printf 'keyPassword=%s\n' "$(cat "$secrets_dir/ANDROID_KEY_PASSWORD")"
        printf 'keyAlias=%s\n' "$(cat "$secrets_dir/ANDROID_KEY_ALIAS")"
        printf 'storeFile=%s\n' "$secrets_dir/keystore.jks"
    } > "$secrets_dir/keystore.properties"
    chmod 600 "$secrets_dir/keystore.jks" "$secrets_dir/keystore.properties"
    export ANDROID_KEYSTORE_PROPERTIES="$secrets_dir/keystore.properties"
    kind=release
    out_name="PeakBot-Mobile-$VERSION-android-arm64.apk"
else
    kind=debug
    out_name="PeakBot-Mobile-$VERSION-android-arm64-debug.apk"
fi

if [ "$format" = aab ]; then
    set -- --aab --apk --target aarch64 --ci
elif [ "$kind" = release ]; then
    set -- --apk --target aarch64 --ci
else
    set -- --apk --debug --target aarch64 --ci
fi

echo "🔨 tauri android build ($kind, $format, versionCode $version_code): npm run tauri -- android build $* --config <versionCode> -- --locked"
cd "$root/mobile"
# `-- --locked`: the extra args after `--` are passed to the cargo runner
# (cargo-ndk), so the build uses the root workspace lock.
npm run tauri -- android build "$@" --config "$cfg" -- --locked

# Tauri 2.12 lays the APK out at outputs/apk/universal/<kind>/app-universal-
# <kind>.apk (older releases: outputs/apk/<kind>/app-<kind>.apk) — take the
# first match of either layout.
apk_glob="$root/mobile/src-tauri/gen/android/app/build/outputs/apk"
apk_src=$(ls "$apk_glob"/*/"$kind"/app*.apk "$apk_glob"/"$kind"/app*.apk 2>/dev/null | head -n 1)
[ -n "$apk_src" ] && [ -f "$apk_src" ] \
    || { echo "❌ no APK found under $apk_glob for the $kind build" >&2; exit 1; }

# 5. Deliverable + smoke: any violation fails the build.
mkdir -p "$root/output/mobile"
cp "$apk_src" "$root/output/mobile/$out_name"
ls -lh "$root/output/mobile/$out_name"
sh "$root/mobile/scripts/smoke-android.sh" "$root/output/mobile/$out_name"
echo "✅ $root/output/mobile/$out_name ($kind, smoked)"

if [ "$format" = aab ]; then
    aab_glob="$root/mobile/src-tauri/gen/android/app/build/outputs/bundle"
    aab_src=""
    for f in "$aab_glob"/*Release/*.aab; do aab_src=$f; break; done
    [ -n "$aab_src" ] && [ -f "$aab_src" ] \
        || { echo "❌ no AAB found under $aab_glob/*Release" >&2; exit 1; }
    aab_out="$root/output/mobile/PeakBot-Mobile-$VERSION-android-arm64.aab"
    cp "$aab_src" "$aab_out"
    ls -lh "$aab_out"
    # An AAB is a jar: jarsigner checks the signature (the upload key's
    # cert is the one the keystore holds; Play re-signs with its app key).
    jarsigner -verify "$aab_out" \
        || { echo "❌ jarsigner -verify failed on $aab_out" >&2; exit 1; }
    echo "✅ $aab_out (release-signed, jarsigner-verified, versionCode $version_code)"
fi
