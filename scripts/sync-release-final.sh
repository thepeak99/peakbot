#!/usr/bin/env bash
#
# Make GitHub releases exactly match the Gitea source of truth.
#
# The sync is convergent: it creates and updates releases, compares assets by
# SHA-256, replaces changed assets, and deletes GitHub-only assets/releases.
# Run with DRY_RUN=1 first to preview every mutation.
#
# Requirements: bash, curl, jq, sha256sum, and authenticated gh.
#
# Environment:
#   GITEA_URL    Gitea base URL            (default: https://git.patchnotes.com)
#   GITEA_REPO   Gitea owner/repo          (default: ai-bots/peakbot)
#   GITEA_TOKEN  Gitea API token           (required)
#   GITHUB_REPO  GitHub owner/repo          (default: thepeak99/peakbot)
#   DRY_RUN      1 = report only, 0 = sync  (default: 0)
#
set -euo pipefail

GITEA_URL="${GITEA_URL:-https://git.patchnotes.com}"
GITEA_REPO="${GITEA_REPO:-ai-bots/peakbot}"
GITHUB_REPO="${GITHUB_REPO:-thepeak99/peakbot}"
DRY_RUN="${DRY_RUN:-0}"

info() { printf '▶ %s\n' "$*"; }
die() { printf '❌ %s\n' "$*" >&2; exit 1; }

# Gitea and GitHub disagree on trailing newlines / CRLF in release bodies.
# Normalize (strip \r, drop trailing blank lines) so identical notes don't
# trigger an endless "update metadata" churn.
normalize_body() { printf '%s' "$1" | tr -d '\r' | sed -e ':a' -e '/^$/{$d;N;ba}'; }

for bin in curl jq gh sha256sum; do
  command -v "$bin" >/dev/null 2>&1 || die "$bin not found"
done
[ -n "${GITEA_TOKEN:-}" ] || die "GITEA_TOKEN is required"
case "$DRY_RUN" in 0|1) ;; *) die "DRY_RUN must be 0 or 1" ;; esac
gh auth status >/dev/null 2>&1 || die "gh is not authenticated (run: gh auth login)"

API="$GITEA_URL/api/v1/repos/$GITEA_REPO"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

api() {
  curl -fsSL -H "Authorization: token $GITEA_TOKEN" "$@"
}

github_releases() {
  gh api --paginate --slurp "repos/$GITHUB_REPO/releases?per_page=100" | jq 'add // []'
}

github_release_by_tag() {
  local tag="$1"
  github_releases | jq --arg tag "$tag" 'first(.[] | select(.tag_name == $tag)) // empty'
}

github_assets() {
  local release_id="$1"
  gh api --paginate --slurp \
    "repos/$GITHUB_REPO/releases/$release_id/assets?per_page=100" | jq 'add // []'
}

fetch_gitea_releases() {
  local output="$1" page=1 count
  printf '[]' >"$output"

  while :; do
    api "$API/releases?page=$page&limit=100" >"$output.chunk" \
      || die "failed to list Gitea releases"
    count="$(jq 'length' "$output.chunk")"
    [ "$count" -eq 0 ] && break
    # Merge via file inputs, not --argjson: release bodies + asset URLs can
    # overflow ARG_MAX when passed on the command line.
    jq -s '.[0] + .[1]' "$output" "$output.chunk" >"$output.next"
    mv "$output.next" "$output"
    [ "$count" -lt 100 ] && break
    page=$((page + 1))
  done
  rm -f "$output.chunk"
}

release_payload() {
  local name="$1" body="$2" draft="$3" prerelease="$4"
  jq -n \
    --arg name "$name" \
    --arg body "$body" \
    --argjson draft "$draft" \
    --argjson prerelease "$prerelease" \
    '{name: $name, body: $body, draft: $draft, prerelease: $prerelease}'
}

