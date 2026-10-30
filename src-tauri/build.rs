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
            "mail_state",
            "add_mail_account",
            "sync_mail_account",
            "cancel_mail_sync",
            "search_mail",
            "cancel_mail_search",
            "read_mail_message",
            "save_mail_search",
            "delete_mail_search",
            "remove_mail_account",
            "update_mail_password",
            "unlock_mail",
            "open_mail_message",
            "send_state",
            "save_draft",
            "delete_draft",
            "send_draft",
            "return_to_drafts",
            "retry_send",
            "retry_sent_copy",
            "resolve_send",
            "dismiss_outbox",
            "update_mail_sending",
            "set_undo_send_seconds",
        ]),
    ))
    .expect("failed to run tauri-build");
}
