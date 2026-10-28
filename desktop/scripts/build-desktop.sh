#!/bin/sh
#
# build-desktop.sh <linux|macos|windows|all>
#
# Single entry point for desktop artifact builds — used by the Makefile AND
# CI (desktop/DESIGN.md §5.7). Needs only sh + a container builder.
#
# Linux pipeline:
#   1. build the peakbot sidecar with the EXISTING Dockerfile.linux
#      (unversioned artifact: output/peakbot-linux-amd64)
#   2. build the AppImage with Dockerfile.desktop-linux, exporting
#      output/desktop/PeakBot-Desktop-<v>-linux-x86_64.AppImage
#      (the Dockerfile runs the S-L1..S-L7 smoke checks before exporting)
#
# macOS pipeline (cross-built on Linux — no Mac involved):
#   1. build the peakbot sidecar with the EXISTING Dockerfile.macos
#      (unversioned artifact: output/peakbot-macos-universal2)
#   2. build the universal .app zip + .app.tar.gz with
#      Dockerfile.desktop-macos (zigbuild universal2 + hand-assembled
#      bundle + rcodesign ad-hoc signing), exporting
#      output/desktop/PeakBot-Desktop-<v>-macos-universal.zip
#      + output/desktop/PeakBot-Desktop-<v>-macos-universal.app.tar.gz
#      (the Dockerfile runs the S-M1..S-M6 smoke checks before exporting)
#
# Windows pipeline (cross-built on Linux — no Windows involved):
#   1. build the peakbot sidecar with the EXISTING Dockerfile.windows
#      (TARGET=x86_64-pc-windows-msvc, cargo-xwin + MSVC SDK, static CRT
#      → unversioned artifact: output/peakbot-windows-amd64-msvc.exe)
#   2. build BOTH Windows artifacts with Dockerfile.desktop-windows
#      (cargo-xwin portable exe + NSIS installer via the pinned Tauri CLI;
#      the sidecar is embedded as a zstd payload)
#      → output/desktop/PeakBot-Desktop-<v>-windows-x86_64-portable.exe
#      + output/desktop/PeakBot-Desktop-<v>-windows-x86_64-setup.exe
#      (S-W1..S-W6 run before export)
#
# Updater pipeline (signing + manifest — no sidecar involved):
#   signs the three update artifacts already in output/desktop/
#   (AppImage, Windows setup.exe, macOS .app.tar.gz) with the Tauri
#   signing key (BuildKit secret), verifies each signature with minisign
#   against the pubkey in desktop/src-tauri/tauri.conf.json, builds
#   latest.json (updater-manifest.sh)
#   → output/desktop/*.sig (3) + output/desktop/latest.json
#
#   Requires TAURI_SIGNING_PRIVATE_KEY + TAURI_SIGNING_PRIVATE_KEY_PASSWORD
#   in the environment (the key never lands in an image layer or log).
#
# Environment:
#   CONTAINER_BUILDER  podman (default) or docker
#   PRUNE=1            run '<builder> builder prune -af' between builds (CI disk)
#   GH_MIRROR_REPO     GitHub mirror owner/repo for latest.json URLs
#                      (default: thepeak99/peakbot)

set -eu

case "${1:-}" in
    linux|macos|windows|all) ;;
    updater) ;;
    *) echo "usage: build-desktop.sh <linux|macos|windows|all> (or: updater)" >&2; exit 2 ;;
esac

# Resolve the repo root from this script's location (works from any cwd).
script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)
cd "$root"

# Version: same extraction the Makefile's release-bump uses.
VERSION=$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' Cargo.toml)
[ -n "$VERSION" ] || { echo "❌ could not read version from Cargo.toml" >&2; exit 1; }

CONTAINER_BUILDER=${CONTAINER_BUILDER:-$(command -v podman 2>/dev/null || echo docker)}

# `docker build --output type=local,dest=...` requires BuildKit. podman
# (buildah) supports it natively; for docker, force BuildKit on so the
# invocation works on daemons where the classic builder is still default.
if [ "$CONTAINER_BUILDER" = "docker" ]; then
    export DOCKER_BUILDKIT=${DOCKER_BUILDKIT:-1}
