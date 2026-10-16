fn main() {
    // The compile target triple, exposed to the glue so the dev build can
    // find the sidecar in `binaries/peakbot-<triple>` (tauri keeps the
    // triple suffix in dev; the bundler strips it).
    println!(
        "cargo:rustc-env=DESKTOP_TARGET_TRIPLE={}",
        std::env::var("TARGET").expect("cargo sets TARGET for build scripts")
    );

    // Command ACL: `status` and `act` get generated `allow-`/`deny-`
    // permissions; the status-page capability grants exactly those two.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["status", "act"])),
    )
    .expect("tauri-build failed");
}
