#!/bin/sh
#
# attach-release.sh — attach built desktop artifacts to a Gitea release.
# (desktop/DESIGN.md §5.7)
#
# Called from .gitea/workflows/desktop.yml after a tag build. The release
# itself is created by `make release` (human), which may lag the tag push,
# so we poll for it.
#
# Requires: sh, curl, jq
#
# Environment (all required):
#   GITEA_URL   base URL, no trailing slash (e.g. git.patchnotes.com)
#   REPO        owner/repo (e.g. ai-bots/peakbot)
#   TAG         release tag (e.g. 0.5.0)
#   TOKEN       Gitea token with write:repository (release asset upload)
#
# Steps:
#   1. GET  /api/v1/repos/$REPO/releases/tags/$TAG
#      — 404: retry every 30 s up to 20 min; any other status: fail fast
#   2. DELETE any existing release asset with the same name (idempotent)
#   3. POST each file in <repo>/output/desktop/ to
#      /api/v1/repos/$REPO/releases/<id>/assets?name=<file>

set -eu

: "${GITEA_URL:?GITEA_URL required (no trailing slash)}"
: "${REPO:?REPO required (owner/repo)}"
: "${TAG:?TAG required}"
: "${TOKEN:?TOKEN required (Gitea token with write:repository)}"

API="$GITEA_URL/api/v1/repos/$REPO"
MAX_WAIT=1200
INTERVAL=30

# Resolve the repo root from this script's location (works from any cwd,
# host or container).
script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)
ART_DIR="$root/output/desktop"

[ -d "$ART_DIR" ] || { echo "❌ artifact dir $ART_DIR not found" >&2; exit 1; }
count=$(find "$ART_DIR" -maxdepth 1 -type f | wc -l)
[ "$count" -gt 0 ] || { echo "❌ no artifacts in $ART_DIR" >&2; exit 1; }
echo "📦 $count artifact(s) in $ART_DIR"

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

echo "⏳ waiting for release '$TAG' (up to $((MAX_WAIT / 60)) min)..."
elapsed=0
while :; do
    status=$(curl -sS -o "$tmp" -w '%{http_code}' \
        -H "Authorization: token $TOKEN" \
        "$API/releases/tags/$TAG" 2>/dev/null) || status=000
    case "$status" in
        200)
            break
            ;;
        404)
            if [ "$elapsed" -ge "$MAX_WAIT" ]; then
                echo "❌ release '$TAG' not found after $((MAX_WAIT / 60)) min" >&2
                exit 1
            fi
            echo "   not found yet — retrying in ${INTERVAL}s ($((elapsed + INTERVAL))/${MAX_WAIT}s)"
            sleep "$INTERVAL"
            elapsed=$((elapsed + INTERVAL))
            ;;
        *)
            echo "❌ unexpected HTTP $status from $API/releases/tags/$TAG:" >&2
            cat "$tmp" >&2 2>/dev/null || true
            echo >&2
            exit 1
            ;;
    esac
done

RELEASE_ID=$(jq -r '.id' "$tmp")
[ -n "$RELEASE_ID" ] && [ "$RELEASE_ID" != "null" ] \
    || { echo "❌ no release id in API response" >&2; exit 1; }
echo "✅ release '$TAG' found (id=$RELEASE_ID)"

for f in "$ART_DIR"/*; do
    [ -f "$f" ] || continue
    name=$(basename "$f")

    old_id=$(jq -r --arg n "$name" '.assets[]? | select(.name == $n) | .id' "$tmp" | head -n 1)
    if [ -n "$old_id" ] && [ "$old_id" != "null" ]; then
        echo "🗑  deleting existing asset '$name' (id=$old_id)"
        curl -fsS -X DELETE -H "Authorization: token $TOKEN" \
            "$API/releases/$RELEASE_ID/assets/$old_id" >/dev/null
    fi

    echo "⬆️  uploading '$name' ($(wc -c < "$f" | awk '{printf "%.1f MB", $1/1048576}'))"
    curl -fsS -X POST \
        -H "Authorization: token $TOKEN" \
        -H "Content-Type: application/octet-stream" \
        --data-binary "@$f" \
        "$API/releases/$RELEASE_ID/assets?name=$name" >/dev/null \
        || { echo "❌ upload failed for '$name'" >&2; exit 1; }
done

echo "✅ attached $count artifact(s) to release '$TAG'"
echo "   https://$GITEA_URL/$REPO/releases/tag/$TAG"
