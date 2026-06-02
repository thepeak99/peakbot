.PHONY: all build build-linux build-macos build-windows clean test docker-build help \
        release release-bump release-tag \
        release-build-linux release-build-windows release-build-macos release-publish

# Config
DOCKERFILE         := Dockerfile.windows
DOCKERFILE_LINUX   := Dockerfile.linux
DOCKERFILE_MACOS   := Dockerfile.macos
OUTPUT_DIR         := output
# Default (unversioned) artifact names. Release builds bake the version into
# the filename via --build-arg VERSION=$v from the .release-version stash.
LINUX_BIN          := peakbot-linux-amd64
WIN_BIN            := peakbot-windows-amd64.exe
MACOS_BIN          := peakbot-macos-universal2
CONTAINER_BUILDER ?= $(shell command -v podman 2>/dev/null || echo docker)

# Release config — derived from `origin` remote so you only need GITEA_TOKEN
ORIGIN_URL := $(shell git config --get remote.origin.url 2>/dev/null)
GITEA_URL  ?= $(shell echo $(ORIGIN_URL) | sed -E 's#(https?://[^/]+).*#\1#')
OWNER      ?= $(shell echo $(ORIGIN_URL) | sed -E 's#https?://[^/]+/([^/]+)/.*#\1#')
REPO       ?= $(shell basename -s .git $(ORIGIN_URL) 2>/dev/null)

# VERSION may be passed on the CLI (make release VERSION=0.2.0) or prompted.
VERSION ?=

# NOTES may point to a markdown file used as the release body + tag message.
# Default resolution (in release-tag and release-publish): release-notes/<v>.md.
NOTES ?=

# Default target
all: build

## build: Cross-compile all three platforms (linux, windows, macos)
build: build-linux build-windows build-macos

## build-windows: Cross-compile to Windows x86_64 .exe (via Dockerfile.windows)
build-windows: $(OUTPUT_DIR)/$(WIN_BIN)

$(OUTPUT_DIR)/$(WIN_BIN): Dockerfile.windows
	@mkdir -p $(OUTPUT_DIR)
	@echo "🔨 Cross-compiling peakbot for Windows..."
	$(CONTAINER_BUILDER) build \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE) \
		.
	@echo "✅ Built: $(OUTPUT_DIR)/$(WIN_BIN)"
	@ls -lh $(OUTPUT_DIR)/$(WIN_BIN)

## build-linux: Cross-compile to Linux x86_64 binary (via Dockerfile.linux)
build-linux: $(OUTPUT_DIR)/$(LINUX_BIN)

$(OUTPUT_DIR)/$(LINUX_BIN): Dockerfile.linux
	@mkdir -p $(OUTPUT_DIR)
	@echo "🐧 Building peakbot for Linux..."
	$(CONTAINER_BUILDER) build \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE_LINUX) \
		.
	@chmod +x $(OUTPUT_DIR)/$(LINUX_BIN)
	@echo "✅ Built: $(OUTPUT_DIR)/$(LINUX_BIN)"
	@ls -lh $(OUTPUT_DIR)/$(LINUX_BIN)

## build-macos: Cross-compile to macOS universal2 binary (Intel + Apple Silicon)
build-macos: $(OUTPUT_DIR)/$(MACOS_BIN)

$(OUTPUT_DIR)/$(MACOS_BIN): Dockerfile.macos
	@mkdir -p $(OUTPUT_DIR)
	@echo "🍎 Cross-compiling peakbot for macOS (universal2)..."
	$(CONTAINER_BUILDER) build \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE_MACOS) \
		.
	@chmod +x $(OUTPUT_DIR)/$(MACOS_BIN)
	@echo "✅ Built: $(OUTPUT_DIR)/$(MACOS_BIN)"
	@ls -lh $(OUTPUT_DIR)/$(MACOS_BIN)

## clean: Remove output artifacts
clean:
	rm -rf $(OUTPUT_DIR)
	@echo "🧹 Cleaned $(OUTPUT_DIR)/"

## docker-build: Alias for build
docker-build: build

## rebuild: Clean and rebuild from scratch
rebuild: clean build

# ─────────────────────────────────────────────────────────────────────────────
# Release pipeline
# ─────────────────────────────────────────────────────────────────────────────

