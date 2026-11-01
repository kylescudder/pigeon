//! Main-window adapters for the mail engine. No SQL or protocol logic here.
use pigeon_core::backup::{self, BackupError, Summary};
use pigeon_core::mail::managed;
use pigeon_core::mail::reply::ReplyKind;
use pigeon_core::mail::{
    Account, AccountConfig, ActionCounts, ActionKind, Draft, DraftInput, Folder, MailEngine,
    MailError, Message, OutboxItem, PendingAction, SavedSearch, SearchQuery, SearchResults,
    SendDecision, SendState, SendingSettings,
};
use pigeon_core::secrets::Secret;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[derive(Serialize)]
pub struct MailApiError {
    code: &'static str,
    message: String,
}

impl From<MailError> for MailApiError {
    fn from(error: MailError) -> Self {
        let code = match &error {
            MailError::InvalidConfig(_) => "invalid_config",
            MailError::Authentication => "authentication",
            MailError::Tls => "tls",
            MailError::Connection => "connection",
            MailError::Protocol => "protocol",
            MailError::IncompleteSync(_) => "incomplete_sync",
            MailError::ParseRetryLimit => "parse_retry_limit",
            MailError::Imap { .. } => "imap",
            MailError::ResponseParse { .. } => "imap_parse",
            MailError::PasswordNeeded => "password_needed",
            MailError::Locked => "locked",
            MailError::Storage(_) => "storage",
            MailError::NotFound => "not_found",
            MailError::Busy => "busy",
            MailError::Restoring => "restoring",
            MailError::Cancelled => "cancelled",
            MailError::InvalidSearch(_) => "invalid_search",
            MailError::KeyMissing => "key_missing",
            MailError::Backup(error) => match error {
                BackupError::WeakPassphrase => "weak_passphrase",
                BackupError::WrongPassphrase => "wrong_passphrase",
                BackupError::Damaged => "damaged_backup",
                BackupError::Newer => "newer_backup",
                BackupError::Busy => "busy",
                BackupError::Io(_) => "backup_file",
                BackupError::Storage(_) => "storage",
            },
            MailError::Unsupported(_) => "unsupported",
            MailError::NoTrashFolder => "no_trash_folder",
            MailError::NoArchiveFolder => "no_archive_folder",
            MailError::InvalidAction(_) => "invalid_action",
            MailError::Conflict(_) => "conflict",
            MailError::Smtp { .. } => "smtp",
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

#[derive(Serialize)]
pub struct MailState {
    accounts: Vec<Account>,
    folders: Vec<Folder>,
    saved_searches: Vec<SavedSearch>,
    actions: ActionCounts,
    /// Problems with the declared accounts file, in words for the user.
    managed_warnings: Vec<String>,
}

fn guard(webview: &tauri::Webview) -> Result<(), MailApiError> {
    crate::require_main(webview).map_err(|_| MailApiError {
        code: "denied",
        message: "command not available to this view".into(),
    })
}

async fn task<T: Send + 'static>(
    app: AppHandle,
    work: impl FnOnce(&MailEngine) -> Result<T, MailError> + Send + 'static,
) -> Result<T, MailApiError> {
    let engine = app.state::<Arc<MailEngine>>().inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || work(&engine))
        .await
        .map_err(|_| MailApiError {
            code: "interrupted",
            message: "The mail operation was interrupted. Try again.".into(),
        })?;
    if matches!(result, Err(MailError::Locked)) {
        // Do not leave decrypted message windows visible after store access is lost.
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            for (label, window) in handle.webview_windows() {
                if label.starts_with(crate::VIEWER_PREFIX) {
                    let _ = window.close();
                }
            }
            handle
                .state::<crate::ShellState>()
                .viewer_documents
                .lock()
                .unwrap()
                .clear();
        });
    }
    result.map_err(Into::into)
}

#[tauri::command]
pub async fn mail_state(
    app: AppHandle,
    webview: tauri::Webview,
) -> Result<MailState, MailApiError> {
    guard(&webview)?;
    let declared = app
        .path()
        .config_dir()
        .map(|dir| managed::accounts_path(&dir))
        .ok();
    task(app, move |engine| {
        let mut managed_warnings = Vec::new();
        if let Some(path) = declared {
            // Re-read on every refresh so a Home Manager switch shows without a restart.
            let declaration = managed::load(&path);
            managed_warnings = declaration.warnings.clone();
            match engine.reconcile_managed(&declaration) {
                // Another operation holds the store; the next refresh retries.
                Ok(()) | Err(MailError::Busy | MailError::Restoring) => {}
                Err(error @ (MailError::Locked | MailError::KeyMissing)) => return Err(error),
                Err(error) => managed_warnings.push(error.to_string()),
            }
        }
        Ok(MailState {
            accounts: engine.accounts()?,
            folders: engine.folders()?,
            saved_searches: engine.saved_searches()?,
            actions: engine.action_counts()?,
            managed_warnings,
        })
    })
    .await
}

