//! The optional status-area icon. Pigeon works fully without one: it is created
//! only when the `tray` setting is on, and any failure leaves the app running.
use std::panic::{AssertUnwindSafe, catch_unwind};

use tauri::AppHandle;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::desktop;

const SHOW: &str = "show";
const QUIT: &str = "quit";

/// Adds the icon. The Linux backend loads libayatana-appindicator at run time
/// and panics when it is missing, so a missing library must not take the app down.
pub fn install(app: &AppHandle) {
    match catch_unwind(AssertUnwindSafe(|| build(app))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("Pigeon: could not add the tray icon ({error})"),
        Err(_) => eprintln!("Pigeon: could not add the tray icon (no appindicator library)"),
    }
}

fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, SHOW, "Open Pigeon", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit Pigeon", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id("pigeon")
        .tooltip("Pigeon")
        .menu(&menu)
        // Left click opens the window; the menu is for the secondary click.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            SHOW => desktop::show_main(app),
            QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                desktop::show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}
