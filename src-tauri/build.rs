fn main() {
    // Declaring the app's commands makes each one subject to capabilities:
    // a webview that is not granted `allow-<command>` cannot call it.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "foundation_report",
            "open_viewer_fixture",
            "open_viewer_probe",
            "viewer_probe_reached",
            "quit_app",
        ]),
    ))
    .expect("failed to run tauri-build");
}
