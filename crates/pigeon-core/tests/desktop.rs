use pigeon_core::desktop::{
    self, DesktopSettings, NotificationPolicy, SettingsError, settings_path,
};

fn path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    settings_path(dir.path())
}

#[test]
fn missing_file_uses_defaults_that_keep_running_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = desktop::load(&path(&dir));
    assert_eq!(loaded.settings, DesktopSettings::default());
    assert!(loaded.settings.close_to_background);
    assert_eq!(
        loaded.settings.notifications,
        NotificationPolicy::SenderSubject
    );
    assert!(!loaded.settings.tray);
    assert!(!loaded.settings.sender_images);
    assert!(!loaded.managed);
    assert_eq!(loaded.warning, None);
}

#[test]
fn partial_file_fills_defaults_and_ignores_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let file = path(&dir);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        r#"{"closeToBackground": false, "notifications": "count", "future": 1}"#,
    )
    .unwrap();
    let loaded = desktop::load(&file);
    assert!(!loaded.settings.close_to_background);
    assert_eq!(loaded.settings.notifications, NotificationPolicy::Count);
    assert!(!loaded.settings.tray);
    // Files written before this setting existed must not start lookups.
    assert!(!loaded.settings.sender_images);
    assert_eq!(loaded.warning, None);
}

#[test]
fn invalid_file_warns_uses_defaults_and_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let file = path(&dir);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, r#"{"notifications": "everything"}"#).unwrap();
    let loaded = desktop::load(&file);
    assert_eq!(loaded.settings, DesktopSettings::default());
    assert!(loaded.warning.unwrap().contains("not valid"));
    assert_eq!(
        desktop::save(&file, &DesktopSettings::default()),
        Err(SettingsError::Invalid)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        r#"{"notifications": "everything"}"#
    );
}

#[test]
fn save_round_trips_and_creates_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let file = path(&dir);
    let wanted = DesktopSettings {
        close_to_background: false,
        notifications: NotificationPolicy::Off,
        tray: true,
        sender_images: true,
    };
    let saved = desktop::save(&file, &wanted).unwrap();
    assert_eq!(saved.settings, wanted);
    assert_eq!(desktop::load(&file).settings, wanted);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains(r#""closeToBackground": false"#), "{text}");
    assert!(text.contains(r#""senderImages": true"#), "{text}");
    assert!(!file.with_file_name(".settings.json.tmp").exists());
}

#[cfg(unix)]
#[test]
fn home_manager_symlink_is_managed_and_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store-settings.json");
    std::fs::write(&store, r#"{"closeToBackground": false}"#).unwrap();
    let file = path(&dir);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&store, &file).unwrap();

    let loaded = desktop::load(&file);
    assert!(loaded.managed);
    assert!(!loaded.settings.close_to_background);
    assert_eq!(
        desktop::save(&file, &DesktopSettings::default()),
        Err(SettingsError::Managed)
    );
    assert_eq!(
        std::fs::read_to_string(&store).unwrap(),
        r#"{"closeToBackground": false}"#
    );
}

#[cfg(unix)]
#[test]
fn read_only_file_is_managed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = path(&dir);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "{}").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(desktop::load(&file).managed);
}