## release: Full release flow — bump, tag, build, publish to Gitea
release: release-bump release-tag release-build-linux release-build-windows release-build-macos release-publish
	@echo ""
	@echo "🎉 Release $$(grep '^version' Cargo.toml | head -1 | cut -d'\"' -f2) complete!"

## release-bump: Update Cargo.toml + Cargo.lock, commit
release-bump:
	@set -eu; \
	if [ -z "$(VERSION)" ]; then \
	  current=$$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$$3); print $$3; exit}' Cargo.toml); \
	  printf "Current version: %s\nNew version: " "$$current"; \
	  read v; \
	else \
	  v="$(VERSION)"; \
	fi; \
	if ! echo "$$v" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.-]+)?$$'; then \
	  echo "❌ Invalid version: '$$v' (expected X.Y.Z)"; exit 1; \
	fi; \
	if git rev-parse "refs/tags/$$v" >/dev/null 2>&1; then \
	  echo "❌ Tag '$$v' already exists"; exit 1; \
	fi; \
	if [ -z "$${ALLOW_DIRTY:-}" ] && [ -n "$$(git status --porcelain | grep -v '^?? ')" ]; then \
	  echo "❌ Working tree has uncommitted changes (set ALLOW_DIRTY=1 to override):"; \
	  git status --short; exit 1; \
	fi; \
	echo "📝 Bumping version to $$v..."; \
	awk -v v="$$v" '/^\[package\]/{p=1} p && /^version[[:space:]]*=/ && !done {sub(/"[^"]*"/, "\"" v "\""); done=1} {print}' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml; \
	echo "🔒 Updating Cargo.lock..."; \
	cargo update -p peakbot --precise "$$v" >/dev/null 2>&1 || cargo update -p peakbot >/dev/null; \
	git add Cargo.toml Cargo.lock; \
	git commit -m "chore: release $$v" -m "Co-authored by PeakBot!"; \
	echo "✅ Committed version bump to $$v"; \
	echo "$$v" > .release-version

## release-tag: Create annotated tag and push branch + tag.
## If the branch is protected, automatically opens a PR, merges it (verifying
## the merge via the PR's `merged` flag, not the merge response body), then
## pushes the tag.
release-tag:
	@set -eu; \
	if [ ! -f .release-version ]; then \
	  echo "❌ No .release-version file — run 'make release-bump' first"; exit 1; \
	fi; \
	v=$$(cat .release-version); \
	branch=$$(git rev-parse --abbrev-ref HEAD); \
	echo "🏷️  Tagging $$v on branch '$$branch'..."; \
	notes="$${NOTES:-release-notes/$$v.md}"; \
	if [ -f "$$notes" ]; then \
	  echo "📝 Using release notes from $$notes"; \
	  git tag -a "$$v" -F "$$notes"; \
	else \
	  echo "ℹ️  No release-notes file (looked for $$notes); using default tag message"; \
	  git tag -a "$$v" -m "Release $$v"; \
	fi; \
	echo "📤 Pushing branch and tag..."; \
	if git push origin "$$branch" 2>/dev/null; then \
	  echo "✅ Branch pushed"; \
	else \
	  echo "⚠️  Direct push to $$branch failed (protected branch?). Creating PR..."; \
	  if [ -z "$${GITEA_TOKEN:-}" ]; then \
	    echo "❌ GITEA_TOKEN is not set. Cannot create PR."; exit 1; \
	  fi; \
	  command -v jq >/dev/null   || { echo "❌ 'jq' is required";   exit 1; }; \
	  command -v curl >/dev/null || { echo "❌ 'curl' is required"; exit 1; }; \
	  release_branch="release/$$v"; \
	  git checkout -b "$$release_branch"; \
	  git push origin "$$release_branch"; \
	  api="$(GITEA_URL)/api/v1/repos/$(OWNER)/$(REPO)"; \
	  pr_body="## Release $$v\\n\\nThis PR contains the version bump for v$$v.\\n\\nCo-authored by PeakBot!"; \
	  pr_resp=$$(curl -sS -X POST "$$api/pulls" \
	    -H "Authorization: token $$GITEA_TOKEN" \
	    -H "Content-Type: application/json" \
	    -d "{\"title\":\"chore: release $$v\",\"body\":\"$$pr_body\",\"head\":\"$$release_branch\",\"base\":\"$$branch\"}"); \
	  pr_num=$$(echo "$$pr_resp" | jq -r '.number // empty'); \
	  if [ -z "$$pr_num" ]; then \
	    echo "❌ Failed to create PR. Response:"; echo "$$pr_resp" | jq .; exit 1; \
	  fi; \
	  echo "✅ Created PR #$$pr_num"; \
	  echo "🔀 Merging PR #$$pr_num..."; \
	  merge_resp=$$(curl -sS -X POST "$$api/pulls/$$pr_num/merge" \
	    -H "Authorization: token $$GITEA_TOKEN" \
	    -H "Content-Type: application/json" \
	    -d '{"Do":"merge","merge_message_field":"chore: release '$$v'","delete_branch_after_merge":true}'); \
	  merged=$$(curl -sS -H "Authorization: token $$GITEA_TOKEN" "$$api/pulls/$$pr_num" | jq -r '.merged // false'); \
	  if [ "$$merged" != "true" ]; then \
	    echo "❌ Failed to merge PR. Merge response:"; echo "$$merge_resp" | jq . 2>/dev/null || echo "$$merge_resp"; exit 1; \
	  fi; \
	  echo "✅ PR merged"; \
	  git checkout "$$branch"; \
	  git pull origin "$$branch"; \
	  git branch -D "$$release_branch" 2>/dev/null || true; \
	fi; \
	git push origin "$$v"; \
	echo "✅ Pushed tag $$v"

