fn main() {
    // The compile target triple, exposed to the glue so the dev build can
    // find the sidecar in `binaries/peakbot-<triple>` (tauri keeps the
    // triple suffix in dev; the bundler strips it).
    println!(
        "cargo:rustc-env=DESKTOP_TARGET_TRIPLE={}",
        std::env::var("TARGET").expect("cargo sets TARGET for build scripts")
    );

    // Command ACL: `status`, `act`, `settings_get` and `settings_set` get
    // generated `allow-`/`deny-` permissions; the capabilities grant each
    // local window exactly its own set.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "status",
            "act",
            "settings_get",
            "settings_set",
        ]),
    ))
    .expect("tauri-build failed");
}
