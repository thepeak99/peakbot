#!/bin/sh
#
# build-android.sh — PeakBot Mobile Android APK build (M7).
#
# The single entry point for `make mobile-android` and CI. Runs INSIDE the
# Dockerfile.mobile-android toolchain container with the worktree mounted at
# /w. Plan §3.1: `npm run tauri --` (D6 — a global CLI breaks the gradle
# callback), the four CARGO_PROFILE_RELEASE_* size env vars, `--locked`
# (D8 — the root workspace lock is the only lock), the D7 named volume for
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
#      absent → debug build (AGP's default debug keystore from the D7
#      /root/.android volume) → output/mobile/PeakBot-Mobile-<v>-android-
#      arm64-debug.apk — a debug APK is never released
#   5. smoke-android.sh (M8) on the result — fails the build on any violation
#
# The secrets live only in the tmpfs /run/secrets mount; they are never
# echoed and never land in the worktree.

set -eu

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

# 1. The Local core embeds the SPA from web/dist, so it must match web/src:
#    ALWAYS build it (a "build if absent" check silently shipped a stale SPA).
#    This is the `make web` equivalent; restore the tracked .gitkeep Vite wipes.
echo "🕸️  building the SPA (make web equivalent)..."
(cd "$root/web" && npm ci --no-audit --no-fund && npm run build)
touch "$root/web/dist/.gitkeep"

# 2. D6: the CLI is a devDependency in mobile/package.json — always go
#    through `npm run tauri --`, never a global tauri.
(cd "$root/mobile" && npm ci --no-audit --no-fund)

# Release size settings come from env (plan §3.1): the workspace profiles
# live at the root and must not change the CLI build.
export CARGO_PROFILE_RELEASE_OPT_LEVEL=s
export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_STRIP=true
# The container is memory-capped by the Makefile; keep the parallelism in
# step with it (a cold LTO release build at full nproc OOMs the container).
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-8}

# 3+4. Signing: the four BuildKit secrets arrive on the tmpfs /run/secrets
#      mount (the Makefile writes them there — never argv, never env of the
#      build container). Decode the keystore and write keystore.properties
#      to the same tmpfs; the committed gradle signingConfig
#      (gen/android/app/build.gradle.kts) reads it via
#      ANDROID_KEYSTORE_PROPERTIES. No secrets ⇒ the debug build.
secrets_dir=/run/secrets
have_secrets=0
if [ -f "$secrets_dir/ANDROID_KEYSTORE_B64" ] \
    && [ -f "$secrets_dir/ANDROID_KEYSTORE_PASSWORD" ] \
    && [ -f "$secrets_dir/ANDROID_KEY_ALIAS" ] \
    && [ -f "$secrets_dir/ANDROID_KEY_PASSWORD" ]; then
    have_secrets=1
fi

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
    tauri_build_args="--apk --target aarch64 --ci"
    out_name="PeakBot-Mobile-$VERSION-android-arm64.apk"
else
    kind=debug
    tauri_build_args="--apk --debug --target aarch64 --ci"
    out_name="PeakBot-Mobile-$VERSION-android-arm64-debug.apk"
fi

echo "🔨 tauri android build ($kind): npm run tauri -- android build $tauri_build_args -- --locked"
cd "$root/mobile"
# `-- --locked`: the extra args after `--` are passed to the cargo runner
# (cargo-ndk), so the build uses the root workspace lock (D8).
npm run tauri -- android build $tauri_build_args -- --locked

# Tauri 2.12 lays the APK out at outputs/apk/universal/<kind>/app-universal-
# <kind>.apk (older releases: outputs/apk/<kind>/app-<kind>.apk) — take the
# first match of either layout.
apk_glob="$root/mobile/src-tauri/gen/android/app/build/outputs/apk"
apk_src=$(ls "$apk_glob"/*/"$kind"/app*.apk "$apk_glob"/"$kind"/app*.apk 2>/dev/null | head -n 1)
[ -n "$apk_src" ] && [ -f "$apk_src" ] \
    || { echo "❌ no APK found under $apk_glob for the $kind build" >&2; exit 1; }

# 5. Deliverable + smoke (M8): any violation fails the build.
mkdir -p "$root/output/mobile"
cp "$apk_src" "$root/output/mobile/$out_name"
ls -lh "$root/output/mobile/$out_name"
sh "$root/mobile/scripts/smoke-android.sh" "$root/output/mobile/$out_name"
echo "✅ $root/output/mobile/$out_name ($kind, smoked)"