## release-build-linux: Build linux/amd64 binary (via Dockerfile.linux) with the release version baked into the filename
release-build-linux:
	@set -eu; \
	if [ ! -f .release-version ]; then echo "❌ run release-bump first"; exit 1; fi; \
	v=$$(cat .release-version); \
	mkdir -p $(OUTPUT_DIR); \
	echo "🐧 Building peakbot $$v for Linux..."; \
	$(CONTAINER_BUILDER) build \
		--build-arg VERSION=$$v \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE_LINUX) \
		.; \
	chmod +x $(OUTPUT_DIR)/peakbot-$$v-linux-amd64; \
	echo "✅ $(OUTPUT_DIR)/peakbot-$$v-linux-amd64"; \
	ls -lh $(OUTPUT_DIR)/peakbot-$$v-linux-amd64

## release-build-windows: Build windows/amd64 binary (via Dockerfile.windows) with the release version baked into the filename
release-build-windows:
	@set -eu; \
	if [ ! -f .release-version ]; then echo "❌ run release-bump first"; exit 1; fi; \
	v=$$(cat .release-version); \
	mkdir -p $(OUTPUT_DIR); \
	echo "🔨 Cross-compiling peakbot $$v for Windows..."; \
	$(CONTAINER_BUILDER) build \
		--build-arg VERSION=$$v \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE) \
		.; \
	echo "✅ $(OUTPUT_DIR)/peakbot-$$v-windows-amd64.exe"; \
	ls -lh $(OUTPUT_DIR)/peakbot-$$v-windows-amd64.exe

## release-build-macos: Build macOS universal2 binary (via Dockerfile.macos) with the release version baked into the filename
release-build-macos:
	@set -eu; \
	if [ ! -f .release-version ]; then echo "❌ run release-bump first"; exit 1; fi; \
	v=$$(cat .release-version); \
	mkdir -p $(OUTPUT_DIR); \
	echo "🍎 Cross-compiling peakbot $$v for macOS (universal2)..."; \
	$(CONTAINER_BUILDER) build \
		--build-arg VERSION=$$v \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE_MACOS) \
		.; \
	chmod +x $(OUTPUT_DIR)/peakbot-$$v-macos-universal2; \
	echo "✅ $(OUTPUT_DIR)/peakbot-$$v-macos-universal2"; \
	ls -lh $(OUTPUT_DIR)/peakbot-$$v-macos-universal2

