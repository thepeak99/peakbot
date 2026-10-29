.PHONY: all build build-linux build-macos build-windows build-android clean test docker-build help \
        web dev \
        desktop-test desktop-test-windows desktop-fonttest desktop-dev desktop-linux desktop-macos desktop-windows desktop \
        desktop-updater \
        release release-check release-bump release-tag \
        release-build-linux release-build-windows release-build-macos release-build-android \
        release-publish release-publish-github

# Config
DOCKERFILE         := Dockerfile.windows
DOCKERFILE_LINUX   := Dockerfile.linux
DOCKERFILE_MACOS   := Dockerfile.macos
DOCKERFILE_ANDROID := Dockerfile.android
OUTPUT_DIR         := output
# RELEASE_VERSION — the version being released, straight from the root
# Cargo.toml [package] section (single source of truth — no state file).
RELEASE_VERSION := $(shell awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[" ]/,"",$$3); print $$3; exit}' Cargo.toml)
# Default (unversioned) artifact names. Release builds bake RELEASE_VERSION
# into the filename via --build-arg VERSION.
LINUX_BIN          := peakbot-linux-amd64
WIN_BIN            := peakbot-windows-amd64.exe
MACOS_BIN          := peakbot-macos-universal2
ANDROID_BIN        := peakbot-android-arm64
CONTAINER_BUILDER ?= $(shell command -v podman 2>/dev/null || echo docker)

# Release config — derived from `origin` remote so you only need GITEA_TOKEN
ORIGIN_URL := $(shell git config --get remote.origin.url 2>/dev/null)
GITEA_URL  ?= $(shell echo $(ORIGIN_URL) | sed -E 's#(https?://[^/]+).*#\1#')
OWNER      ?= $(shell echo $(ORIGIN_URL) | sed -E 's#https?://[^/]+/([^/]+)/.*#\1#')
REPO       ?= $(shell basename -s .git $(ORIGIN_URL) 2>/dev/null)

# GitHub mirror (public) — the auto-updater's download host; the pipeline
# publishes the release there too (release-publish-github).
GH_MIRROR_REPO    ?= thepeak99/peakbot
GH_API            ?= https://api.github.com
GH_UPLOADS        ?= https://uploads.github.com
GH_TAG_WAIT_TRIES ?= 60
GH_TAG_WAIT_SECS  ?= 10

# The 10 release asset basenames, shared by release-publish (Gitea) and
# release-publish-github (GitHub mirror). tests/release.sh T44 counts the
# words of the FIRST assignment block (name + '=' + 8 names = 10), so the
# last two entries live in the += line.
RELEASE_ASSETS = \
	peakbot-$$v-linux-amd64 \
	peakbot-$$v-windows-amd64.exe \
	PeakBot-Desktop-$$v-linux-x86_64.AppImage \
	PeakBot-Desktop-$$v-macos-universal.zip \
	PeakBot-Desktop-$$v-macos-universal.app.tar.gz \
	PeakBot-Desktop-$$v-windows-x86_64-portable.exe \
	PeakBot-Desktop-$$v-windows-x86_64-setup.exe \
	latest.json
RELEASE_ASSETS += \
	peakbot-$$v-macos-universal2 \
	peakbot-$$v-android-arm64

# VERSION for release-bump: make release-bump VERSION=X.Y.Z (required, no prompt).
VERSION ?=

# Default target
all: build

## build: Cross-compile all four platforms (linux, windows, macos, android)
build: build-linux build-windows build-macos build-android

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

## build-android: Cross-compile to Android arm64 (aarch64-linux-android) terminal binary
build-android: $(OUTPUT_DIR)/$(ANDROID_BIN)

$(OUTPUT_DIR)/$(ANDROID_BIN): Dockerfile.android
	@mkdir -p $(OUTPUT_DIR)
	@echo "🤖 Cross-compiling peakbot for Android (arm64)..."
	$(CONTAINER_BUILDER) build \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE_ANDROID) \
		.
	@chmod +x $(OUTPUT_DIR)/$(ANDROID_BIN)
	@echo "✅ Built: $(OUTPUT_DIR)/$(ANDROID_BIN)"
	@ls -lh $(OUTPUT_DIR)/$(ANDROID_BIN)

## web: Build the web UI (web/dist/) — required for bare `peakbot` to serve the real SPA.
##        CI and Docker builds run the Node stage automatically; this is for local dev.
web:
	@command -v node >/dev/null 2>&1 || { echo "❌ node not found — install Node.js 22+"; exit 1; }
	@command -v npm >/dev/null 2>&1 || { echo "❌ npm not found — install Node.js 22+"; exit 1; }
	cd web && npm install && npm run build
	@touch web/dist/.gitkeep   # Vite's emptyOutDir wipes it each build; keep the tracked placeholder

## dev: Hot-reload dev loop — backend (cargo watch) + web (Vite HMR) together.
##       Open http://localhost:5173 (Vite/HMR). The backend runs on :8080 and
##       Vite proxies /ws to it. A Rust edit rebuilds+restarts the backend
##       (drops the WS session, ~seconds); a web/src edit hot-swaps (<1s, no reload).
dev:
	@command -v cargo-watch >/dev/null 2>&1 || { echo "❌ cargo-watch not found — run: cargo install cargo-watch"; exit 1; }
	@command -v npm >/dev/null 2>&1 || { echo "❌ npm not found — install Node.js 22+"; exit 1; }
	cd web && npm install
	@echo "→ web (HMR): http://localhost:5173   backend: http://127.0.0.1:8080"
	@trap 'kill 0' INT TERM; \
	PEAKBOT_NO_OPEN=1 cargo watch -x 'run -- --bind 127.0.0.1:8080' & \
	( cd web && npm run dev ) & \
	wait

## clean: Remove ALL build artifacts (cargo targets, web SPA + node_modules, desktop sidecar/gen/fonts, outputs, release temp)
clean:
	rm -rf $(OUTPUT_DIR)
	cargo clean
	cargo clean --manifest-path desktop/Cargo.toml
	rm -rf web/dist web/node_modules
	rm -rf desktop/src-tauri/gen desktop/src-tauri/linux/fonts
	rm -f  desktop/src-tauri/binaries/peakbot-*
	@mkdir -p web/dist && touch web/dist/.gitkeep desktop/src-tauri/binaries/.gitkeep
	@echo "🧹 Cleaned all build artifacts"

## docker-build: Alias for build
docker-build: build

## rebuild: Clean and rebuild from scratch
rebuild: clean build

# ─────────────────────────────────────────────────────────────────────────────
# Desktop (Tauri shell — see desktop/DESIGN.md)
# ─────────────────────────────────────────────────────────────────────────────

## desktop-test: Unit tests for the desktop core crate (desktop/core, no webkit)
desktop-test:
	cargo test --locked --manifest-path desktop/Cargo.toml -p peakbot-desktop-core

## desktop-test-windows: Static (no-docker) tests for the Windows portable-exe
##                       pipeline (build script, Dockerfiles, Makefile, smoke)
desktop-test-windows:
	sh desktop/scripts/test-build-desktop-windows.sh

## desktop-fonttest: Host fontconfig tests for the DESIGN §4.12 emoji-isolation
##                   config — fetches NotoColorEmoji.ttf (sha-verified) and
##                   runs the ignored desktop/core/tests/fontconfig_host.rs suite
desktop-fonttest:
	sh desktop/scripts/fetch-emoji-font.sh desktop/src-tauri/linux/fonts
	PEAKBOT_EMOJI_FONT_DIR=$(CURDIR)/desktop/src-tauri/linux/fonts cargo test --locked --manifest-path desktop/Cargo.toml -p peakbot-desktop-core --test fontconfig_host -- --ignored

## desktop-dev: Build peakbot (debug), stage the sidecar into src-tauri/binaries,
##              then run the Tauri dev loop (desktop/DESIGN.md §5.7)
desktop-dev:
	cargo build
	cp target/debug/peakbot desktop/src-tauri/binaries/peakbot-$(shell rustc -vV | sed -n 's/host: //p')
	cd desktop/src-tauri && npx --yes @tauri-apps/cli@2.12.3 dev

## desktop-linux: Build the Linux x86_64 AppImage (sidecar via Dockerfile.linux,
##                then Dockerfile.desktop-linux → output/desktop/*.AppImage)
desktop-linux:
	sh desktop/scripts/build-desktop.sh linux

## desktop-macos: Build the macOS universal .app zip (sidecar via Dockerfile.macos,
##                then Dockerfile.desktop-macos → output/desktop/*.zip)
desktop-macos:
	sh desktop/scripts/build-desktop.sh macos

## desktop-windows: Build the Windows x86_64 portable exe + NSIS installer (→
##                  output/desktop/PeakBot-Desktop-<v>-windows-x86_64-{portable,setup}.exe)
desktop-windows:
	sh desktop/scripts/build-desktop.sh windows

## desktop-updater: Sign the 3 updater artifacts + build latest.json
##                  (Dockerfile.desktop-updater → output/desktop/*.sig ×3 +
##                  latest.json). The signing key crosses the build boundary
##                  as a BuildKit secret from TAURI_SIGNING_PRIVATE_KEY
##                  (+_PASSWORD) in the environment — never in a layer/log.
desktop-updater:
	GH_MIRROR_REPO="$(GH_MIRROR_REPO)" sh desktop/scripts/build-desktop.sh updater

## desktop: Build all desktop artifacts (linux + macos + windows)
desktop:
	sh desktop/scripts/build-desktop.sh all

# ─────────────────────────────────────────────────────────────────────────────
# Release pipeline
#
# Flow: make release-bump VERSION=X.Y.Z → open a PR → merge → make release-tag
#       (the tag push triggers CI, which runs: make release TAG=X.Y.Z)
# ─────────────────────────────────────────────────────────────────────────────

# PRUNE=1 makes the release recipe prune the container builder between
# platform builds. Read by the recipe shell (not make) so the guard text
# survives `make -n` (see tests/release.sh T32).
PRUNE ?=
export PRUNE
PRUNE_STEP = if [ "$${PRUNE}" = "1" ]; then $(CONTAINER_BUILDER) builder prune -af; fi

## release-check: Verify TAG is set and equals the Cargo.toml version, desktop
##                version is in sync, release-notes/<TAG>.md exists, and the
##                tauri.conf.json updater endpoints point at GH_MIRROR_REPO
release-check:
	@set -eu; \
	if [ -z "$(TAG)" ]; then \
	  echo "❌ TAG is not set — pass TAG=<version> (expected: $(RELEASE_VERSION))"; exit 1; \
	fi; \
	if [ "$(TAG)" != "$(RELEASE_VERSION)" ]; then \
	  echo "❌ TAG '$(TAG)' does not match the Cargo.toml version '$(RELEASE_VERSION)'"; exit 1; \
	fi; \
	sh desktop/scripts/check-version.sh; \
	if [ ! -f "release-notes/$(TAG).md" ]; then \
	  echo "❌ Release notes missing: release-notes/$(TAG).md"; exit 1; \
	fi; \
	if ! jq -r '.plugins.updater.endpoints[]?' desktop/src-tauri/tauri.conf.json 2>/dev/null | grep -qF "$(GH_MIRROR_REPO)"; then \
	  echo "❌ tauri.conf.json plugins.updater endpoints do not contain GH_MIRROR_REPO ($(GH_MIRROR_REPO)) — the app would fetch updates from the wrong host"; exit 1; \
	fi; \
	echo "✅ release-check passed: TAG=$(TAG)"

## release: Full release pipeline — check, build all 10 assets, sign the
##          updater artifacts, publish to Gitea, publish to the GitHub
##          mirror. CI runs this on tag push: make release TAG=<v>.
##          Explicit sequential recipe of sub-make calls: the per-platform
##          targets (release-build-*, desktop-*) are the single source of
##          truth for the build commands — nothing is inlined here.
##          Command-line vars (TAG, PRUNE, CONTAINER_BUILDER, GITEA_URL,
##          OWNER, REPO, OUTPUT_DIR) propagate to the sub-makes via
##          MAKEFLAGS; GITEA_TOKEN / GH_RELEASE_TOKEN / TAURI_SIGNING_* ride
##          along in the environment.
##          Order matters: never run with -j.
release:
	@echo "🚀 Release $(RELEASE_VERSION) — sequential by design, do not run with -j"
	$(MAKE) release-check TAG=$(TAG)
	$(MAKE) release-build-linux
	$(MAKE) desktop-linux
	$(PRUNE_STEP)
	$(MAKE) release-build-macos
	$(MAKE) desktop-macos
	$(PRUNE_STEP)
	$(MAKE) release-build-windows
	$(MAKE) desktop-windows
	$(PRUNE_STEP)
	$(MAKE) release-build-android
	$(MAKE) desktop-updater
	$(MAKE) release-publish TAG=$(TAG)
	$(MAKE) release-publish-github TAG=$(TAG)

## release-bump: Bump to VERSION (X.Y.Z, required) on a fresh release/<v>
##               branch cut from origin/master — bump both Cargo.tomls (+
##               lock updates), promote release-notes/current.md to
##               release-notes/<v>.md, commit, push. Open a PR; after the
##               merge, run release-tag.
release-bump:
	@set -eu; \
	if [ -z "$(VERSION)" ]; then \
	  echo "❌ VERSION is required: make release-bump VERSION=X.Y.Z"; exit 1; \
	fi; \
	v="$(VERSION)"; \
	if ! echo "$$v" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$$'; then \
	  echo "❌ Invalid version: '$$v' (expected X.Y.Z)"; exit 1; \
	fi; \
	if git rev-parse "refs/tags/$$v" >/dev/null 2>&1; then \
	  echo "❌ Tag '$$v' already exists"; exit 1; \
	fi; \
	if [ -n "$$(git status --porcelain | grep -v '^?? ')" ]; then \
	  echo "❌ Working tree has uncommitted changes (dirty tree) — commit or stash first:"; \
	  git status --short; exit 1; \
	fi; \
	echo "🌿 Cutting release/$$v from origin/master..."; \
	git fetch origin master; \
	git switch -c "release/$$v" origin/master; \
	echo "📝 Bumping Cargo.toml to $$v..."; \
	awk -v v="$$v" '/^\[package\]/{p=1} p && /^version[[:space:]]*=/ && !done {sub(/"[^"]*"/, "\"" v "\""); done=1} {print}' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml; \
	echo "🖥️  Bumping desktop/Cargo.toml to $$v..."; \
	awk -v v="$$v" '/^\[workspace.package\]/{p=1} p && /^version[[:space:]]*=/ && !done {sub(/"[^"]*"/, "\"" v "\""); done=1} {print}' desktop/Cargo.toml > desktop/Cargo.toml.tmp && mv desktop/Cargo.toml.tmp desktop/Cargo.toml; \
	echo "🔒 Updating Cargo.lock..."; \
	cargo update -p peakbot --precise "$$v" >/dev/null 2>&1 || cargo update -p peakbot >/dev/null; \
	cargo update --workspace --manifest-path desktop/Cargo.toml >/dev/null 2>&1; \
	echo "📄 Promoting release notes: release-notes/$$v.md ← release-notes/current.md"; \
	if [ ! -f release-notes/current.md ]; then \
	  echo "❌ release-notes/current.md not found"; exit 1; \
	fi; \
	awk '/^## /{f=1} f' release-notes/current.md > release-notes/$$v.md; \
	awk '/^## /{exit} {print}' release-notes/current.md > release-notes/current.md.tmp; \
	printf '## Changes\n' >> release-notes/current.md.tmp; \
	mv release-notes/current.md.tmp release-notes/current.md; \
	git add Cargo.toml Cargo.lock desktop/Cargo.toml desktop/Cargo.lock release-notes/$$v.md release-notes/current.md; \
	git commit -m "chore: release $$v" -m "Co-authored by PeakBot!"; \
	echo "📤 Pushing release/$$v to origin..."; \
	git push -u origin HEAD; \
	echo "✅ Bumped to $$v on release/$$v — open a PR, merge it, then run make release-tag"

