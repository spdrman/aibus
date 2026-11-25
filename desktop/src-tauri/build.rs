fn main() {
    // Every webview-callable command is listed here, so each one gets a permission the capability
    // file has to grant by name. A command added to `lib.rs` and not listed here is not callable.
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&["replay_fixture", "stack_info"]),
        ),
    )
    .expect("tauri build");
}