fi

echo "🖥️  PeakBot Desktop $VERSION — builder: $CONTAINER_BUILDER"

prune_between() {
    if [ "${PRUNE:-}" = "1" ]; then
        echo "🧹 PRUNE=1 — pruning builder cache..."
        "$CONTAINER_BUILDER" builder prune -af
    fi
}

build_sidecar_linux() {
    echo "🐧 Building peakbot sidecar (Dockerfile.linux)..."
    mkdir -p output
    "$CONTAINER_BUILDER" build \
        --output type=local,dest=output \
        -f Dockerfile.linux \
        .
    chmod +x output/peakbot-linux-amd64
    echo "✅ $(ls -lh output/peakbot-linux-amd64)"
}

build_desktop_linux() {
    [ -f output/peakbot-linux-amd64 ] \
        || { echo "❌ missing sidecar output/peakbot-linux-amd64 — build it first" >&2; exit 1; }
    echo "🖥️  Building Linux AppImage (Dockerfile.desktop-linux)..."
    mkdir -p output/desktop
    "$CONTAINER_BUILDER" build \
        --build-arg VERSION="$VERSION" \
        --output type=local,dest=output/desktop \
        -f Dockerfile.desktop-linux \
        .
    chmod +x "output/desktop/PeakBot-Desktop-$VERSION-linux-x86_64.AppImage"
    echo "✅ $(ls -lh "output/desktop/PeakBot-Desktop-$VERSION-linux-x86_64.AppImage")"
}

build_desktop_macos() {
    [ -f output/peakbot-macos-universal2 ] \
        || { echo "❌ missing sidecar output/peakbot-macos-universal2 — build it first" >&2; exit 1; }
    echo "🖥️  Building macOS universal .app (Dockerfile.desktop-macos)..."
    mkdir -p output/desktop
    "$CONTAINER_BUILDER" build \
        --build-arg VERSION="$VERSION" \
        --output type=local,dest=output/desktop \
        -f Dockerfile.desktop-macos \
        .
    [ -f "output/desktop/PeakBot-Desktop-$VERSION-macos-universal.zip" ] \
        || { echo "❌ expected output/desktop/PeakBot-Desktop-$VERSION-macos-universal.zip" >&2; exit 1; }
    [ -f "output/desktop/PeakBot-Desktop-$VERSION-macos-universal.app.tar.gz" ] \
        || { echo "❌ expected output/desktop/PeakBot-Desktop-$VERSION-macos-universal.app.tar.gz" >&2; exit 1; }
    echo "✅ $(ls -lh "output/desktop/PeakBot-Desktop-$VERSION-macos-universal.zip")"
    echo "✅ $(ls -lh "output/desktop/PeakBot-Desktop-$VERSION-macos-universal.app.tar.gz")"
}

build_sidecar_macos() {
    echo "🍎 Building peakbot sidecar (Dockerfile.macos)..."
    mkdir -p output
    "$CONTAINER_BUILDER" build \
        --output type=local,dest=output \
        -f Dockerfile.macos \
        .
    chmod +x output/peakbot-macos-universal2
    echo "✅ $(ls -lh output/peakbot-macos-universal2)"
}

build_sidecar_windows() {
    echo "🪟 Building peakbot sidecar (Dockerfile.windows, msvc/cargo-xwin)..."
    mkdir -p output
    "$CONTAINER_BUILDER" build \
        --build-arg TARGET=x86_64-pc-windows-msvc \
        --output type=local,dest=output \
        -f Dockerfile.windows \
        .
    echo "✅ $(ls -lh output/peakbot-windows-amd64-msvc.exe)"
}

build_desktop_windows() {
    [ -f output/peakbot-windows-amd64-msvc.exe ] \
        || { echo "❌ missing sidecar output/peakbot-windows-amd64-msvc.exe — build it first" >&2; exit 1; }
    echo "🖥️  Building Windows portable exe + NSIS installer (Dockerfile.desktop-windows)..."
    mkdir -p output/desktop
    "$CONTAINER_BUILDER" build \
        --build-arg VERSION="$VERSION" \
        --output type=local,dest=output/desktop \
        -f Dockerfile.desktop-windows \
        .
    [ -f "output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64-portable.exe" ] \
        || { echo "❌ expected output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64-portable.exe" >&2; exit 1; }
    [ -f "output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64-setup.exe" ] \
        || { echo "❌ expected output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64-setup.exe" >&2; exit 1; }
    echo "✅ $(ls -lh "output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64-portable.exe" "output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64-setup.exe")"
}