## release-tag: Create the annotated tag for the Cargo.toml version (the
##              release-notes file as tag message) and push it. Requires a
##              clean tree on master in sync with origin, an existing
##              release-notes/<v>.md, and a not-yet-existing tag.
release-tag:
	@set -eu; \
	v="$(RELEASE_VERSION)"; \
	if [ -n "$$(git status --porcelain | grep -v '^?? ')" ]; then \
	  echo "❌ Working tree has uncommitted changes (dirty tree) — commit or stash first:"; \
	  git status --short; exit 1; \
	fi; \
	git fetch origin --no-tags; \
	if [ "$$(git rev-parse HEAD)" != "$$(git rev-parse origin/master)" ]; then \
	  echo "❌ HEAD is not at origin/master — release-tag must run on master, in sync with origin"; exit 1; \
	fi; \
	notes="release-notes/$$v.md"; \
	if [ ! -f "$$notes" ]; then \
	  echo "❌ Release notes missing: $$notes"; exit 1; \
	fi; \
	if git rev-parse "refs/tags/$$v" >/dev/null 2>&1; then \
	  echo "❌ Tag '$$v' already exists"; exit 1; \
	fi; \
	echo "🏷️  Tagging $$v (message from $$notes)..."; \
	git tag -a "$$v" -F "$$notes" --cleanup=verbatim; \
	git push origin "$$v"; \
	echo "✅ Tagged and pushed $$v"

