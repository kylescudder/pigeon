//! Main-window adapters for the mail engine. No SQL or protocol logic here.
use pigeon_core::mail::{
    Account, AccountConfig, Draft, DraftInput, Folder, MailEngine, MailError, Message, OutboxItem,
    SavedSearch, SearchQuery, SearchResults, SendDecision, SendState, SendingSettings,
};
use pigeon_core::secrets::Secret;
use pigeon_core::viewer::{self, RemoteContent};
use serde::Serialize;
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
            MailError::Locked => "locked",
            MailError::Storage(_) => "storage",
            MailError::NotFound => "not_found",
            MailError::Busy => "busy",
            MailError::Cancelled => "cancelled",
            MailError::InvalidSearch(_) => "invalid_search",
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
    task(app, |engine| {
        Ok(MailState {
            accounts: engine.accounts()?,
            folders: engine.folders()?,
            saved_searches: engine.saved_searches()?,
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

#[tauri::command]
pub async fn open_mail_message(
    app: AppHandle,
    webview: tauri::Webview,
    id: i64,
) -> Result<(), MailApiError> {
    guard(&webview)?;
    let document = task(app.clone(), move |engine| {
        let message = engine.message(id)?;
        let html = message.body_html.unwrap_or_else(|| {
            format!(
                "<pre>{}</pre>",
                message
                    .body_text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            )
        });
        Ok(viewer::document(
            &viewer::sanitise(&html, RemoteContent::Blocked),
            RemoteContent::Blocked,
        ))
    })
    .await?;
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
    })?
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
