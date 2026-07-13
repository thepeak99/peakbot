#!/usr/bin/env bash
#
# Mirror Gitea releases + assets to the GitHub mirror.
#
# Gitea does not copy releases or their binary artifacts to a mirrored
# GitHub repo (only git refs are mirrored). This script closes that gap:
# for every Gitea release it creates the matching GitHub release (title,
# body, draft/prerelease flags) and uploads every asset.
#
# It is IDEMPOTENT — safe to re-run any time:
#   * a GitHub release that already exists is reused (body/title synced)
#   * an asset already present on the GitHub release (matched by name) is
#     skipped, so re-runs only fetch+upload what is missing.
#
# Requirements: bash, curl, jq, and gh (authenticated for the target repo).
#
# Env / config (auto-derived where possible):
#   GITEA_URL     Gitea base URL           (default: https://git.patchnotes.com)
#   GITEA_TOKEN   Gitea API token          (REQUIRED — assets are auth-gated)
#   GITEA_REPO    <owner>/<repo> on Gitea  (default: ai-bots/peakbot)
#   GITHUB_REPO   <owner>/<repo> on GitHub (default: thepeak99/peakbot)
#   DRY_RUN       set to 1 to print actions without mutating GitHub
#
set -euo pipefail

GITEA_URL="${GITEA_URL:-https://git.patchnotes.com}"
GITEA_REPO="${GITEA_REPO:-ai-bots/peakbot}"
GITHUB_REPO="${GITHUB_REPO:-thepeak99/peakbot}"
DRY_RUN="${DRY_RUN:-0}"

die() { echo "❌ $*" >&2; exit 1; }
info() { echo "▶ $*"; }

command -v jq  >/dev/null || die "jq not found"
command -v gh  >/dev/null || die "gh not found"
command -v curl >/dev/null || die "curl not found"
[ -n "${GITEA_TOKEN:-}" ] || die "GITEA_TOKEN is required (assets are auth-gated on Gitea)"
gh auth status >/dev/null 2>&1 || die "gh is not authenticated (run: gh auth login)"

API="$GITEA_URL/api/v1/repos/$GITEA_REPO"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

info "Fetching Gitea releases from $GITEA_REPO ..."
RELEASES_JSON="$WORKDIR/releases.json"
curl -fsSL -H "Authorization: token $GITEA_TOKEN" \
  "$API/releases?limit=100" >"$RELEASES_JSON" \
  || die "failed to list Gitea releases"

count="$(jq 'length' "$RELEASES_JSON")"
[ "$count" -gt 0 ] || { info "No releases found. Nothing to do."; exit 0; }
info "Found $count Gitea release(s). Mirroring oldest→newest into $GITHUB_REPO"

# Oldest→newest so GitHub's "Latest" ends up on the newest tag.
for idx in $(jq -r 'to_entries | sort_by(.value.created_at) | .[].key' "$RELEASES_JSON"); do
  rel="$(jq -c ".[$idx]" "$RELEASES_JSON")"
  tag="$(jq -r '.tag_name'   <<<"$rel")"
  name="$(jq -r '.name // .tag_name' <<<"$rel")"
  draft="$(jq -r '.draft'       <<<"$rel")"
  prerelease="$(jq -r '.prerelease' <<<"$rel")"

  echo
  info "── Release $tag  (draft=$draft prerelease=$prerelease) ──"

  notes_file="$WORKDIR/notes-$tag.md"
  jq -r '.body // ""' <<<"$rel" >"$notes_file"

  flags=()
  [ "$draft" = "true" ]      && flags+=(--draft)
  [ "$prerelease" = "true" ] && flags+=(--prerelease)

  # Ensure the GitHub release exists (idempotent).
  if gh release view "$tag" --repo "$GITHUB_REPO" >/dev/null 2>&1; then
    info "  GitHub release exists — syncing title/notes"
    if [ "$DRY_RUN" = "1" ]; then
      echo "    [dry-run] gh release edit $tag --title '$name' --notes-file ..."
    else
      gh release edit "$tag" --repo "$GITHUB_REPO" \
        --title "$name" --notes-file "$notes_file" >/dev/null
    fi
  else
    info "  Creating GitHub release"
    if [ "$DRY_RUN" = "1" ]; then
      echo "    [dry-run] gh release create $tag --title '$name' --notes-file ... ${flags[*]:-}"
    else
      gh release create "$tag" --repo "$GITHUB_REPO" \
        --title "$name" --notes-file "$notes_file" "${flags[@]}" >/dev/null
    fi
  fi

  # Assets already on the GitHub release (skip these).
  existing_assets=""
  if [ "$DRY_RUN" != "1" ]; then
    existing_assets="$(gh release view "$tag" --repo "$GITHUB_REPO" \
      --json assets -q '.assets[].name' 2>/dev/null || true)"
  fi

  # Upload each Gitea asset that is missing on GitHub.
  jq -c '.assets[]?' <<<"$rel" | while read -r asset; do
    a_name="$(jq -r '.name' <<<"$asset")"
    a_url="$(jq -r '.browser_download_url' <<<"$asset")"

    if grep -Fxq "$a_name" <<<"$existing_assets"; then
      info "    ✓ asset present: $a_name (skip)"
      continue
    fi

    info "    ↑ asset: $a_name"
    if [ "$DRY_RUN" = "1" ]; then
      echo "      [dry-run] download $a_url → upload to $tag"
      continue
    fi

    local_path="$WORKDIR/$a_name"
    curl -fsSL -H "Authorization: token $GITEA_TOKEN" "$a_url" -o "$local_path" \
      || die "failed to download $a_name from Gitea"
    gh release upload "$tag" "$local_path" --repo "$GITHUB_REPO" --clobber >/dev/null \
      || die "failed to upload $a_name to GitHub"
    rm -f "$local_path"
  done
done

echo
info "✅ Done. GitHub releases for $GITHUB_REPO are in sync with $GITEA_REPO."