## release-build-linux: Build linux/amd64 binary (via Dockerfile.linux) with the release version baked into the filename
release-build-linux:
	@set -eu; \
	mkdir -p $(OUTPUT_DIR); \
	echo "🐧 Building peakbot $(RELEASE_VERSION) for Linux..."; \
	$(CONTAINER_BUILDER) build \
		--build-arg VERSION=$(RELEASE_VERSION) \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE_LINUX) \
		.; \
	chmod +x $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-linux-amd64; \
	echo "✅ $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-linux-amd64"; \
	ls -lh $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-linux-amd64

## release-build-windows: Build windows/amd64 binary (via Dockerfile.windows) with the release version baked into the filename
release-build-windows:
	@set -eu; \
	mkdir -p $(OUTPUT_DIR); \
	echo "🔨 Cross-compiling peakbot $(RELEASE_VERSION) for Windows..."; \
	$(CONTAINER_BUILDER) build \
		--build-arg VERSION=$(RELEASE_VERSION) \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE) \
		.; \
	echo "✅ $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-windows-amd64.exe"; \
	ls -lh $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-windows-amd64.exe

## release-build-macos: Build macOS universal2 binary (via Dockerfile.macos) with the release version baked into the filename
release-build-macos:
	@set -eu; \
	mkdir -p $(OUTPUT_DIR); \
	echo "🍎 Building peakbot $(RELEASE_VERSION) for macOS (universal2)..."; \
	$(CONTAINER_BUILDER) build \
		--build-arg VERSION=$(RELEASE_VERSION) \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE_MACOS) \
		.; \
	chmod +x $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-macos-universal2; \
	echo "✅ $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-macos-universal2"; \
	ls -lh $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-macos-universal2

