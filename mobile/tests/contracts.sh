#!/bin/sh
#
# contracts.sh — static contract tests for the Android build
# (in the style of tests/release.sh).
#
# Written BEFORE the implementation: expect RED until the developer lands
#   - mobile/scripts/build-android.sh: the four
#     CARGO_PROFILE_RELEASE_* size env vars, `npm run tauri --`, `--locked`
#   - mobile/scripts/smoke-android.sh: every smoke assertion
#     (aapt2 badging package/versionName/minSdk 24/targetSdk 37/arm64-v8a
#     only, apksigner verify --print-certs, zipalign -c -P 16 -v 4, manifest
#     networkSecurityConfig + allowBackup=false, NSC src="user" + the
#     127.0.0.1 domain-config, size < 40 MB)
# The ci.yml and lock contracts are GREEN already —
# they are guards that must stay green.
#
# The iOS locks live in
# mobile/tests/ios-contracts.sh and are invoked from here — this file stays
# the single CI entry point.
#
# Runs NO docker, NO cargo, NO network: pure file greps.
#
# Run:  sh mobile/tests/contracts.sh        (from the repo root, or anywhere)
# Exit: 0 when every check passes (GREEN); 1 with a failing-check list
#       otherwise (RED — each failure names the missing implementation piece).
#
# CI wiring: `sh mobile/tests/contracts.sh` runs in the `desktop` job of
# .gitea/workflows/ci.yml (it has no container deps; the `ci` job could
# run it too).

set -u

script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)

BA="$root/mobile/scripts/build-android.sh"
SA="$root/mobile/scripts/smoke-android.sh"
CI="$root/.gitea/workflows/ci.yml"
ML="$root/mobile/src-tauri/Cargo.lock"

PASS=0
FAIL=0
FAILED=""

ok()  { PASS=$((PASS+1)); echo "✅ $1"; }
bad() { FAIL=$((FAIL+1)); FAILED="${FAILED}  - $1\n"; echo "❌ $1"; }

# grep_ok <name> <fixed-string> <file>
grep_ok() {
    if grep -qF -- "$2" "$3" 2>/dev/null; then ok "$1"; else bad "$1 (missing: $2 in $3)"; fi
}

# grep_bad <name> <fixed-string> <file> — passes when the string is ABSENT.
grep_bad() {
    if grep -qF -- "$2" "$3" 2>/dev/null; then bad "$1 (forbidden: $2 in $3)"; else ok "$1"; fi
}

# grep_ok_live <name> <fixed-string> <file> — passes only when the string
# occurs on a NON-COMMENT line. The stub's header comments already name
# the symbols, so a plain grep would be GREEN without any implementation.
grep_ok_live() {
    if grep -v '^[[:space:]]*#' "$3" 2>/dev/null | grep -qF -- "$2"; then ok "$1"; else bad "$1 (missing live: $2 in $3)"; fi
}

# ── build-android.sh ─────────────────────────────────────────────────────

if [ ! -f "$BA" ]; then
    bad "mobile/scripts/build-android.sh exists"
else
    # Release size settings come from env: the workspace
    # profiles live at the root and must not change the CLI build.
    grep_ok "build-android sets CARGO_PROFILE_RELEASE_OPT_LEVEL=s" "CARGO_PROFILE_RELEASE_OPT_LEVEL=s" "$BA"
    grep_ok "build-android sets CARGO_PROFILE_RELEASE_LTO=true" "CARGO_PROFILE_RELEASE_LTO=true" "$BA"
    grep_ok "build-android sets CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1" "CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1" "$BA"
    grep_ok "build-android sets CARGO_PROFILE_RELEASE_STRIP=true" "CARGO_PROFILE_RELEASE_STRIP=true" "$BA"
    # Invocation through the package.json script:
    grep_ok "build-android uses 'npm run tauri --'" "npm run tauri --" "$BA"
    # --locked on every cargo/tauri command.
    grep_ok "build-android uses --locked" "--locked" "$BA"
fi

# ── smoke-android.sh ─────────────────────────────────────────────────────

if [ ! -f "$SA" ]; then
    bad "mobile/scripts/smoke-android.sh exists"