#[tauri::command]
pub async fn add_mail_account(
    app: AppHandle,
    webview: tauri::Webview,
    config: AccountConfig,
    password: String,
) -> Result<Account, MailApiError> {
    guard(&webview)?;
    let password = Secret::new(password.into_bytes());
    task(app, move |engine| engine.add_account(config, password)).await
}

#[tauri::command]
pub async fn sync_mail_account(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.sync_account(&id)).await
}

#[tauri::command]
pub async fn cancel_mail_sync(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| {
        engine.cancel_sync(&id);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn search_mail(
    app: AppHandle,
    webview: tauri::Webview,
    query: SearchQuery,
    request_id: String,
) -> Result<SearchResults, MailApiError> {
    guard(&webview)?;
    task(app, move |engine| {
        engine.search_request(&request_id, &query)
    })
    .await
}

#[tauri::command]
pub fn cancel_mail_search(
    app: AppHandle,
    webview: tauri::Webview,
    request_id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    app.state::<Arc<MailEngine>>().cancel_search(&request_id);
    Ok(())
}

#[tauri::command]
pub async fn read_mail_message(
    app: AppHandle,
    webview: tauri::Webview,
    id: i64,
) -> Result<Message, MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.message(id)).await
}

/// Checks the setting here as well as in the interface: only the user's choice
/// may send a sender's domain off the device.
#[tauri::command]
pub async fn sender_image(
    app: AppHandle,
    webview: tauri::Webview,
    domain: String,
) -> Result<Option<String>, MailApiError> {
    guard(&webview)?;
    if !app.state::<crate::desktop::DesktopState>().sender_images() {
        return Ok(None);
    }
    task(app, move |engine| engine.sender_image(&domain)).await
}

#[tauri::command]
pub async fn save_mail_search(
    app: AppHandle,
    webview: tauri::Webview,
    name: String,
    query: SearchQuery,
) -> Result<SavedSearch, MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.save_search(&name, &query)).await
}

#[tauri::command]
pub async fn delete_mail_search(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.delete_saved_search(&id)).await
}

#[tauri::command]
pub async fn remove_mail_account(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.remove_account(&id)).await
}

#[tauri::command]
pub async fn update_mail_password(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
    password: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    let password = Secret::new(password.into_bytes());
    task(app, move |engine| engine.update_password(&id, password)).await
}

#[tauri::command]
pub async fn unlock_mail(app: AppHandle, webview: tauri::Webview) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, MailEngine::unlock).await
}

#[derive(Serialize)]
pub struct OpenedMessage {
    /// Embedded images that could not be downloaded, such as while offline.
    missing_inline: u32,
}

#[tauri::command]
pub async fn open_mail_message(
    app: AppHandle,
    webview: tauri::Webview,
    id: i64,
) -> Result<OpenedMessage, MailApiError> {
    guard(&webview)?;
    // Embedded images are fetched over the mail connection and inlined; the
    // viewer document itself can load nothing from the network.
    let document = task(app.clone(), move |engine| engine.viewer_document(id)).await?;
    let opened = OpenedMessage {
        missing_inline: document.missing_inline,
    };
    let document = document.html;
    let token = app
        .state::<crate::ShellState>()
        .viewer_count
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let page = format!("message-{token}");
    app.state::<crate::ShellState>()
        .viewer_documents
        .lock()
        .unwrap()
        .insert(page.clone(), document);
    tauri::async_runtime::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = app.clone();
        app.run_on_main_thread(move || {
            let _ = tx.send(crate::open_viewer(&handle, &page, "Message · Pigeon"));
        })
        .map_err(|error| MailApiError {
            code: "viewer",
            message: error.to_string(),
        })?;
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| MailApiError {
                code: "viewer",
                message: "The message viewer did not open. Try again.".into(),
            })?
            .map_err(|message| MailApiError {
                code: "viewer",
                message,
            })
    })
    .await
    .map_err(|_| MailApiError {
        code: "viewer",
        message: "The message viewer was interrupted.".into(),
    })??;
    Ok(opened)
}

/// Queues a change and starts applying it in the background. Returns how many
/// messages it affects; completion shows in the pending-change list.
#[tauri::command]
pub async fn queue_mail_actions(
    app: AppHandle,
    webview: tauri::Webview,
    ids: Vec<i64>,
    kind: ActionKind,
) -> Result<usize, MailApiError> {
    guard(&webview)?;
    let queued = task(app.clone(), move |engine| engine.queue_actions(&ids, kind)).await?;
    apply_in_background(&app);
    Ok(queued)
}

/// Applies every account's queue. An account that is syncing is skipped: its
/// sync applies the queue itself when it finishes.
fn apply_in_background(app: &AppHandle) {
    let engine = app.state::<Arc<MailEngine>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Ok(accounts) = engine.accounts() {
            for account in accounts {
                let _ = engine.apply_actions(&account.id);
            }
        }
    });
}