## release-build-android: Build Android arm64 binary (via Dockerfile.android) with the release version baked into the filename
release-build-android:
	@set -eu; \
	mkdir -p $(OUTPUT_DIR); \
	echo "🤖 Building peakbot $(RELEASE_VERSION) for Android (arm64)..."; \
	$(CONTAINER_BUILDER) build \
		--build-arg VERSION=$(RELEASE_VERSION) \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE_ANDROID) \
		.; \
	chmod +x $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-android-arm64; \
	echo "✅ $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-android-arm64"; \
	ls -lh $(OUTPUT_DIR)/peakbot-$(RELEASE_VERSION)-android-arm64

## release-publish: Create a DRAFT Gitea release (name = version), upload
##              all 10 RELEASE_ASSETS, then finalize (draft:false). The
##              prerelease flag follows '-' in TAG (0.25.0-rc.1 → true).
##              Any failure DELETEs the draft. Requires TAG, GITEA_TOKEN and
##              release-notes/<TAG>.md; GITEA_URL/OWNER/REPO default to the
##              origin remote, OUTPUT_DIR stays overridable.
release-publish:
	@set -eu; \
	if [ -z "$(TAG)" ]; then \
	  echo "❌ TAG is not set — pass TAG=<version> (expected: $(RELEASE_VERSION))"; exit 1; \
	fi; \
	v="$(TAG)"; \
	if [ -z "$(GITEA_TOKEN)" ]; then \
	  echo "❌ GITEA_TOKEN is not set. Generate one at $(GITEA_URL)/user/settings/applications"; exit 1; \
	fi; \
	command -v jq >/dev/null   || { echo "❌ 'jq' is required";   exit 1; }; \
	command -v curl >/dev/null || { echo "❌ 'curl' is required"; exit 1; }; \
	notes="release-notes/$$v.md"; \
	if [ ! -f "$$notes" ]; then \
	  echo "❌ Release notes missing: $$notes (no fallback)"; exit 1; \
	fi; \
	for n in $(RELEASE_ASSETS); do \
	  case $$n in \
	    peakbot-*) p="$(OUTPUT_DIR)/$$n" ;; \
	    *) p="$(OUTPUT_DIR)/desktop/$$n" ;; \
	  esac; \
	  if [ ! -f "$$p" ]; then \
	    echo "❌ Missing release asset: $$p"; exit 1; \
	  fi; \
	done; \
	case $$v in *-*) prerelease=true ;; *) prerelease=false ;; esac; \
	api="$(GITEA_URL)/api/v1/repos/$(OWNER)/$(REPO)"; \
	echo "🚀 Creating draft Gitea release $$v (prerelease=$$prerelease) at $$api/releases ..."; \
	body=$$(jq -nc --arg tag "$$v" --arg name "$$v" --argjson pre "$$prerelease" --rawfile bd "$$notes" \
	  '{tag_name:$$tag, target_commitish:"", name:$$name, body:$$bd, draft:true, prerelease:$$pre}'); \
	resp=$$(curl -sS -X POST "$$api/releases" \
	  -H "Authorization: token $(GITEA_TOKEN)" \
	  -H "Content-Type: application/json" \
	  -d "$$body"); \
	rid=$$(echo "$$resp" | jq -r '.id // empty'); \
	if [ -z "$$rid" ]; then \
	  echo "❌ Failed to create draft release. Response:"; echo "$$resp" | jq .; exit 1; \
	fi; \
	echo "✅ Draft release id=$$rid"; \
	trap 'curl -sS -X DELETE "$$api/releases/$$rid" -H "Authorization: token $(GITEA_TOKEN)" >/dev/null 2>&1 || true; echo "🗑️  Deleted draft release $$rid (publish failed)"' EXIT; \
	for n in $(RELEASE_ASSETS); do \
	  case $$n in \
	    peakbot-*) f="$(OUTPUT_DIR)/$$n" ;; \
	    *) f="$(OUTPUT_DIR)/desktop/$$n" ;; \
	  esac; \
	  echo "⬆️  Uploading $$n ..."; \
	  up=$$(curl -sS -X POST "$$api/releases/$$rid/assets?name=$$n" \
	    -H "Authorization: token $(GITEA_TOKEN)" \
	    -F "attachment=@$$f"); \
	  url=$$(echo "$$up" | jq -r '.browser_download_url // empty'); \
	  if [ -z "$$url" ]; then \
	    echo "❌ Upload failed for $$n. Response:"; echo "$$up" | jq .; exit 1; \
	  fi; \
	  echo "    → $$url"; \
	done; \
	echo "📢 Finalizing release $$v (draft → published)..."; \
	fresp=$$(curl -sS -X PATCH "$$api/releases/$$rid" \
	  -H "Authorization: token $(GITEA_TOKEN)" \
	  -H "Content-Type: application/json" \
	  -d '{"draft":false}'); \
	if ! echo "$$fresp" | jq -e '.draft == false' >/dev/null 2>&1; then \
	  echo "❌ Failed to finalize release. Response:"; echo "$$fresp" | jq .; exit 1; \
	fi; \
	trap - EXIT; \
	echo "✅ Published $$v to $(GITEA_URL)/$(OWNER)/$(REPO)/releases/tag/$$v"

