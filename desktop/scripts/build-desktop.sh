#!/bin/sh
#
# build-desktop.sh <linux|macos|all>
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
# Environment:
#   CONTAINER_BUILDER  podman (default) or docker
#   PRUNE=1            run '<builder> builder prune -af' between builds (CI disk)

set -eu

case "${1:-}" in
    linux|macos|all) ;;
    *) echo "usage: build-desktop.sh <linux|macos|all>" >&2; exit 2 ;;
esac

# Resolve the repo root from this script's location (works from any cwd).
script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)
cd "$root"

# Version: same extraction the Makefile's release-bump uses.
VERSION=$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$3); print $3; exit}' Cargo.toml)
[ -n "$VERSION" ] || { echo "❌ could not read version from Cargo.toml" >&2; exit 1; }

CONTAINER_BUILDER=${CONTAINER_BUILDER:-$(command -v podman 2>/dev/null || echo docker)}

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
    # TODO(P4.2): Dockerfile.desktop-macos (zigbuild universal2) +
    # macos/assemble-app.sh + scripts/smoke-macos.sh. Separate task.
    echo "❌ macOS desktop build not implemented yet (task P4.2)" >&2
    exit 1
}

build_linux() {
    build_sidecar_linux
    prune_between
    build_desktop_linux
}

case "$1" in
    linux) build_linux ;;
    macos) build_desktop_macos ;;
    all)   build_linux; prune_between; build_desktop_macos ;;
esac
