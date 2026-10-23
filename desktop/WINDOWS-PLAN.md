# PeakBot Desktop for Windows — single self-extracting portable `.exe`

**Status:** LOCKED plan, waiting for user approval. **Nothing gets built, run or committed until the user approves.**
**Branch:** `feat/desktop-windows` (worktree `/var/home/exe/workz/peakhands/peakbot-desktop-windows`, base `7c7848f` = master after #385).
**Supersedes:** the uncommitted NSIS-installer WIP in that worktree (per-file keep/change/revert in §3).

---

## 0. SUMMARY

We ship **one file**, `PeakBot-Desktop-<v>-windows-x86_64.exe`. It is the normal Tauri desktop exe, cross-built on
Linux with `cargo xwin`, **without the Tauri CLI**. The macOS pipeline already works this way (Dockerfile.desktop-macos,
CI run 647). The `peakbot.exe` sidecar is built into the desktop exe as a zstd-compressed byte blob. On startup, only on
Windows, the app writes it once to
`%LOCALAPPDATA%\com.peakbot.desktop\bin\<version>-<sha12>\peakbot.exe` and points `DesktopPaths.sidecar` at that file.
Nothing else in the app changes: every sidecar use (`--version`, session spawn, `install`, `service …`, the #385
byte-compare) already goes through that one field.

**The key idea:** the sidecar's identity is `<version>-<sha12>`. build.rs computes it once from the staged sidecar bytes.
The extraction folder is named after it, and the container smoke test looks for that same string in the built exe. If
the identity matches, the file exists and is correct. If it doesn't match, it's a different folder. So we never need
runtime hashing, version checks, locks or overwrites.

### Goals
- G1 One downloadable file. Double-click it and it runs. No installer, no zip, no admin rights.
- G2 Runs on a clean Windows 10 22H2 / Windows 11 x64 with no VC++ redistributable. WebView2 Evergreen is assumed
  present (it is preinstalled on Windows 11 and on almost every Windows 10).
- G3 No console window ever appears: not for the desktop exe, not for any child process.
- G4 Linux and macOS behaviour does not change at all.
- G5 Built in the existing container pipeline (`build-desktop.sh windows|all`, Makefile, `desktop.yml`) and smoke-checked
  inside the container.

### Non-goals (v1)
- Code signing (unsigned, so SmartScreen shows "Run anyway"; see Q2). arm64. Auto-update. Start-menu shortcut or
  registration in Apps & Features. "Portable data" stored next to the exe (data stays in the normal per-user folders).
- Making background-service mode work on Windows (defect 4; see Q1). Replacing `wmic` and graceful Ctrl-C on Windows
  (defect 5; follow-up tickets).

### Artifact contract
`output/desktop/PeakBot-Desktop-<v>-windows-x86_64.exe`: a single PE32+ x86-64 **GUI-subsystem** executable. It does not
import `VCRUNTIME*.dll`, `MSVCP*.dll` or `WebView2Loader.dll`. It carries the sidecar whose SHA-256 begins with the
`<sha12>` in its embedded id. Expected size is about 22–25 MB: roughly 11 MB of sidecar at zstd -19 plus the Tauri app.

---

## 1. COMPONENTS

| # | Component | Where | Single responsibility |
|---|---|---|---|
| C1 | **Sidecar build** (existing recipe, msvc path) | `Dockerfile.windows` `TARGET=x86_64-pc-windows-msvc` | Produce `output/peakbot-windows-amd64-msvc.exe` with a **static CRT**. |
| C2 | **Payload packer** | `desktop/src-tauri/build.rs` (Windows targets only) | Read the staged sidecar, zstd-compress it into `OUT_DIR/peakbot.exe.zst`, emit `PEAKBOT_SIDECAR_ID=<pkg-version>-<sha12>`. |
| C3 | **Payload module** | `desktop/src-tauri/src/sidecar_payload.rs` (`#[cfg(windows)]`) | Own the blob (`include_bytes!`) and the id; supply the decode-into-file closure. About 20 lines. |
| C4 | **Extractor** | `desktop/core/src/extract.rs` (new, platform-neutral, unit-tested in CI) | Pure filesystem work: target path layout, atomic write-once, pruning of other versions. Knows nothing about zstd or Tauri. |
| C5 | **Path wiring** | `desktop/src-tauri/src/main.rs::build_paths` | On Windows: `sidecar = extracted path`. Elsewhere: unchanged. |
| C6 | **No-console fixes** | `main.rs` crate attribute; `core/src/sys.rs` single `command()` constructor | Desktop exe is GUI subsystem; every child is created with `CREATE_NO_WINDOW`. |
| C7 | **Desktop image** | `Dockerfile.desktop-windows` (rewrite) | `cargo xwin build … --features tauri/custom-protocol`, rename, smoke, scratch export. |
| C8 | **Smoke** | `desktop/scripts/smoke-windows.sh` (rewrite) | S-W1..S-W4 on the real artifact, inside the container. Hard-fail only, no SKIP paths. |

**Why the split between C3 and C4:** the extractor's logic (atomicity, pruning, failure paths) is the part that can be
wrong, so it lives in `core`, which CI tests on Linux with a temp dir. The zstd/`include_bytes!` part is glue that can
only be checked on Windows. It is kept to a few lines with no branches.

**Deliberately NOT added (and why):**
- **No `embed-sidecar` cargo feature.** The payload is needed exactly when the target is Windows, so `cfg(windows)` /
  `TARGET` *is* the switch. A feature flag would add a second axis whose only legal value on Windows is "on".
- **No Tauri CLI, NSIS, Node, appindicator, 7z.** The macOS precedent shows that tauri-build + `custom-protocol` does
  everything the CLI did for us (ACL, externalBin check, icon/version `.rc`, static vcruntime, embedded `ui/`).
- **No `TAURI_CONFIG` override** to drop `externalBin`. That path is unverified. Staging the file satisfies tauri-build
  and is also the `include_bytes!` source.
- **No `Sys`-trait seam for the extractor.** `Sys` abstracts processes and HTTP. Filesystem tests against a real temp dir
  are simpler and follow the existing pattern (`core/tests/real_sys.rs`, `tempfile` dev-dep).
- **No runtime hash verification and no zstd frame checksum.** The final file name only appears after an atomic rename
  of a fully written and synced file, and the blob's integrity is the exe's own integrity. If something external damages
  the file later, the existing `SidecarBroken` / `SidecarMissing` views report it.
- **No new `StartError` variant for extraction failure.** On failure the path simply doesn't exist, so the existing
  `SidecarMissing { path }` view appears and the cause goes to the log (Q4).
- **No pid-unique temp names, no "already exists" race handling.** The single-instance plugin creates its mutex at plugin
  init, before `.setup`, and secondaries exit there. A different user has a different `%LOCALAPPDATA%`. So two writers to
  the same folder cannot exist (invariant I3).
- **No "keep last 2" retention policy.** We delete every sibling folder, best-effort. Windows refuses to delete a running
  image, so a folder that is in use survives on its own and is retried on the next launch.
- **Not merging the backend into the desktop binary, and no generic SFX wrapper.** Both rejected per research: the
  GUI-vs-console subsystem conflict, the installed copy being the user's `peakbot` CLI, the #385 byte compare, and temp
  dirs / wrapper processes.

---

## 2. DATA MODEL

```
PEAKBOT_SIDECAR_ID : &'static str   = "<CARGO_PKG_VERSION>-<first 12 hex of SHA-256(uncompressed sidecar)>"
                                       e.g. "0.23.0-3fa1c09be27d"      (compile-time, build.rs → rustc-env)
BLOB               : &'static [u8]  = include_bytes!(OUT_DIR/peakbot.exe.zst)   (zstd level 19, single frame)

bin_root   = app_local_data_dir()/bin            = %LOCALAPPDATA%\com.peakbot.desktop\bin
payload    = bin_root/<ID>/peakbot.exe            (core::extract::payload_path)
temp file  = bin_root/<ID>/peakbot.exe.tmp        (only during extraction)
```

**Invariants**
- **I1 Identity = content.** A folder named `<ID>` only ever contains the sidecar whose hash prefix is `<ID>`. The ID is
  derived from the bytes in one place (build.rs) and never written down by hand anywhere.
- **I2 Complete or absent.** `payload` exists only after `write → sync_all → rename` has finished. A crash leaves at most
  `peakbot.exe.tmp`, which the next attempt overwrites.
- **I3 Single writer.** Extraction runs in `.setup`, after the single-instance plugin has already made secondaries exit.
  Different Windows users have different `%LOCALAPPDATA%`.
- **I4 Never overwritten.** An existing `payload` is never rewritten. That is safe because of I1, and it is required
  because a running sidecar locks its image.
- **I5 Linux/macOS untouched.** No payload code is compiled for non-Windows targets. The core module is compiled but
  only called on Windows (and in its tests).
- `%LOCALAPPDATA%` (Local), not `app_data_dir` (Roaming): a 40 MB binary must not roam. Not `%TEMP%`: cleaners wipe it,
  and `resolve_service_exe` could end up pointing into it.

---

## 3. PER-FILE CHANGE LIST

Legend: **KEEP** = WIP as-is · **CHANGE** = keep WIP and edit · **REWRITE** = replace WIP content · **NEW** · **EDIT** = tracked file not in WIP · **NO CHANGE** = deliberately untouched.

### 3.1 WIP files (from `git status`)

| File | Verdict | What |
|---|---|---|
| `Dockerfile.windows` | **CHANGE** | Keep the WIP gnu/msvc dual path; the gnu default stays byte-for-byte the same in behaviour. Add `ENV CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static"` (target-scoped, so gnu ignores it and no shell branching is needed; it applies to the dep-cache build and the real build alike). Pin `cargo install --locked cargo-xwin@0.23.1`. Update the header comment ("static CRT — no VC++ redist needed"). Defect 3. |
| `.dockerignore` | **KEEP** | The `!output/peakbot-windows-amd64-msvc.exe` allowlist and its comment are still correct. |
| `Makefile` | **CHANGE** | Keep the target, `.PHONY` and help lines. Help text becomes: `desktop-windows: Build the Windows x86_64 portable exe (→ output/desktop/PeakBot-Desktop-<v>-windows-x86_64.exe)`. |
| `desktop/scripts/build-desktop.sh` | **CHANGE** | Keep the structure (`build_sidecar_windows`, `build_desktop_windows`, `build_windows`, `all`). Artifact name drops `-setup`. Header comment step 2 becomes "build the portable exe with Dockerfile.desktop-windows (cargo-xwin, no Tauri CLI; sidecar embedded) → …-windows-x86_64.exe (S-W1..S-W4 run before export)". Message "NSIS installer" → "portable exe". |
| `.gitea/workflows/desktop.yml` | **CHANGE** | Keep `timeout-minutes: 180`. Job/step names: "Windows NSIS" → "Windows exe". |
| `Dockerfile.desktop-windows` | **REWRITE** | See §3.3. |
| `desktop/scripts/smoke-windows.sh` | **REWRITE** | See §3.4. |
| `desktop/scripts/test-build-desktop-windows.sh` | **REWRITE** | See §5.2. Keep the harness (`ok`/`bad`, `make_fake_pe`, `run_sw`), make `make_fake_pe` take a subsystem byte, renumber. |
| `desktop/DESIGN.md` | **CHANGE** | **Keep** WIP hunks @915/@920 (sidecar staging, msvc variant), add "built with static CRT" to them; keep the build-desktop.sh / Makefile / desktop.yml hunks (@960/@962) with `-setup` removed. **Revert** the WIP `Dockerfile.desktop-windows` block (@940, incl. NSIS + appindicator gotchas), the S-W list (@1263) and §9.5 (@1308). **Replace** them with the content of this plan: §5.7 Windows block, §9.2 S-W1..S-W4, §9.5 W1..W12, plus a new section "Amendment (windows-portable) — single self-extracting exe (2026-10-17)" holding §0–§2 of this plan in condensed form. §3.7: document the Windows `sidecar` source. |
| `desktop/README.md` | **CHANGE** | **Keep** the Windows token path (`%APPDATA%\peakbot\peakbot\config\web-token`) and the settings path (`%APPDATA%\com.peakbot.desktop\settings.json`) hunks. **Building table:** artifact → `…-windows-x86_64.exe`. **Delete** the "Windows build gotchas" paragraph. **Rewrite `## Windows`:** (a) single exe, no install, run it from anywhere; (b) unsigned → "Windows protected your PC" → More info → Run anyway; (c) WebView2 required (preinstalled on Win11/most Win10; if it's missing the app shows a dialog with the download link); (d) first launch unpacks the backend to `%LOCALAPPDATA%\com.peakbot.desktop\bin\` (~40 MB; older versions are removed automatically); (e) background mode, per Q1. **Rewrite the Windows removal bullet:** quit, delete the exe, optionally delete `%LOCALAPPDATA%\com.peakbot.desktop` and `%APPDATA%\com.peakbot.desktop` (+ the Q1-dependent schtasks line). |

### 3.2 Tracked files (not in WIP)

| File | Verdict | What |
|---|---|---|
| `desktop/core/src/extract.rs` | **NEW** | §4.2 API, about 50 lines plus tests. |
| `desktop/core/src/lib.rs` | **EDIT** | `pub mod extract;` |
| `desktop/core/src/sys.rs` | **EDIT** | Defect 2: one private `fn command(program: impl AsRef<OsStr>) -> Command` that, under `#[cfg(windows)]`, calls `CommandExt::creation_flags(CREATE_NO_WINDOW)` (`const CREATE_NO_WINDOW: u32 = 0x0800_0000;`). **All** `Command::new` sites in `sys.rs` use it: `run`, `spawn`, `ps`, `wmic`, `taskkill`. Fixing the whole bug class at once matters because `wmic` and `taskkill` would also flash consoles. |
| `desktop/src-tauri/src/main.rs` | **EDIT** | Defect 1: first line `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`. `#[cfg(windows)] mod sidecar_payload;`. `build_paths`: §4.3. The resource-dir fallback becomes `#[cfg(not(windows))]`, so the missing-`.exe` dev-fallback bug (main.rs:226-228) is unreachable by construction. Update the `build_paths` doc comment. |
| `desktop/src-tauri/src/sidecar_payload.rs` | **NEW** | §4.3, `#[cfg(windows)]` (gated at the `mod` line). |
| `desktop/src-tauri/build.rs` | **EDIT** | C2, §4.1. |
| `desktop/src-tauri/Cargo.toml` | **EDIT** | `[build-dependencies]` add `zstd = "0.13"`, `sha2 = "0.10"` (sha2 0.10.9 is already in the lock). `[target.'cfg(windows)'.dependencies]` add `ruzstd = "0.8"`. |
| `desktop/Cargo.lock` | **EDIT** | Regenerated once (`cargo generate-lockfile` is NOT allowed; use `cargo update -p peakbot-desktop` / a plain `cargo check` to add only the new packages), then committed. All builds stay `--locked`. |
| `desktop/core/src/paths.rs` | **EDIT (doc only)** | The `DesktopPaths.sidecar` doc currently says "`sidecar_path(current_exe)`", which is a misleading comment. Change it to "the sidecar the app runs: next to the exe in Linux/macOS bundles, `binaries/peakbot-<triple>` in dev, the extracted payload on Windows (`extract::payload_path`)". |
| `desktop/src-tauri/tauri.conf.json` | **NO CHANGE** | `externalBin` stays (tauri-build checks it on every target; the CLI-only `bundle.targets` is irrelevant without the CLI). |
| `controller.rs`, `service.rs`, `session.rs`, `phase.rs`, backend `src/` | **NO CHANGE** | Single-field swap (research). |

### 3.3 `Dockerfile.desktop-windows` (rewrite), locked content

```
FROM ghcr.io/rust-lang/rust:1.91-bookworm AS build     # same toolchain image as Dockerfile.windows (sidecar)
apt: clang lld llvm file                                # xwin C/link + llvm-rc (tauri_winres) + llvm-objdump (smoke)
RUN llvm-rc --version >/dev/null && llvm-objdump --version >/dev/null   # guard: fail fast with a clear message
rustup target add x86_64-pc-windows-msvc
ENV XWIN_ACCEPT_LICENSE=1 XWIN_CACHE_DIR=/root/.cache/cargo-xwin CARGO_BUILD_JOBS=8
cargo install --locked cargo-xwin@0.23.1
(keep WIP) MSVC SDK pre-warm layer (throwaway `cargo xwin build`)
ARG VERSION
COPY Cargo.toml /src/Cargo.toml ; COPY desktop/ /src/desktop
ARG SIDECAR=output/peakbot-windows-amd64-msvc.exe
COPY ${SIDECAR} /src/desktop/src-tauri/binaries/peakbot-x86_64-pc-windows-msvc.exe
RUN sh /src/desktop/scripts/check-version.sh
WORKDIR /src/desktop
RUN cargo xwin build --release --locked -p peakbot-desktop \
        --target x86_64-pc-windows-msvc --features tauri/custom-protocol
RUN mkdir -p /out && cp target/x86_64-pc-windows-msvc/release/peakbot-desktop.exe \
        /out/PeakBot-Desktop-${VERSION}-windows-x86_64.exe
RUN sh scripts/smoke-windows.sh /out/PeakBot-Desktop-${VERSION}-windows-x86_64.exe ${VERSION} \
        src-tauri/binaries/peakbot-x86_64-pc-windows-msvc.exe
FROM scratch
COPY --from=build /out/ /
```
If the spike shows Debian's `llvm` package lacks the unversioned `llvm-rc`/`llvm-objdump`, the guard line becomes
`ln -s` to the `-14` names. That is a spike outcome to record, not a design change.

### 3.4 `smoke-windows.sh <exe> <version> <sidecar>`, locked checks

Any other argument count → usage, exit 2. Missing `file` / `llvm-objdump` / `sha256sum` → fail. Missing exe or sidecar →
`not found`, exit 1. **There are no SKIP paths.**
- **S-W1** basename is exactly `PeakBot-Desktop-<version>-windows-x86_64.exe`.
- **S-W2** `file -b <exe>` matches PE32+ … x86-64 … MS Windows (both word orders, as today) **and** contains `(GUI)`.
  `file -b <sidecar>` matches PE32+ … x86-64.
- **S-W3** for each of exe and sidecar: `llvm-objdump -p` exits 0 and prints at least one `DLL Name:` line (so the check
  can't pass on an empty or unparsed import table). No line matches `-i 'DLL Name: *\(vcruntime\|msvcp\)'`. Exe only:
  no `WebView2Loader.dll`.
- **S-W4** exe size > 10 MB (the payload is present) **and** `grep -aqF "<version>-$(sha256sum <sidecar> | cut -c1-12)" <exe>`
  (the embedded payload id equals the staged sidecar's hash). The sidecar in S-W3 is therefore exactly the payload's
  content.

---

## 4. DATA FLOW / CONTROL FLOW, AND INTERFACES

### 4.1 Build time (`build.rs`, runs on the Linux host)
```
TARGET contains "windows"?  no → unchanged (DESKTOP_TARGET_TRIPLE + tauri_build) ; yes ↓
src = "binaries/peakbot-{TARGET}.exe"; println!("cargo:rerun-if-changed={src}")
bytes = fs::read(src).expect("staged sidecar missing: {src} (build-desktop.sh stages it; DESIGN §5.7)")
id    = format!("{CARGO_PKG_VERSION}-{hex(sha256(bytes)[..6])}")
fs::write(OUT_DIR/peakbot.exe.zst, zstd::encode_all(&bytes[..], 19))
println!("cargo:rustc-env=PEAKBOT_SIDECAR_ID={id}")
then the existing tauri_build::try_build(...)
```
Level **19**: 11.3 MB against 14.6 MB at -3 (research). It costs about 20 s of build-script time once per sidecar change,
and decoding is just as fast. xz -9e saves another 1.2 MB but would need a second, slower decoder, so it's rejected.
Long-distance / `--ultra` modes are not used, so the window stays ≤ 8 MB and ruzstd handles it.

### 4.2 `core::extract` (new). This is the seam the developer builds to.
```rust
/// bin_root/<id>/peakbot.exe
pub fn payload_path(bin_root: &Path, id: &str) -> PathBuf;

/// Write-once: if `target` is a file → Ok(()) without calling `write`.
/// Else: create_dir_all(parent); File::create(<target>.tmp) (truncates a crash leftover);
/// write(&mut file)?; file.sync_all()?; rename(tmp → target), retried up to 10× at 100 ms
/// (Defender briefly holds freshly written executables → sharing violation).
/// On any error: best-effort remove tmp, return the error. Target is never left partial.
pub fn ensure_extracted(target: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()>;

/// Best-effort: remove every *directory* in target.parent().parent() other than target.parent().
/// Errors are ignored (an in-use older image can't be deleted on Windows; retried next launch).
pub fn prune_siblings(target: &Path);
```
`prune_siblings` derives `bin_root` and "keep" from `target` instead of taking them as arguments, so there is one source
of truth.

### 4.3 Startup (Windows). Glue in `main.rs` + `sidecar_payload.rs`
```
main() ─ single-instance plugin init (secondary → focus primary, exit)        ← I3 established here
       ─ .setup ─ build_paths ─┐
                               │ #[cfg(windows)]
                               │ bin_root = app.path().app_local_data_dir()?.join("bin")
                               │ sidecar  = extract::payload_path(&bin_root, sidecar_payload::ID)
                               │ if let Err(e) = extract::ensure_extracted(&sidecar, sidecar_payload::write_to) {
                               │     log::error!("could not unpack the PeakBot backend to {}: {e}", sidecar.display());
                               │ }
                               │ extract::prune_siblings(&sidecar);
                               │ #[cfg(not(windows))]  → today's code (sibling, else dev resource dir)
                               └→ DesktopPaths { sidecar, … } → window, tray, controller → Action::Start
```
`sidecar_payload.rs`:
```rust
pub const ID: &str = env!("PEAKBOT_SIDECAR_ID");
static BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/peakbot.exe.zst"));
pub fn write_to(f: &mut File) -> io::Result<()> {
    let mut d = ruzstd::decoding::StreamingDecoder::new(BLOB).map_err(io::Error::other)?;
    io::copy(&mut d, f).map(drop)
}
```
A `peakbot.exe` sitting next to the desktop exe is **ignored** on Windows. That is intentional: the payload is the single
source.

**Failure paths (all user-visible through existing views; the cause is always in `desktop.log`)**

| Failure | What happens | User sees |
|---|---|---|
| Disk full / ACL / AV blocks the write or rename | `ensure_extracted` → Err, logged; `sidecar` doesn't exist | `--version` NotFound → **SidecarMissing** "The PeakBot backend is missing from this app" + the path, [Quit]. The next launch tries again. |
| Crash or power loss mid-write | only `.tmp` is left (I2) | The next launch overwrites it; normal start. |
| AV quarantines `peakbot.exe` after extraction | file gone | This launch: SidecarMissing. The next launch re-extracts (and AV may quarantine it again; see R5). |
| Extracted file corrupted by a third party | `--version` fails | **SidecarBroken** [Retry, Logs, Quit]. Fix: delete the `bin` folder (README). |
| `app_local_data_dir()` fails (no profile) | setup `?` → app panics (same as today's `app_data_dir`) | Silent exit. Accepted: not reachable on a real user session. |
| WebView2 missing | tauri-runtime-wry modal with download link, then exit | Modal (research). |
| Old version's folder in use | prune skips it | Nothing. Removed on a later launch. |

**#385 auto-replace and service install** work unchanged. `<sidecar> install` copies its own image (the extracted file)
to `%LOCALAPPDATA%\Programs\peakbot\peakbot.exe`, and the replace decision compares contents, not paths.

---

## 5. TEST STRATEGY

### 5.1 Rust unit tests: `core/src/extract.rs` `#[cfg(test)]`, `tempfile` (run in CI `ci.yml` "Desktop core")
Written first (RED), as in the existing core TDD flow:
- **X1** `payload_path(root, "0.23.0-abc")` == `root/0.23.0-abc/peakbot[.exe]` (uses `sidecar_file_name()`).
- **X2** target missing → `write` is called exactly once; the target has the written bytes; there is no `.tmp` sibling;
  the parent dirs are created.
- **X3** target exists → `write` is **not** called (closure panics if invoked); the content is unchanged.
- **X4** `write` returns Err → Err propagated; target absent; `.tmp` absent.
- **X5** a stale `peakbot.exe.tmp` with junk exists → success, target == new bytes, no `.tmp`.
- **X6** `prune_siblings`: root has `0.22.0-aaa/peakbot.exe`, `0.23.0-bbb/peakbot.exe` (current), `stray.txt` → `0.22.0-aaa`
  removed, current kept, `stray.txt` kept.
- **X7** `prune_siblings` when `bin_root` does not exist → no panic.

The rename-retry path (a Windows sharing violation) is not unit-tested. It is covered by the manual W2 check.
`sys::command()` and `windows_subsystem` can't be tested on Linux. They are covered by host greps (T29/T30) and manual W1.

### 5.2 Host static tests: `test-build-desktop-windows.sh` (rewrite; no docker, sh + file only)
- **A build-desktop.sh:** T01–T05 kept as they are; T06 references `Dockerfile.desktop-windows`; T07 passes
  `TARGET=x86_64-pc-windows-msvc`; T08 expects `peakbot-windows-amd64-msvc.exe`; T09 expects
  `PeakBot-Desktop-$VERSION-windows-x86_64.exe` and contains **no** `-setup`.
- **B Dockerfile.desktop-windows:** T10 exists; T11 `cargo xwin build`; T12 `--target x86_64-pc-windows-msvc`;
  T13 `--features tauri/custom-protocol`; T14 `--locked`; T15 `cargo-xwin@0.23.1`; T16 `check-version.sh`;
  T17 `smoke-windows.sh` invoked with the staged sidecar path; T18 stages `peakbot-x86_64-pc-windows-msvc.exe`;
  T19 exports `windows-x86_64.exe`; T20 `FROM scratch`; T21 contains none of `@tauri-apps/cli`, `nsis`, `appindicator`,
  `nodesource` (guards against regressing to the abandoned path).
- **C Dockerfile.windows:** T22 msvc target; T23 `cargo-xwin@0.23.1`; T24 `peakbot-windows-amd64-msvc.exe`;
  T25 `CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS` with `+crt-static`.
- **D Makefile:** T26 target, T27 `.PHONY`, T28 help line.
- **E source guards** (defects 1–3 have no Windows CI, so these are the regression tripwires): T29 `main.rs` has
  `windows_subsystem = "windows"`; T30 `sys.rs` has `CREATE_NO_WINDOW` and `creation_flags`, and `Command::new` appears
  exactly once in non-test code (inside `command()`); T31 `build.rs` emits `PEAKBOT_SIDECAR_ID`.
- **F smoke-windows.sh** (fake artifacts): T32 exists and is executable; T33 `sh -n`; T34 0/2/4 args → exit 2 + usage;
  T35 missing exe → `not found`; T36 wrong name → fails at S-W1; T37 correct name, text content → fails at S-W2;
  T38 fake PE32+ with subsystem 3 (console) → fails at S-W2; T39 fake PE32+ with subsystem 2 (GUI) + fake sidecar →
  output contains `✅ S-W2` (later checks need llvm-objdump and are proven for real in the container).

### 5.3 In-container smoke
S-W1..S-W4 (§3.4) on the real artifact. Any failure fails `docker build`, and so fails `build-desktop.sh` and CI.

### 5.4 Manual Windows checklist (human; DESIGN §9.5, replaces the NSIS one)
Use **Windows Sandbox** (Win11 Pro; clean: no VC++ redist, WebView2 present) and one real Win10 22H2 machine.
- **W1** Download and double-click → SmartScreen → More info → Run anyway. **No console window ever appears** (startup,
  sidecar spawn, settings change restart, quit).
- **W2** First run: `%LOCALAPPDATA%\com.peakbot.desktop\bin\<v>-<sha12>\peakbot.exe` exists, no `.tmp`; the window loads
  the UI; Task Manager → Details shows `peakbot-desktop.exe` and `peakbot.exe` (path in that folder, `--bind 127.0.0.1:7824`).
- **W3** In Sandbox: everything in W2 works, which proves the static CRT (no VC++ redist there).
- **W4** Launch again while it's running → the existing window is focused; there's no second process and no re-extraction.
- **W5** Quit and relaunch → the `peakbot.exe` modified time is unchanged (no re-extraction).
- **W6** Run the exe from a read-only folder (e.g. `C:\Program Files\PeakBotTest\` copied as admin, run as a normal user)
  and from Downloads → both work.
- **W7** Run a build with a different `<sha12>` (next version, or a local rebuild) → a new folder appears and the old one
  is removed.
- **W8** Delete `bin\` while the app is closed → the next launch re-extracts and works.
- **W9** Close the window → `peakbot.exe` is gone within 3 s.
- **W10** Tray menu; Ctrl+Shift+S / tray Settings…; protocol/token change → the sidecar restarts with no console flash.
- **W11** Background toggle behaves per the Q1 decision (default: not shown on Windows).
- **W12** Removal per README: delete the exe and the two data folders → nothing PeakBot is left running or scheduled.

---

## 6. KEY TRADEOFFS

| Decision | Chosen | Rejected | Why |
|---|---|---|---|
| Packaging | Sidecar embedded, extracted by the app itself | NSIS installer (user said no); zip (user said no); generic SFX (7-Zip/warp/Enigma); one merged binary | Research: SFX means temp extraction + a wrapper process + `current_exe` in temp. Merging breaks the subsystem, the CLI role, the lockfile and #385. |
| Build | `cargo xwin build --features tauri/custom-protocol` | Tauri CLI | macOS precedent (run 647). Drops Node/NSIS/appindicator and both "gotchas". |
| Gating | `cfg(windows)` / `TARGET` | `embed-sidecar` feature | The feature would always be on for Windows, so it's a redundant axis. |
| Extraction dir | `%LOCALAPPDATA%\com.peakbot.desktop\bin\<v>-<sha12>` | `%TEMP%`; Roaming; next to the exe | Research: temp gets cleaned and the service fallback could point there; Roaming would sync 40 MB; next to the exe may be read-only. The hash name avoids lock conflicts with a running image. |
| Where it runs | `.setup` / `build_paths`, before the window | In the controller thread; lazily before the first spawn | After the single-instance mutex (research) gives one writer. One place, no core control-flow change. Cost: about 0.3–1 s first-run delay, once per version. |
| Decoder | **ruzstd** (pure Rust) at runtime; **zstd** (C) at build time on the host | zstd C crate at runtime | The desktop exe's CRT comes from tauri-build's static-vcruntime link tricks. C code compiled by `cc` under xwin would add a second CRT question to exactly the area defect 3 is about. Pure Rust removes it. The host build-dep needs only the Linux cc. The format is standard, and the round-trip is checked in the spike. |
| Level | zstd 19 | -3 (14.6 MB), xz -9e (10.1 MB) | Best ratio our one decoder supports; decoding speed doesn't depend on level. |
| Build-deps | unconditional `zstd` + `sha2` | target-specific build-deps / feature | build.rs is a host program and can't `cfg` on the target, so conditional deps would need a feature (rejected above). Cost: Linux/macOS builds compile zstd-sys once (~20 s), with no behaviour change. |
| Sidecar CRT | `+crt-static` via the target-scoped env | Ship the VC++ redist; gnu sidecar | Research defect 3. A single exe can't carry the redist. The gnu sidecar is the fallback if the spike fails (R1). |
| Failure UX | Reuse SidecarMissing + log | New `StartError::SidecarExtract` | YAGNI for a rare path. The message is true (the backend is missing at that path). See Q4. |
| Cleanup | Delete all sibling version dirs | Keep N | Simpler, and the OS protects in-use images. |

---

## 7. IMPLEMENTATION PLAN (ordered)

Effort: 🟢 junior-safe (mechanical) · 🟡 mid · 🔴 senior judgment. **Commits** are marked; the arrows give the required order.

**S0 🔴 Spike (throwaway, no commit; results go into the DESIGN amendment).** Gate for everything below.
- a) `Dockerfile.windows` msvc + `+crt-static` builds; `llvm-objdump -p` on the sidecar shows no `VCRUNTIME140.dll`
  (checks that ring / aws-lc-sys / zstd-sys build under /MT). **If it fails:** stop and decide with the user. Fallback:
  the existing gnu sidecar recipe, after checking its imports.
- b) Desktop `cargo xwin build … --features tauri/custom-protocol` on `rust:1.91-bookworm` works without the CLI
  (`llvm-rc` found, icon/version resource present); the exe imports no VCRUNTIME/WebView2Loader; `file` says `(GUI)`
  once the attribute is in. **If static vcruntime doesn't apply cross-host:** add the same target-scoped `+crt-static`
  env to the desktop image and re-check.
- c) Host round-trip: `zstd::encode_all(sidecar, 19)` → `ruzstd::StreamingDecoder` → sha256 equals the input.

**Commit 1: `fix(desktop): no console windows on Windows`** (defects 1+2 *must* land together: with the GUI subsystem
alone, every child would get its own console window, which is worse than today)
1. 🟡 `sys.rs`: `command()` constructor with `CREATE_NO_WINDOW`; replace all 5 `Command::new` sites.
2. 🟢 `main.rs`: `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`.
3. 🟢 Host tests T29, T30 (land in this commit inside the rewritten test script skeleton, or in Commit 4 if the script is
   rewritten wholesale; either way they must exist before the PR merges).

**Commit 2: `build(windows): msvc sidecar with static CRT`** (independent; gnu default unchanged)
4. 🟢 `Dockerfile.windows` per §3.1 (WIP + `CARGO_TARGET_…_RUSTFLAGS` + pinned cargo-xwin); `.dockerignore` keep.

**Commit 3: `feat(desktop): embedded Windows sidecar payload`** (3a–3c land together; extract.rs with no caller would be
dead code)
5. 🟡 `core/src/extract.rs` tests X1–X7 (RED), then the implementation (GREEN); `lib.rs` `pub mod extract;`.
6. 🔴 `build.rs` §4.1; `Cargo.toml` deps; `Cargo.lock` (add only the new packages; review the lock diff:
   `zstd`, `zstd-safe`, `zstd-sys`, `ruzstd` + their deps only).
7. 🟡 `sidecar_payload.rs` + `build_paths` §4.3; `paths.rs` doc fix; `cfg(not(windows))` on the dev fallback.

**Commit 4: `build(desktop): Windows single-exe pipeline`** (→ after Commit 3; packaging without the payload would ship
a broken exe)
8. 🟢 `Dockerfile.desktop-windows` rewrite per §3.3.
9. 🟡 `smoke-windows.sh` rewrite per §3.4.
10. 🟢 `test-build-desktop-windows.sh` rewrite per §5.2 (incl. T31; T29/T30 if they weren't in Commit 1); run it, all green.
11. 🟢 `build-desktop.sh`, `Makefile`, `desktop.yml` edits per §3.1.
12. 🟢 `DESIGN.md` + `README.md` per §3.1 (the README Windows background line follows Q1).

**Verify (no commit):**
13. 🟡 `sh desktop/scripts/build-desktop.sh windows` locally → S-W1..S-W4 green; `make desktop-test` green;
    `cargo fmt/clippy` for core green; Linux and macOS builds still green (`build-desktop.sh all` in CI via workflow_dispatch).
14. 🔴 Human: §5.4 W1–W12 on Windows Sandbox + Win10. Release blocks on this.

**Separate PR (only if Q1 = hide):** 🟡 hide the background row/tray item on Windows (`cfg!(windows)` in the tray model
and the settings view), plus its core test.

---

## 8. RISKS & OPEN QUESTIONS

### Risks
| # | Risk | Likelihood | Mitigation |
|---|---|---|---|
| R1 | A C dep (aws-lc-sys, ring, zstd-sys) doesn't build or link under `+crt-static` with xwin | Med | Spike S0a first; S-W3 enforces it. Fallback: gnu sidecar (decision with the user). |
| R2 | tauri-build's static vcruntime doesn't apply when cross-compiling, so the desktop exe imports VCRUNTIME140 | Low–Med | S0b; S-W3 catches it; fallback is the target-scoped `+crt-static` on the desktop build. |
| R3 | Debian bookworm `llvm` lacks unversioned `llvm-rc`/`llvm-objdump` | Med | Guard `RUN` fails fast; symlink the `-14` names. |
| R4 | ruzstd decode mismatch with a zstd-19 frame | Low | S0c round-trip; W2 every release. |
| R5 | Defender / SmartScreen: unsigned outer exe warns; the extracted inner exe may be flagged or quarantined; Smart App Control (enforce) blocks unsigned outright | Med | README documents "Run anyway". The inner exe has no Mark-of-the-Web and is launched by CreateProcess (no SmartScreen). A false-positive quarantine shows SidecarMissing; we'd report it to Microsoft. Real fix = signing (Q2). |
| R6 | First-run delay (decompress + write 40 MB + AV scan on first exec) | Certain, small | ~0.3–1 s before the window, once per version; `RUN_TIMEOUT` is 60 s, so an AV scan can't time out `--version`. |
| R7 | `resolve_service_exe` fallback (src/main.rs:299-319) points the service at the extraction dir if `install` was skipped, and prune later deletes it | Low | Flows always run `install` first; Windows can't delete a running image; moot if Q1 = hide. |
| R8 | Disk growth if prune keeps failing | Low | In-use folders are released when the process ends; deleted on the next launch. |
| R9 | No Windows CI | Certain | Host greps T29–T31 + container smoke + mandatory manual checklist. |
| R10 | Orphan sidecar from a crashed run isn't reaped (wmic gone on Win11 24H2), so its port is taken | Existing, unchanged | Defect 5 follow-up ticket. Not caused by this design. |

### Open questions
- **Q1 BLOCKING for release (not for building): background-service toggle on Windows.** Today it's non-functional
  (schtasks has no `/Run`, `run_state` is Unknown, a console window appears at logon). Showing it breaks "advertise
  nothing you don't deliver". **Default: hide it on Windows in v1** (separate small PR), with a follow-up ticket to make it
  work (hidden window + `/Run`).
- **Q2 NON-BLOCKING: code signing.** **Default: unsigned v1**, with SmartScreen documented. Signing (OV/EV or Azure
  Trusted Signing) is a follow-up; it's the only fix for Smart App Control and the AV false-positive risk.
- **Q3 BLOCKING (confirm): include defects 1–3 in this work.** **Default: yes.** Without them the exe shows console
  windows (1, 2) or fails to start on machines without the VC++ redist (3), so it doesn't "work".
- **Q4 NON-BLOCKING: extraction-failure UX.** **Default:** reuse SidecarMissing (+ the cause in the log). Alternative: a
  dedicated "Could not unpack the backend: <reason>" view with [Retry], about +20 lines in core.
- **Q5 NON-BLOCKING: defect 5** (`wmic` replacement, graceful stop on Windows). **Default:** separate tickets.
- **Q6 NON-BLOCKING: arm64 Windows.** **Default:** not in v1 (x64 runs emulated on Windows-on-Arm).

---

## 9. ZEN REVIEW (applied to this plan; the cuts are already reflected above)

**TL;DR:** It's the macOS pipeline pointed at Windows, plus one write-once file copy keyed by a content hash. One new
core module (about 50 lines), one glue module (about 20), one build-script block, three small defect fixes. The NSIS
stack is deleted.

**What's working**
- The swap happens at a single seam (`DesktopPaths.sidecar`). The controller, service, #385 and the backend are untouched.
- Illegal states are designed out instead of checked at runtime. A folder named by content can't hold the wrong version
  (I1). Atomic rename means a file is complete or absent (I2). The single-instance ordering means there's one writer (I3).
  None of this needs code.
- The id is the single source of truth: build.rs derives it once; the runtime folder and the smoke check S-W4 both use it.
- The defect fixes cover the whole bug class: *every* `Command::new` goes through `command()`, and the `.exe`-less dev
  fallback is made unreachable on Windows rather than patched.

**Cut or simplified during review**
- ✂️ `embed-sidecar` cargo feature → `cfg(windows)` (it was a redundant axis).
- ✂️ pid-unique temp names + "lost the race" tolerance → unnecessary under I3.
- ✂️ "keep last 2" retention → delete all siblings; the OS protects in-use images.
- ✂️ runtime SHA verification / zstd checksum / size const → I1 + I2 make them dead weight.
- ✂️ new `StartError` variant → the existing SidecarMissing view (Q4 keeps the door open).
- ✂️ `Sys` seam for the extractor → real temp-dir tests.
- ✂️ Tauri CLI, NSIS, Node, appindicator, 7z, Ubuntu 24.04 base, the NSIS/appindicator "gotchas" docs, the
  best-effort SKIP branch in smoke.
- ✂️ `TAURI_CONFIG` override to drop externalBin → keep staging the file (it's the include source anyway).
- ✂️ xz as a second codec.

**Biggest astonishment risk:** on Windows, a `peakbot.exe` placed next to the desktop exe is silently **ignored**, and a
40 MB `peakbot.exe` appears under `%LOCALAPPDATA%`. Both are documented (README "first launch unpacks…", the
`DesktopPaths.sidecar` doc, the `build_paths` comment). The second biggest: Linux/macOS builds now compile zstd-sys as a
build dependency even though they never use it. Noted in Cargo.toml with a one-line comment.

**One thing to keep in mind:** the identity string `<version>-<sha12>` is the contract between build, runtime and smoke.
If anyone ever writes the id by hand, or derives the folder name some other way, I1 breaks silently.

**Candidate rules (for AGENTS.md / DESIGN)**
1. *On Windows, every child process is created through `sys::command()`*. A bare `Command::new` brings back console windows.
2. *Write-once, content-named files are never overwritten.* If the content changes, the name changes.
3. *The sidecar path is decided in one place (`build_paths`); nothing else looks for `peakbot.exe`.*
4. *Container smoke checks never SKIP*: a missing tool fails the build.