## release-publish: Create Gitea release and upload binaries
release-publish:
	@set -eu; \
	if [ -z "$${GITEA_TOKEN:-}" ]; then \
	  echo "❌ GITEA_TOKEN is not set. Generate one at $(GITEA_URL)/user/settings/applications"; exit 1; \
	fi; \
	command -v jq >/dev/null   || { echo "❌ 'jq' is required";   exit 1; }; \
	command -v curl >/dev/null || { echo "❌ 'curl' is required"; exit 1; }; \
	if [ ! -f .release-version ]; then echo "❌ run release-bump first"; exit 1; fi; \
	v=$$(cat .release-version); \
	linux_asset="$(OUTPUT_DIR)/peakbot-$$v-linux-amd64"; \
	win_asset="$(OUTPUT_DIR)/peakbot-$$v-windows-amd64.exe"; \
	macos_asset="$(OUTPUT_DIR)/peakbot-$$v-macos-universal2"; \
	[ -f "$$linux_asset" ] || { echo "❌ Missing $$linux_asset"; exit 1; }; \
	[ -f "$$win_asset"   ] || { echo "❌ Missing $$win_asset";   exit 1; }; \
	[ -f "$$macos_asset" ] || { echo "❌ Missing $$macos_asset"; exit 1; }; \
	api="$(GITEA_URL)/api/v1/repos/$(OWNER)/$(REPO)"; \
	echo "🚀 Creating Gitea release $$v at $$api/releases ..."; \
	notes="$${NOTES:-release-notes/$$v.md}"; \
	if [ -f "$$notes" ]; then \
	  echo "📝 Using release notes from $$notes"; \
	  body=$$(jq -nc --arg tag "$$v" --arg name "Release $$v" --rawfile bd "$$notes" \
	    '{tag_name:$$tag, target_commitish:"", name:$$name, body:$$bd, draft:false, prerelease:false}'); \
	else \
	  echo "ℹ️  No release-notes file (looked for $$notes); using default body"; \
	  body=$$(jq -nc --arg tag "$$v" --arg name "Release $$v" --arg bd "Release $$v" \
	    '{tag_name:$$tag, target_commitish:"", name:$$name, body:$$bd, draft:false, prerelease:false}'); \
	fi; \
	resp=$$(curl -sS -X POST "$$api/releases" \
	  -H "Authorization: token $$GITEA_TOKEN" \
	  -H "Content-Type: application/json" \
	  -d "$$body"); \
	rid=$$(echo "$$resp" | jq -r '.id // empty'); \
	if [ -z "$$rid" ]; then \
	  echo "❌ Failed to create release. Response:"; echo "$$resp" | jq .; exit 1; \
	fi; \
	echo "✅ Release id=$$rid"; \
	for f in "$$linux_asset" "$$win_asset" "$$macos_asset"; do \
	  name=$$(basename "$$f"); \
	  echo "⬆️  Uploading $$name ..."; \
	  up=$$(curl -sS -X POST "$$api/releases/$$rid/assets?name=$$name" \
	    -H "Authorization: token $$GITEA_TOKEN" \
	    -F "attachment=@$$f"); \
	  url=$$(echo "$$up" | jq -r '.browser_download_url // empty'); \
	  if [ -z "$$url" ]; then \
	    echo "❌ Upload failed for $$name. Response:"; echo "$$up" | jq .; exit 1; \
	  fi; \
	  echo "    → $$url"; \
	done; \
	rm -f .release-version; \
	echo "✅ Published $$v to $(GITEA_URL)/$(OWNER)/$(REPO)/releases/tag/$$v"

## help: Show this help
help:
	@echo "PeakBot Makefile"
	@echo ""
	@echo "Usage:"
	@echo "  make [target] [VAR=value]"
	@echo ""
	@echo "Targets:"
	@awk 'BEGIN {FS = ":[[:space:]]*"} /^## / {sub(/^## /,""); printf "  %-22s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo ""
	@echo "Release usage:"
	@echo "  export GITEA_TOKEN=...                      # required"
	@echo "  make release                                # prompts for version"
	@echo "  make release VERSION=0.2.0                  # non-interactive"
	@echo "  make release VERSION=0.2.0 ALLOW_DIRTY=1    # bypass clean tree check"
	@echo "  make release VERSION=0.2.0 NOTES=path/to/notes.md  # custom notes file"
	@echo ""
	@echo "Release notes:"
	@echo "  Default: release-notes/<v>.md (if present) is used as the Gitea body + git tag message."
	@echo "  Override: pass NOTES=<path> to use any markdown file."
	@echo "  Missing:  pipeline still ships, body falls back to \"Release <v>\"."
	@echo ""
	@echo "Environment:"
	@echo "  CONTAINER_BUILDER  Container runtime (default: podman or docker)"
	@echo "  GITEA_TOKEN        Gitea personal access token (required for release)"
	@echo "  GITEA_URL/OWNER/REPO  Auto-derived from origin remote — override if needed"
	@echo "  NOTES              Path to a markdown release-notes file (default: release-notes/<v>.md)"
