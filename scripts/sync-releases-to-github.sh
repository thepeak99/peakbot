#!/usr/bin/env bash
#
# Mirror Gitea releases + their assets to GitHub.
#
# Gitea's push-mirror copies commits and tags but NOT releases or their
# binary assets. This script closes that gap. It is idempotent: a release
# that already exists on GitHub is reused, and an asset already attached is
# skipped — so it is safe to run after every Gitea release, or as a backfill.
#
# Source of truth : Gitea REST API (needs GITEA_URL + GITEA_TOKEN)
# Destination     : GitHub, via the `gh` CLI (must be authenticated)
#
# Config via environment (all have sane defaults for this repo):
#   GITEA_URL      Gitea base URL            (required; already in env)
#   GITEA_TOKEN    Gitea API token          (required; already in env)
#   GITEA_OWNER    Gitea org/user           (default: ai-bots)
#   GITEA_REPO     Gitea repo name          (default: peakbot)
#   GH_REPO        GitHub owner/repo         (default: thepeak99/peakbot)
#   DRY_RUN        1 = print actions only, change nothing (default: 0)
#
set -euo pipefail

GITEA_URL="${GITEA_URL:?set GITEA_URL}"
GITEA_TOKEN="${GITEA_TOKEN:?set GITEA_TOKEN}"
GITEA_OWNER="${GITEA_OWNER:-ai-bots}"
GITEA_REPO="${GITEA_REPO:-peakbot}"
GH_REPO="${GH_REPO:-thepeak99/peakbot}"
DRY_RUN="${DRY_RUN:-0}"

for bin in curl jq gh; do
  command -v "$bin" >/dev/null 2>&1 || { echo "❌ missing required tool: $bin" >&2; exit 1; }
done
gh auth status >/dev/null 2>&1 || { echo "❌ gh is not authenticated (run: gh auth login)" >&2; exit 1; }

api() { curl -fsSL -H "Authorization: token $GITEA_TOKEN" "$@"; }

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

echo "→ Source : $GITEA_URL/$GITEA_OWNER/$GITEA_REPO"
echo "→ Dest   : github.com/$GH_REPO"
[ "$DRY_RUN" = "1" ] && echo "→ DRY RUN — no changes will be made"
echo

# Pull every Gitea release (paginated), skipping drafts.
releases_json="$WORKDIR/releases.json"
: > "$releases_json"
page=1
while :; do
  chunk="$(api "$GITEA_URL/api/v1/repos/$GITEA_OWNER/$GITEA_REPO/releases?page=$page&limit=50")"
  count="$(jq 'length' <<<"$chunk")"
  [ "$count" -eq 0 ] && break
  jq -c '.[] | select(.draft == false)' <<<"$chunk" >> "$releases_json"
  page=$((page + 1))
done

total="$(wc -l < "$releases_json" | tr -d ' ')"
echo "Found $total non-draft Gitea release(s)."
echo

created=0 skipped_rel=0 uploaded=0 skipped_asset=0

while IFS= read -r rel; do
  tag="$(jq -r '.tag_name' <<<"$rel")"
  name="$(jq -r '.name // .tag_name' <<<"$rel")"
  body="$(jq -r '.body // ""' <<<"$rel")"
  prerelease="$(jq -r '.prerelease' <<<"$rel")"

  echo "=== $tag ==="

  # Ensure the GitHub release exists (attaching to the mirrored tag).
  if gh release view "$tag" --repo "$GH_REPO" >/dev/null 2>&1; then
    echo "  release: exists, reusing"
    skipped_rel=$((skipped_rel + 1))
  else
    pre_flag=(); [ "$prerelease" = "true" ] && pre_flag=(--prerelease)
    if [ "$DRY_RUN" = "1" ]; then
      echo "  release: would CREATE (prerelease=$prerelease)"
    else
      printf '%s' "$body" | gh release create "$tag" \
        --repo "$GH_REPO" \
        --title "$name" \
        --verify-tag \
        --notes-file - \
        "${pre_flag[@]}"
      echo "  release: created"
    fi
    created=$((created + 1))
  fi

  # Existing GitHub asset names for this release (empty on a dry-run create).
  existing=""
  if gh release view "$tag" --repo "$GH_REPO" >/dev/null 2>&1; then
    existing="$(gh release view "$tag" --repo "$GH_REPO" --json assets \
      --jq '.assets[].name' 2>/dev/null || true)"
  fi

  # Sync each Gitea asset.
  while IFS=$'\t' read -r aname aurl; do
    [ -z "$aname" ] && continue
    if grep -qxF "$aname" <<<"$existing"; then
      echo "  asset: $aname — already present, skip"
      skipped_asset=$((skipped_asset + 1))
      continue
    fi
    if [ "$DRY_RUN" = "1" ]; then
      echo "  asset: $aname — would UPLOAD"
      uploaded=$((uploaded + 1))
      continue
    fi
    echo "  asset: $aname — downloading…"
    dest="$WORKDIR/$aname"
    api -o "$dest" "$aurl"
    echo "  asset: $aname — uploading…"
    gh release upload "$tag" "$dest" --repo "$GH_REPO"
    rm -f "$dest"
    uploaded=$((uploaded + 1))
  done < <(jq -r '.assets[]? | [.name, .browser_download_url] | @tsv' <<<"$rel")

  echo
done < "$releases_json"

echo "Done. releases created=$created reused=$skipped_rel | assets uploaded=$uploaded skipped=$skipped_asset"
