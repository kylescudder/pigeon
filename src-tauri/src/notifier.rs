//! Shows new-mail notifications through the desktop's notification service and
//! coos. What they say is decided in `pigeon_core::notify`; this only displays
//! it.
use std::io::Cursor;
use std::thread;

use pigeon_core::notify::Notification;
use rodio::DeviceSinkBuilder;

const COO: &[u8] = include_bytes!("../sounds/coo.wav");

/// A missing or failing notification service is not an error for a mail client.
pub fn show(note: &Notification) {
    let _ = notify_rust::Notification::new()
        .appname("Pigeon")
        .summary(&note.summary)
        .body(&note.body)
        .show();
    // On its own thread so a slow sound device cannot hold up the refresh.
    thread::spawn(coo);
}

/// Neither is a missing or failing sound device.
fn coo() {
    let Ok(mut sink) = DeviceSinkBuilder::open_default_sink() else {
        return;
    };
    sink.log_on_drop(false);
    if let Ok(player) = rodio::play(sink.mixer(), Cursor::new(COO)) {
        player.sleep_until_end();
    }
}
