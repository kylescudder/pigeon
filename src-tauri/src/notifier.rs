//! Shows new-mail notifications through the desktop's notification service.
//! What they say is decided in `pigeon_core::notify`; this only displays it.
use pigeon_core::notify::Notification;

/// A missing or failing notification service is not an error for a mail client.
pub fn show(note: &Notification) {
    let _ = notify_rust::Notification::new()
        .appname("Pigeon")
        .summary(&note.summary)
        .body(&note.body)
        .show();
}