## release-publish-github: Publish the same 10 assets to the public GitHub
##              mirror (the auto-updater's download host). Waits for the tag
##              to arrive (push-mirror lag: GH_TAG_WAIT_TRIES ×
##              GH_TAG_WAIT_SECS), then: an existing PUBLISHED release with
##              all 10 assets → no-op exit; a leftover DRAFT → deleted;
##              otherwise create a DRAFT, upload all 10 assets (201 each),
##              PATCH draft:false + make_latest ("true" for stable tags,
##              "false" for '-'). Any failure DELETEs the draft. Requires
##              TAG + GH_RELEASE_TOKEN; the token is never printed.
release-publish-github:
	@set -eu; \
	if [ -z "$(TAG)" ]; then \
	  echo "❌ TAG is not set — pass TAG=<version> (expected: $(RELEASE_VERSION))"; exit 1; \
	fi; \
	v="$(TAG)"; \
	if [ -z "$(GH_RELEASE_TOKEN)" ]; then \
	  echo "❌ GH_RELEASE_TOKEN is empty — set the GitHub release token (Gitea secret GH_RELEASE_TOKEN)"; exit 1; \
	fi; \
	command -v jq >/dev/null   || { echo "❌ 'jq' is required";   exit 1; }; \
	command -v curl >/dev/null || { echo "❌ 'curl' is required"; exit 1; }; \
	notes="release-notes/$$v.md"; \
	if [ ! -f "$$notes" ]; then \
	  echo "❌ Release notes missing: $$notes (no fallback)"; exit 1; \
	fi; \
	for n in $(RELEASE_ASSETS); do \
	  case $$n in \
	    peakbot-*) p="$(OUTPUT_DIR)/$$n" ;; \
	    *) p="$(OUTPUT_DIR)/desktop/$$n" ;; \
	  esac; \
	  if [ ! -f "$$p" ]; then \
	    echo "❌ Missing release asset: $$p"; exit 1; \
	  fi; \
	done; \
	case $$v in *-*) prerelease=true; make_latest=false ;; *) prerelease=false; make_latest=true ;; esac; \
	api="$(GH_API)/repos/$(GH_MIRROR_REPO)"; \
	uploads="$(GH_UPLOADS)/repos/$(GH_MIRROR_REPO)"; \
	auth="Authorization: Bearer $(GH_RELEASE_TOKEN)"; \
	echo "⏳ Waiting for tag $$v on $(GH_MIRROR_REPO) (up to $(GH_TAG_WAIT_TRIES)×$(GH_TAG_WAIT_SECS)s)..."; \
	found=0; \
	i=1; \
	while [ "$$i" -le "$(GH_TAG_WAIT_TRIES)" ]; do \
	  code=$$(curl -sS -o /dev/null -w '%{http_code}' -H "$$auth" "$$api/git/ref/tags/$$v"); \
	  if [ "$$code" = "200" ]; then found=1; break; fi; \
	  [ "$$i" -lt "$(GH_TAG_WAIT_TRIES)" ] && sleep "$(GH_TAG_WAIT_SECS)"; \
	  i=$$((i+1)); \
	done; \
	if [ "$$found" -ne 1 ]; then \
	  echo "❌ Tag $$v never appeared on $(GH_MIRROR_REPO) after $(GH_TAG_WAIT_TRIES)×$(GH_TAG_WAIT_SECS)s (push-mirror lag?)"; exit 1; \
	fi; \
	resp=$$(curl -sS -H "$$auth" "$$api/releases/tags/$$v"); \
	if printf '%s' "$$resp" | jq -e '.id' >/dev/null 2>&1; then \
	  rid=$$(printf '%s' "$$resp" | jq -r '.id'); \
	  draft=$$(printf '%s' "$$resp" | jq -r '.draft'); \
	  if [ "$$draft" = "true" ]; then \
	    echo "🗑️  Deleting leftover draft release $$rid for $$v..."; \
	    curl -sS -X DELETE -H "$$auth" "$$api/releases/$$rid" >/dev/null; \
	  else \
	    missing=""; \
	    for n in $(RELEASE_ASSETS); do \
	      printf '%s' "$$resp" | jq -e --arg n "$$n" '[.assets[] | if type=="object" then .name else . end] | index($$n)' >/dev/null \
	        || missing="$$missing $$n"; \
	    done; \
	    if [ -z "$$missing" ]; then \
	      echo "✅ $$v already published on $(GH_MIRROR_REPO) with all 10 assets — nothing to do"; exit 0; \
	    fi; \
	    echo "❌ Published release for $$v is missing assets:$$missing — refusing to modify a published release (resolve manually)"; exit 1; \
	  fi; \
	fi; \
	echo "🚀 Creating draft GitHub release $$v (prerelease=$$prerelease) at $$api/releases ..."; \
	body=$$(jq -nc --arg tag "$$v" --arg name "$$v" --argjson pre "$$prerelease" --rawfile bd "$$notes" \
	  '{tag_name:$$tag, name:$$name, body:$$bd, draft:true, prerelease:$$pre}'); \
	resp=$$(curl -sS -X POST "$$api/releases" \
	  -H "$$auth" \
	  -H "Content-Type: application/json" \
	  -d "$$body"); \
	rid=$$(echo "$$resp" | jq -r '.id // empty'); \
	if [ -z "$$rid" ]; then \
	  echo "❌ Failed to create draft GitHub release. Response:"; echo "$$resp" | jq .; exit 1; \
	fi; \
	echo "✅ Draft release id=$$rid"; \
	trap 'curl -sS -X DELETE -H "$$auth" "$$api/releases/$$rid" >/dev/null 2>&1 || true; echo "🗑️  Deleted draft GitHub release $$rid (publish failed)"' EXIT; \
	for n in $(RELEASE_ASSETS); do \
	  case $$n in \
	    peakbot-*) f="$(OUTPUT_DIR)/$$n" ;; \
	    *) f="$(OUTPUT_DIR)/desktop/$$n" ;; \
	  esac; \
	  echo "⬆️  Uploading $$n ..."; \
	  code=$$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H "$$auth" \
	    -F "file=@$$f" "$$uploads/releases/$$rid/assets?name=$$n"); \
	  if [ "$$code" != "201" ]; then \
	    echo "❌ GitHub upload failed for $$n (HTTP $$code)"; exit 1; \
	  fi; \
	done; \
	echo "📢 Publishing GitHub release $$v (draft → published, make_latest=$$make_latest)..."; \
	fresp=$$(curl -sS -X PATCH "$$api/releases/$$rid" \
	  -H "$$auth" \
	  -H "Content-Type: application/json" \
	  -d "{\"draft\":false,\"make_latest\":\"$$make_latest\"}"); \
	if ! echo "$$fresp" | jq -e '.draft == false' >/dev/null 2>&1; then \
	  echo "❌ Failed to publish GitHub release. Response:"; echo "$$fresp" | jq .; exit 1; \
	fi; \
	trap - EXIT; \
	echo "✅ Published $$v to https://github.com/$(GH_MIRROR_REPO)/releases/tag/$$v"

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
	@echo "Release usage (bump → PR → merge → tag; CI runs 'make release TAG=<v>'):"
	@echo "  make release-bump VERSION=0.2.0   # cut release/0.2.0 from origin/master, bump, promote notes, push — open a PR"
	@echo "  make release-tag                  # after the PR merges: annotated tag from release-notes/<v>.md + push"
	@echo "  make release TAG=0.2.0            # full pipeline: release-check, build all 10 assets, sign updater,"
	@echo "                                    # publish to Gitea + the GitHub mirror"
	@echo "  make release TAG=0.2.0 PRUNE=1    # as above, pruning the container builder between platforms"
	@echo "  make desktop-updater              # sign the 3 updater artifacts + build latest.json (needs signing key)"
	@echo "  GITEA_TOKEN + GH_RELEASE_TOKEN    # required for the publish steps (env vars)"
	@echo ""
	@echo "Release notes: release-notes/<v>.md is required — Gitea/GitHub release body + git tag message (no fallback)."
	@echo ""
	@echo "Environment:"
	@echo "  CONTAINER_BUILDER  Container runtime (default: podman or docker)"
	@echo "  GITEA_TOKEN        Gitea personal access token (required for release-publish)"
	@echo "  GITEA_URL/OWNER/REPO  Auto-derived from origin remote — override if needed"
	@echo "  GH_RELEASE_TOKEN   GitHub release token (required for release-publish-github, never printed)"
	@echo "  GH_MIRROR_REPO     GitHub mirror owner/repo for latest.json + publishing (default: thepeak99/peakbot)"
	@echo "  TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)  Tauri updater signing key (required for desktop-updater)"
	@echo "  PRUNE              1 = prune the container builder between platform builds in the release recipe"
