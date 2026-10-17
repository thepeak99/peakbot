#!/bin/sh
#
# fetch-emoji-font.sh — fetch the bundled CBDT emoji font for the
# AppImage's fontconfig isolation (desktop/DESIGN.md §4.12) and verify
# it by sha256. Test infrastructure for
# desktop/core/tests/fontconfig_host.rs (run via `make desktop-fonttest`).
#
# Usage:
#   fetch-emoji-font.sh <dir>           # mkdir -p <dir>; fetch files whose
#                                       # sha is missing/mismatched (curl to
#                                       # <name>.part, verify, mv into place)
#   fetch-emoji-font.sh --verify <dir>  # exit 1, naming each file, on any
#                                       # missing/mismatched file
#
# Files (googlefonts/noto-emoji, TAG below):
#   NotoColorEmoji.ttf  $BASE/NotoColorEmoji.ttf
#       sha256 72a635cb3d2f3524c51620cdde406b217204e8a6a06c6a096ff8ed4b5fd6e27b  (10673480 B)
#   OFL.txt             $BASE/LICENSE
#       sha256 6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2  (4301 B)

set -eu

TAG=v2.051
BASE="https://raw.githubusercontent.com/googlefonts/noto-emoji/$TAG/fonts"
FILES="NotoColorEmoji.ttf OFL.txt"

# "<url> <sha256>" for a known file name.
spec_for() {
    case $1 in
        NotoColorEmoji.ttf)
            echo "$BASE/NotoColorEmoji.ttf 72a635cb3d2f3524c51620cdde406b217204e8a6a06c6a096ff8ed4b5fd6e27b"
            ;;
        OFL.txt)
            echo "$BASE/LICENSE 6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2"
            ;;
        *)
            echo "fetch-emoji-font.sh: unknown file '$1'" >&2
            exit 1
            ;;
    esac
}

# sha256 of a file (first sha256sum column).
sha_of() {
    sha256sum "$1" | awk '{print $1}'
}

# Fetch one file into $dir (set by the caller): skip when the sha already
# matches; otherwise curl to <name>.part, verify the sha, mv into place.
fetch_one() {
    name=$1
    spec=$(spec_for "$name")
    url=${spec%% *}
    sha=${spec##* }
    dest="$dir/$name"

    if [ -f "$dest" ]; then
        have=$(sha_of "$dest")
        if [ "$have" = "$sha" ]; then
            echo "fetch-emoji-font.sh: OK      $name (already present, sha matches)"
            return 0
        fi
        echo "fetch-emoji-font.sh: stale   $name (sha mismatch) — refetching"
    fi

    curl -fsSL "$url" -o "$dest.part"
    have=$(sha_of "$dest.part")
    if [ "$have" != "$sha" ]; then
        rm -f "$dest.part"
        echo "fetch-emoji-font.sh: sha256 MISMATCH for $name (want $sha, got $have)" >&2
        exit 1
    fi
    mv "$dest.part" "$dest"
    echo "fetch-emoji-font.sh: OK      $name"
}

# Check one file in $dir (set by the caller): report (and fail) on
# missing/mismatched.
verify_one() {
    name=$1
    spec=$(spec_for "$name")
    sha=${spec##* }
    dest="$dir/$name"

    if [ ! -f "$dest" ]; then
        echo "fetch-emoji-font.sh: MISSING   $dest" >&2
        return 1
    fi
    have=$(sha_of "$dest")
    if [ "$have" != "$sha" ]; then
        echo "fetch-emoji-font.sh: MISMATCH  $dest (want $sha, got $have)" >&2
        return 1
    fi
    echo "fetch-emoji-font.sh: OK      $name"
}

mode=fetch
if [ "${1:-}" = "--verify" ]; then
    mode=verify
    shift
fi
dir=${1:-}
if [ -z "$dir" ]; then
    echo "usage: fetch-emoji-font.sh [--verify] <dir>" >&2
    exit 2
fi
mkdir -p "$dir"

case $mode in
fetch)
    for name in $FILES; do
        fetch_one "$name"
    done
    ;;
verify)
    rc=0
    for name in $FILES; do
        verify_one "$name" || rc=1
    done
    [ "$rc" -eq 0 ]
    ;;
esac