sync_release_metadata() {
  local rel="$1" existing="$2"
  local tag name body draft prerelease release_id payload flags notes err
  tag="$(jq -r '.tag_name' <<<"$rel")"
  name="$(jq -r '.name // .tag_name' <<<"$rel")"
  body="$(jq -r '.body // ""' <<<"$rel")"
  draft="$(jq -r '.draft' <<<"$rel")"
  prerelease="$(jq -r '.prerelease' <<<"$rel")"

  if [ -z "$existing" ]; then
    info "  release: create"
    [ "$DRY_RUN" = "1" ] && return
    flags=()
    [ "$draft" = "true" ] && flags+=(--draft)
    [ "$prerelease" = "true" ] && flags+=(--prerelease)
    notes="$WORKDIR/notes-$(printf '%s' "$tag" | sha256sum | cut -d' ' -f1).md"
    printf '%s' "$body" >"$notes"
    # --verify-tag requires the tag to already exist on GitHub. GitHub is a
    # push-mirror of Gitea, so a not-yet-mirrored tag means "run the git
    # mirror first" — skip this release rather than aborting the whole sync.
    if ! err="$(gh release create "$tag" --repo "$GITHUB_REPO" --verify-tag \
      --title "$name" --notes-file "$notes" "${flags[@]}" 2>&1 >/dev/null)"; then
      printf '%s\n' "$err" >&2
      info "  release: SKIPPED — tag '$tag' not on GitHub yet (push the git mirror first)"
      return 2
    fi
    return
  fi

  if jq -e --arg name "$name" --arg body "$(normalize_body "$body")" \
    --argjson draft "$draft" --argjson prerelease "$prerelease" \
    '.name == $name and (.body // "" | gsub("\r";"") | sub("\n+$";"")) == $body and .draft == $draft and .prerelease == $prerelease' \
    <<<"$existing" >/dev/null; then
    info "  release: metadata already matches"
    return
  fi

  info "  release: update metadata"
  [ "$DRY_RUN" = "1" ] && return
  release_id="$(jq -r '.id' <<<"$existing")"
  payload="$(release_payload "$name" "$body" "$draft" "$prerelease")"
  gh api --method PATCH "repos/$GITHUB_REPO/releases/$release_id" \
    --input - <<<"$payload" >/dev/null
}

# Upload a Gitea asset to GitHub (clobbering any existing same-named asset).
# The third arg is either a local file (already downloaded) or a Gitea URL to
# fetch first. In DRY_RUN nothing is downloaded or uploaded.
sync_upload_asset() {
  local tag="$1" name="$2" src="$3" upload_dir upload_file
  [ "$DRY_RUN" = "1" ] && return 0
  if [ -f "$src" ]; then
    upload_file="$src"
  else
    upload_dir="$WORKDIR/upload-$(printf '%s' "$tag/$name" | sha256sum | cut -d' ' -f1)"
    mkdir -p "$upload_dir"
    upload_file="$upload_dir/$name"
    api -o "$upload_file" "$src" || return 1
  fi
  gh release upload "$tag" "$upload_file" --repo "$GITHUB_REPO" --clobber >/dev/null || return 1
  rm -f "$upload_file"
}

