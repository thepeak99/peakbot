/// First 12 hex chars of the SHA-256 of `bytes` — the `<sha12>` half of
/// the sidecar identity.
fn sha12(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn main() {
    // The compile target triple, exposed to the glue so the dev build can
    // find the sidecar in `binaries/peakbot-<triple>` (tauri keeps the
    // triple suffix in dev; the bundler strips it).
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=DESKTOP_TARGET_TRIPLE={target}");

    // Windows only: pack the staged sidecar into the exe as a zstd blob
    // (level 19 — the best ratio the pure-Rust decoder supports) and
    // derive its identity `<version>-<sha12>` (DESIGN amendment
    // (windows-portable): I1 identity = content). The id is the single
    // source of truth shared with the runtime extraction folder and the
    // S-W4 smoke check.
    if target.contains("windows") {
        let src = format!("binaries/peakbot-{target}.exe");
        println!("cargo:rerun-if-changed={src}");
        let bytes = std::fs::read(&src).unwrap_or_else(|e| {
            panic!("staged sidecar missing: {src} (build-desktop.sh stages it; DESIGN §5.7): {e}")
        });
        let id = format!(
            "{}-{}",
            std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION"),
            sha12(&bytes)
        );
        let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"))
            .join("peakbot.exe.zst");
        std::fs::write(
            &out,
            zstd::encode_all(&bytes[..], 19).expect("zstd level 19"),
        )
        .expect("write the payload blob");
        println!("cargo:rustc-env=PEAKBOT_SIDECAR_ID={id}");
    }

    // Command ACL: every command gets generated `allow-`/`deny-`
    // permissions. `local-pages` grants the bundled pages their set;
    // `remote-theme` grants the remote web UI (loopback :7823 only)
    // exactly `theme_set`.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&["status", "act", "theme_set", "theme_get"]),
    ))
    .expect("tauri-build failed");
}
