fn main() {
    // Declaring the app's commands generates an `allow-<command>` permission
    // for each, so every command must be granted explicitly in a capability
    // (spec section 9).
    let manifest = tauri_build::AppManifest::new().commands(&["set_hit_regions"]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to run tauri-build");
}