sync_release_assets() {
  local rel="$1" existing="$2"
  local tag source_assets github_assets_json asset name url asset_dir source_file github_file
  local source_hash github_hash github_asset github_id github_size github_digest source_size
  tag="$(jq -r '.tag_name' <<<"$rel")"
  source_assets="$(jq -c '.assets // []' <<<"$rel")"
  if ! jq -e '([.[].name] | length) == ([.[].name] | unique | length)' \
    <<<"$source_assets" >/dev/null; then
    die "release $tag has duplicate asset names"
  fi

  if [ -z "$existing" ]; then
    github_assets_json='[]'
  else
    github_assets_json="$(github_assets "$(jq -r '.id' <<<"$existing")")"
  fi

  while IFS= read -r asset; do
    name="$(jq -r '.name' <<<"$asset")"
    url="$(jq -r '.browser_download_url' <<<"$asset")"
    source_size="$(jq -r '.size // -1' <<<"$asset")"
    [ -n "$name" ] || die "release $tag has an asset with an empty name"
    case "$name" in */*) die "release $tag has an asset name containing '/': $name" ;; esac

    github_asset="$(jq -c --arg name "$name" 'first(.[] | select(.name == $name)) // empty' <<<"$github_assets_json")"

    if [ "$DRY_RUN" = "1" ]; then
      # Preview from metadata only — never download gigabytes to say "matches".
      if [ -z "$github_asset" ]; then
        info "  asset: $name missing — would upload"
      elif [ "$source_size" != "-1" ] \
        && [ "$(jq -r '.size // -1' <<<"$github_asset")" != "$source_size" ]; then
        info "  asset: $name size differs — would replace"
      else
        info "  asset: $name present (size match) — would verify content on sync"
      fi
      continue
    fi

    if [ -z "$github_asset" ]; then
      info "  asset: $name missing — upload"
      sync_upload_asset "$tag" "$name" "$url" && continue || die "failed to sync $tag/$name"
    fi

    github_id="$(jq -r '.id' <<<"$github_asset")"
    github_size="$(jq -r '.size // -1' <<<"$github_asset")"
    github_digest="$(jq -r '.digest // ""' <<<"$github_asset")"

    # Cheap pre-filters that avoid downloading the GitHub asset entirely:
    #   1. size differs           → definitely changed, skip straight to replace
    #   2. digest present + equal  → identical, nothing to do
    #   3. digest present + differ → changed, replace
    if [ "$github_size" != "-1" ] && [ "$source_size" != "-1" ] \
      && [ "$github_size" != "$source_size" ]; then
      info "  asset: $name changed (size $github_size→$source_size) — replace"
      sync_upload_asset "$tag" "$name" "$url" && continue || die "failed to sync $tag/$name"
    fi

    if [ -n "$github_digest" ]; then
      asset_dir="$WORKDIR/source-$(printf '%s' "$tag/$name" | sha256sum | cut -d' ' -f1)"
      mkdir -p "$asset_dir"
      source_file="$asset_dir/$name"
      api -o "$source_file" "$url" || die "failed to download $tag/$name from Gitea"
      source_hash="$(sha256sum "$source_file" | cut -d' ' -f1)"
      if [ "$github_digest" = "sha256:$source_hash" ]; then
        info "  asset: $name already matches (digest)"
        rm -f "$source_file"
        continue
      fi
      info "  asset: $name changed (digest) — replace"
      sync_upload_asset "$tag" "$name" "$source_file" && continue || die "failed to sync $tag/$name"
    fi

    # No digest and same size: fall back to downloading both and hash-comparing.
    asset_dir="$WORKDIR/source-$(printf '%s' "$tag/$name" | sha256sum | cut -d' ' -f1)"
    mkdir -p "$asset_dir"
    source_file="$asset_dir/$name"
    api -o "$source_file" "$url" || die "failed to download $tag/$name from Gitea"
    source_hash="$(sha256sum "$source_file" | cut -d' ' -f1)"

    github_file="$WORKDIR/github-$github_id"
    gh api -H 'Accept: application/octet-stream' \
      "repos/$GITHUB_REPO/releases/assets/$github_id" >"$github_file" \
      || die "failed to download GitHub asset $tag/$name"
    github_hash="$(sha256sum "$github_file" | cut -d' ' -f1)"
    if [ "$source_hash" = "$github_hash" ]; then
      info "  asset: $name already matches"
      rm -f "$source_file" "$github_file"
      continue
    fi
    info "  asset: $name changed — replace"
    rm -f "$github_file"
    sync_upload_asset "$tag" "$name" "$source_file" && continue || die "failed to sync $tag/$name"
  done < <(jq -c '.[]' <<<"$source_assets")

  while IFS=$'\t' read -r asset_id name; do
    [ -n "$asset_id" ] || continue
    if jq -e --arg name "$name" 'any(.[]; .name == $name)' <<<"$source_assets" >/dev/null; then
      continue
    fi
    info "  asset: $name exists only on GitHub — delete"
    if [ "$DRY_RUN" != "1" ]; then
      gh api --method DELETE "repos/$GITHUB_REPO/releases/assets/$asset_id" >/dev/null
    fi
  done < <(jq -r '.[] | [.id, .name] | @tsv' <<<"$github_assets_json")
}

GITEA_RELEASES="$WORKDIR/gitea-releases.json"
fetch_gitea_releases "$GITEA_RELEASES"
GITHUB_RELEASES="$(github_releases)"
count="$(jq 'length' "$GITEA_RELEASES")"

info "Source: $GITEA_REPO on $GITEA_URL"
info "Destination: $GITHUB_REPO on GitHub"
[ "$DRY_RUN" = "1" ] && info "DRY RUN — no changes will be made"
info "Found $count Gitea release(s); syncing oldest to newest"

while IFS= read -r rel; do
  tag="$(jq -r '.tag_name' <<<"$rel")"
  [ -n "$tag" ] || die "Gitea returned a release with an empty tag"
  existing="$(jq -c --arg tag "$tag" 'first(.[] | select(.tag_name == $tag)) // empty' <<<"$GITHUB_RELEASES")"

  printf '\n'
  info "── $tag ──"
  # Skip asset sync if the release couldn't be created (tag not mirrored yet).
  if ! sync_release_metadata "$rel" "$existing"; then
    continue
  fi

  if [ -z "$existing" ] && [ "$DRY_RUN" != "1" ]; then
    existing="$(github_release_by_tag "$tag")"
  fi
  sync_release_assets "$rel" "$existing"
done < <(jq -c 'sort_by(.created_at)[]' "$GITEA_RELEASES")

while IFS=$'\t' read -r release_id tag; do
  [ -n "$release_id" ] || continue
  if jq -e --arg tag "$tag" 'any(.[]; .tag_name == $tag)' "$GITEA_RELEASES" >/dev/null; then
    continue
  fi
  info "Release $tag exists only on GitHub — delete"
  if [ "$DRY_RUN" != "1" ]; then
    gh api --method DELETE "repos/$GITHUB_REPO/releases/$release_id" >/dev/null
  fi
done < <(jq -r '.[] | [.id, .tag_name] | @tsv' <<<"$GITHUB_RELEASES")

printf '\n'
if [ "$DRY_RUN" = "1" ]; then
  info "✅ Dry-run complete. No changes were made."
else
  info "✅ GitHub releases now match Gitea."
fi
