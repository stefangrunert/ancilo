fn main() {
    // The app's own commands, each allowed explicitly (capabilities/): the
    // page comes from the daemon – a remote origin to Tauri.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["remove_ancilo", "quit"])),
    )
    .expect("tauri build");
}
