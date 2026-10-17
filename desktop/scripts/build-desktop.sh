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
#   2. build the universal .app zip with Dockerfile.desktop-macos
#      (zigbuild universal2 + hand-assembled bundle + rcodesign ad-hoc
#      signing), exporting output/desktop/PeakBot-Desktop-<v>-macos-universal.zip
#      (the Dockerfile runs the S-M1..S-M6 smoke checks before exporting)
#
# Windows pipeline (cross-built on Linux — no Windows involved):
#   1. build the peakbot sidecar with the EXISTING Dockerfile.windows
#      (TARGET=x86_64-pc-windows-msvc, cargo-xwin + MSVC SDK, static CRT
#      → unversioned artifact: output/peakbot-windows-amd64-msvc.exe)
#   2. build the portable exe with Dockerfile.desktop-windows
#      (cargo-xwin, no Tauri CLI; the sidecar is embedded as a zstd
#      payload) → output/desktop/PeakBot-Desktop-<v>-windows-x86_64.exe
#      (S-W1..S-W4 run before export)
#
# Environment:
#   CONTAINER_BUILDER  podman (default) or docker
#   PRUNE=1            run '<builder> builder prune -af' between builds (CI disk)

set -eu

case "${1:-}" in
    linux|macos|windows|all) ;;
    *) echo "usage: build-desktop.sh <linux|macos|windows|all>" >&2; exit 2 ;;
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
    echo "✅ $(ls -lh "output/desktop/PeakBot-Desktop-$VERSION-macos-universal.zip")"
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
    echo "🖥️  Building Windows portable exe (Dockerfile.desktop-windows)..."
    mkdir -p output/desktop
    "$CONTAINER_BUILDER" build \
        --build-arg VERSION="$VERSION" \
        --output type=local,dest=output/desktop \
        -f Dockerfile.desktop-windows \
        .
    [ -f "output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64.exe" ] \
        || { echo "❌ expected output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64.exe" >&2; exit 1; }
    echo "✅ $(ls -lh "output/desktop/PeakBot-Desktop-$VERSION-windows-x86_64.exe")"
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

case "$1" in
    linux)   build_linux ;;
    macos)   build_macos ;;
    windows) build_windows ;;
    all)     build_linux; prune_between; build_macos; prune_between; build_windows ;;
esac