else
    # aapt2 dump badging: package sh.shifu.mobile, versionName, minSdk 24,
    # targetSdk 37, arm64-v8a only.
    grep_ok "smoke: aapt2 dump badging" "aapt2 dump badging" "$SA"
    grep_ok "smoke: package sh.shifu.mobile" "sh.shifu.mobile" "$SA"
    grep_ok "smoke: versionName" "versionName" "$SA"
    grep_ok "smoke: minSdk 24" "minSdkVersion='24'" "$SA"
    grep_ok "smoke: targetSdk 37" "targetSdkVersion='37'" "$SA"
    grep_ok "smoke: arm64-v8a" "arm64-v8a" "$SA"
    # Release signing: cert fingerprint checked against the pinned one.
    grep_ok "smoke: apksigner verify --print-certs" "apksigner verify --print-certs" "$SA"
    # 16 KB page alignment (S10 / R11).
    grep_ok "smoke: zipalign -c -P 16 -v 4" "zipalign -c -P 16 -v 4" "$SA"
    # Manifest: networkSecurityConfig reference + no cloud backup of the
    # token/config.
    grep_ok "smoke: manifest networkSecurityConfig" "networkSecurityConfig" "$SA"
    grep_ok "smoke: manifest allowBackup=false" 'allowBackup="false"' "$SA"
    # NSC: user-installed CA anchors + cleartext for 127.0.0.1 only.
    grep_ok "smoke: NSC src=\"user\"" 'src="user"' "$SA"
    grep_ok "smoke: NSC 127.0.0.1 domain-config" "127.0.0.1" "$SA"
    # Size cap (measured on a development build: 22.1 MB; the core link is
    # ~2× the remote-only build).
    grep_ok "smoke: size cap 40" "40" "$SA"
    # Host-only symbol leak check: the unstripped-.so symbol check must be
    # LIVE — an uncommented call, a body that names the four host-only
    # symbols, and a real `llvm-nm -D` scan. grep_ok_live ignores comment
    # lines, so the stub's header comments cannot satisfy these.
    grep_ok_live "smoke: run_l10_symbol_check is called (uncommented)" 'run_l10_symbol_check "$apk"' "$SA"
    for sym in portable_pty TokioChildProcess BashTool ratatui; do
        grep_ok_live "smoke: L10 symbol list names $sym (uncommented)" "$sym" "$SA"
    done
    grep_ok_live "smoke: L10 runs llvm-nm -D (uncommented)" "llvm-nm -D" "$SA"
fi

# ── ci.yml (guard) ───────────────────────────────────────────────────────

# The root job runs `cargo test --locked` (default-members = root).
grep_ok "ci: root job runs cargo test --locked" "cargo test --locked" "$CI"
# … and there is no root `--workspace` test run (that line would need
# webkit in the ci job). The desktop workspace keeps its own
# `--manifest-path desktop/Cargo.toml --workspace` runs, which do not
# contain this exact string.
grep_bad "ci: no root 'cargo test --workspace'" "cargo test --workspace" "$CI"

# ── lock policy (guard) ──────────────────────────────────────────────────

if [ -e "$ML" ]; then
    bad "no mobile/src-tauri/Cargo.lock (D8: one workspace, one lock)"
else
    ok "no mobile/src-tauri/Cargo.lock (D8)"
fi

# ── iOS (delegates to ios-contracts.sh) ──────────────────────────────────
# Separate file: its own ✅/❌ lines and counters; its exit status folds
# into this verdict so CI keeps exactly one entry point.
if [ -f "$script_dir/ios-contracts.sh" ]; then
    if sh "$script_dir/ios-contracts.sh"; then
        ok "ios-contracts: Phase 4 iOS static locks pass"
    else
        bad "ios-contracts: Phase 4 iOS static locks failing (see ❌ above)"
    fi
else
    bad "mobile/tests/ios-contracts.sh exists"
fi

# ── verdict ──────────────────────────────────────────────────────────────

echo
echo "contracts: $PASS passed, $FAIL failed"
if [ "$FAIL" -ne 0 ]; then
    printf 'failing checks:\n%s\n' "$FAILED"
    exit 1
fi
exit 0
