fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new()
                .commands(&["status", "probe", "activate", "shutdown"]),
        ),
    )
    .expect("failed to build Melon Tauri manifest");
}
