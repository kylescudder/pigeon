//! Window lifecycle and the non-secret desktop settings file.
use std::path::PathBuf;
use std::sync::Mutex;

use pigeon_core::desktop::{self, DesktopSettings, LoadedSettings, NotificationPolicy};
use tauri::{AppHandle, Manager};

/// Starts without showing the window. Used by the login service.
pub const BACKGROUND: &str = "--background";

pub struct DesktopState {
    path: PathBuf,
    loaded: Mutex<LoadedSettings>,
}

impl DesktopState {
    pub fn load(app: &AppHandle) -> tauri::Result<Self> {
        let path = desktop::settings_path(&app.path().config_dir()?);
        let loaded = desktop::load(&path);
        if let Some(warning) = &loaded.warning {
            eprintln!("Pigeon: {warning}");
        }
        Ok(Self {
            path,
            loaded: Mutex::new(loaded),
        })
    }

    /// Whether to add a status-area icon. Read once at start: the icon is not
    /// added or removed while Pigeon runs.
    pub fn tray_enabled(&self) -> bool {
        desktop::load(&self.path).settings.tray
    }

    /// Read fresh each time, so a Home Manager switch applies without a restart.
    pub fn notification_policy(&self) -> NotificationPolicy {
        desktop::load(&self.path).settings.notifications
    }

    /// Reads the file at close time, so a Home Manager switch applies at once.
    pub fn close_to_background(&self) -> bool {
        let fresh = desktop::load(&self.path);
        let keep = fresh.settings.close_to_background;
        *self.loaded.lock().unwrap() = fresh;
        keep
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(crate::MAIN) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
pub fn desktop_settings(app: AppHandle, webview: tauri::Webview) -> Result<LoadedSettings, String> {
    crate::require_main(&webview)?;
    let state = app.state::<DesktopState>();
    // Re-read so a Home Manager switch shows without a restart.
    let fresh = desktop::load(&state.path);
    *state.loaded.lock().unwrap() = fresh.clone();
    Ok(fresh)
}

#[tauri::command]
pub fn set_desktop_settings(
    app: AppHandle,
    webview: tauri::Webview,
    settings: DesktopSettings,
) -> Result<LoadedSettings, String> {
    crate::require_main(&webview)?;
    let state = app.state::<DesktopState>();
    let saved = desktop::save(&state.path, &settings).map_err(|e| e.to_string())?;
    *state.loaded.lock().unwrap() = saved.clone();
    Ok(saved)
}