#[tauri::command]
pub async fn mail_actions(
    app: AppHandle,
    webview: tauri::Webview,
) -> Result<Vec<PendingAction>, MailApiError> {
    guard(&webview)?;
    task(app, MailEngine::pending_actions).await
}

#[tauri::command]
pub async fn retry_mail_action(
    app: AppHandle,
    webview: tauri::Webview,
    id: i64,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app.clone(), move |engine| engine.retry_action(id)).await?;
    apply_in_background(&app);
    Ok(())
}

#[tauri::command]
pub async fn discard_mail_action(
    app: AppHandle,
    webview: tauri::Webview,
    id: i64,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.discard_action(id)).await
}

// Composing and sending. Message content stays in the encrypted store; the
// outbox reports outcomes, never server reply text.

#[tauri::command]
pub async fn send_state(
    app: AppHandle,
    webview: tauri::Webview,
) -> Result<SendState, MailApiError> {
    guard(&webview)?;
    task(app, |engine| engine.send_state()).await
}

/// Saves a draft that replies to, or forwards, a stored message.
#[tauri::command]
pub async fn reply_to_message(
    app: AppHandle,
    webview: tauri::Webview,
    id: i64,
    kind: ReplyKind,
) -> Result<Draft, MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.start_reply(id, kind)).await
}

#[tauri::command]
pub async fn save_draft(
    app: AppHandle,
    webview: tauri::Webview,
    draft: DraftInput,
) -> Result<Draft, MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.save_draft(draft)).await
}

#[tauri::command]
pub async fn delete_draft(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.delete_draft(&id)).await
}

#[tauri::command]
pub async fn send_draft(
    app: AppHandle,
    webview: tauri::Webview,
    draft: DraftInput,
) -> Result<OutboxItem, MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.send_draft(draft)).await
}

#[tauri::command]
pub async fn return_to_drafts(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<Draft, MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.return_to_drafts(&id)).await
}

#[tauri::command]
pub async fn retry_send(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.retry_send(&id)).await
}

#[tauri::command]
pub async fn retry_sent_copy(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.retry_sent_copy(&id)).await
}

#[tauri::command]
pub async fn resolve_send(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
    decision: SendDecision,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.resolve_send(&id, decision)).await
}

#[tauri::command]
pub async fn dismiss_outbox(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.dismiss_outbox(&id)).await
}

#[tauri::command]
pub async fn update_mail_sending(
    app: AppHandle,
    webview: tauri::Webview,
    id: String,
    settings: SendingSettings,
) -> Result<Account, MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.update_sending(&id, settings)).await
}

#[tauri::command]
pub async fn set_undo_send_seconds(
    app: AppHandle,
    webview: tauri::Webview,
    seconds: u32,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    task(app, move |engine| engine.set_undo_send_seconds(seconds)).await
}

/// Asks where to save, then writes an encrypted backup. The file chooser runs
/// natively, so the webview never names a path. `None` means cancelled.
#[tauri::command]
pub async fn backup_mail(
    app: AppHandle,
    webview: tauri::Webview,
    passphrase: String,
) -> Result<Option<Summary>, MailApiError> {
    guard(&webview)?;
    let passphrase = zeroize::Zeroizing::new(passphrase);
    backup::check_passphrase(&passphrase).map_err(MailError::from)?;
    let Some(path) = choose_file(&app, true).await? else {
        return Ok(None);
    };
    task(app, move |engine| engine.create_backup(&path, &passphrase))
        .await
        .map(Some)
}

/// Asks for a backup file, then replaces local-only data with it.
#[tauri::command]
pub async fn restore_mail(
    app: AppHandle,
    webview: tauri::Webview,
    passphrase: String,
) -> Result<Option<Summary>, MailApiError> {
    guard(&webview)?;
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let Some(path) = choose_file(&app, false).await? else {
        return Ok(None);
    };
    task(app, move |engine| engine.restore_backup(&path, &passphrase))
        .await
        .map(Some)
}

async fn choose_file(app: &AppHandle, save: bool) -> Result<Option<PathBuf>, MailApiError> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app
        .dialog()
        .file()
        .add_filter("Pigeon backup", &[backup::FILE_EXTENSION]);
    if let Some(window) = app.get_webview_window(crate::MAIN) {
        dialog = dialog.set_parent(&window);
    }
    let picked = tauri::async_runtime::spawn_blocking(move || {
        if save {
            dialog
                .set_title("Save Pigeon backup")
                .set_file_name(backup::default_file_name())
                .blocking_save_file()
        } else {
            dialog
                .set_title("Restore Pigeon backup")
                .blocking_pick_file()
        }
    })
    .await
    .map_err(|_| MailApiError {
        code: "interrupted",
        message: "The file chooser was interrupted. Try again.".into(),
    })?;
    picked
        .map(|file| {
            file.into_path().map_err(|_| MailApiError {
                code: "backup_file",
                message: "Choose a file on this computer.".into(),
            })
        })
        .transpose()
}
