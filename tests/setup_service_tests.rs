//! B3 — the setup wizard's service install must point the service at the
//! install target, not at a path that does not exist (desktop DESIGN §7
//! B3).
//!
//! **The seam.** The exe-selection logic lives inside
//! `peakbot::ui::web::setup::service_op_adapter` — the production body
//! behind the `ServiceFn` fn-pointer seam. The HTTP seam sits *above*
//! that logic (the fakes in `tests/setup_api_tests.rs` replace the whole
//! adapter), so only a direct call to `service_op_adapter` exercises it.
//! That is the smallest seam; no refactoring needed.
//!
//! **RED today:** with a fresh `$HOME`, `install_target()` does not
//! exist, and the adapter takes the fallback (pointing the service at a
//! non-existent target) without installing the binary. The test asserts
//! the target exists after the call — the fixed behaviour.
//!
//! **Safety:** the call reaches the real `service_op`. On a machine with
//! a live systemd user bus the fixed code would install+start a real
//! service (pointing at the tempdir copy) — the test skips there. CI
//! containers have no user bus, so the test runs where it matters and
//! the adapter fails at the preflight (no `/run/systemd/user`), leaving
//! exactly one observable: whether the binary was installed first.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::Path;

/// Serialize the env-mutating test in this binary.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Save/restore one env var (edition 2024: `set_var` is `unsafe`).
struct EnvVar {
    key: &'static str,
    old: Option<OsString>,
}

impl EnvVar {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let old = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, old }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

#[test]
fn service_install_installs_the_binary_to_the_target_first_when_it_is_missing() {
    let _serial = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Isolate $HOME and $XDG_CONFIG_HOME so `install_target()`
    // (`$HOME/.local/bin/peakbot`) and the config dir resolve into the
    // tempdir — no writes to the real user profile.
    let _home = EnvVar::set("HOME", dir.path());
    let _xdg = EnvVar::set("XDG_CONFIG_HOME", &dir.path().join(".config"));

    // Skip on machines with a live systemd user bus (see module docs).
    if Path::new("/run/systemd/user").exists() {
        eprintln!("SKIP: a live systemd user bus is present (CI has none)");
        return;
    }

    let target =
        peakbot::install::install_target().expect("a unix $HOME resolves an install target");
    assert!(
        !target.exists(),
        "precondition: fresh $HOME ⇒ no install target"
    );

    let _ = peakbot::ui::web::setup::service_op_adapter(serde_json::json!({
        "op": "install",
        "bind": "127.0.0.1:7823",
    }));

    // B3: when `install_target()` doesn't exist, the adapter must run
    // `install_binary()` first and point the service at the target —
    // never at a non-existent path.
    assert!(
        target.exists(),
        "B3: the service install must install the binary to {} first; the target is missing after the call",
        target.display()
    );
    assert!(target.is_file(), "the target must be a file");
    assert!(
        std::fs::metadata(&target)
            .map(|m| m.len() > 0)
            .unwrap_or(false),
        "the installed target must be a real (non-empty) binary"
    );
}