build_linux() {
    build_sidecar_linux
    prune_between
    build_desktop_linux
}

build_macos() {
    build_sidecar_macos
    prune_between
    build_desktop_macos
}

build_windows() {
    build_sidecar_windows
    prune_between
    build_desktop_windows
}

build_updater() {
    # The three update artifacts must exist (built by the desktop pipelines).
    for n in "PeakBot-Desktop-$VERSION-linux-x86_64.AppImage" \
             "PeakBot-Desktop-$VERSION-windows-x86_64-setup.exe" \
             "PeakBot-Desktop-$VERSION-macos-universal.app.tar.gz"; do
        [ -f "output/desktop/$n" ] \
            || { echo "❌ missing updater artifact output/desktop/$n — build the desktop artifacts first" >&2; exit 1; }
    done
    [ -f desktop/src-tauri/tauri.conf.json ] \
        || { echo "❌ missing desktop/src-tauri/tauri.conf.json (pubkey source)" >&2; exit 1; }
    [ -f "release-notes/$VERSION.md" ] \
        || { echo "❌ missing release notes: release-notes/$VERSION.md" >&2; exit 1; }
    [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] \
        || { echo "❌ TAURI_SIGNING_PRIVATE_KEY is empty — export the Tauri signing private key" >&2; exit 1; }
    [ -n "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ] \
        || { echo "❌ TAURI_SIGNING_PRIVATE_KEY_PASSWORD is empty — export the key password" >&2; exit 1; }

    echo "🔏 Signing updater artifacts + building latest.json (Dockerfile.desktop-updater)..."
    mkdir -p output/desktop
    # The key + password cross the build boundary as BuildKit secrets. Both
    # podman and docker take --secret id=,src=<file>; the temp files are
    # chmod 600 and deleted after the build (they never land in a layer —
    # the Dockerfile keeps the key only in /run/secrets + a same-RUN temp).
    keyfile=$(mktemp); pwfile=$(mktemp)
    chmod 600 "$keyfile" "$pwfile"
    trap 'rm -f "$keyfile" "$pwfile"' EXIT
    printf '%s' "$TAURI_SIGNING_PRIVATE_KEY" > "$keyfile"
    printf '%s' "$TAURI_SIGNING_PRIVATE_KEY_PASSWORD" > "$pwfile"
    "$CONTAINER_BUILDER" build \
        --build-arg VERSION="$VERSION" \
        --build-arg GH_MIRROR_REPO="${GH_MIRROR_REPO:-thepeak99/peakbot}" \
        --build-arg NOTES="release-notes/$VERSION.md" \
        --secret id=tauri_key,src="$keyfile" \
        --secret id=tauri_key_pw,src="$pwfile" \
        --output type=local,dest=output/desktop \
        -f Dockerfile.desktop-updater \
        .
    rm -f "$keyfile" "$pwfile"
    trap - EXIT
    for n in "PeakBot-Desktop-$VERSION-linux-x86_64.AppImage.sig" \
             "PeakBot-Desktop-$VERSION-windows-x86_64-setup.exe.sig" \
             "PeakBot-Desktop-$VERSION-macos-universal.app.tar.gz.sig" \
             "latest.json"; do
        [ -f "output/desktop/$n" ] \
            || { echo "❌ expected output/desktop/$n after the updater build" >&2; exit 1; }
    done
    echo "✅ $(ls -lh "output/desktop"/*.sig "output/desktop/latest.json" | tr '\n' ' ')"
}

case "$1" in
    linux)   build_linux ;;
    macos)   build_macos ;;
    windows) build_windows ;;
    updater) build_updater ;;
    all)     build_linux; prune_between; build_macos; prune_between; build_windows ;;
esac
