fn main() {
    // An app manifest makes every command need an explicit `allow-*` grant:
    // only the bundled Profiles page gets them (capabilities/profiles-page.json).
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "profiles_get",
            "profile_add",
            "profile_rename",
            "profile_remove",
            "local_consent_set",
            "profile_open",
        ]),
    ))
    .expect("failed to run tauri-build");
}
