//! Desktop shell: a thin adapter between the interface and `pigeon-core`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use pigeon_core::diagnostics::Check;
use pigeon_core::mail::MailEngine;
use pigeon_core::secrets::{KeyringState, SecretStore};
use pigeon_core::unlock::{UnlockOutcome, open_store};
use pigeon_core::viewer::{self, InlineImage, InlineImages, RemoteContent};
use serde::Serialize;
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_opener::OpenerExt;

mod desktop;
mod mail;

pub(crate) const MAIN: &str = "main";
const DIAGNOSE_SYNC: &str = "--diagnose-sync";
const VIEWER_SCHEME: &str = "pigeon-viewer";
const VIEWER_PREFIX: &str = "viewer-";
const HOSTILE_FIXTURE: &str = include_str!("../../crates/pigeon-core/tests/fixtures/hostile.html");

#[derive(Default)]
struct ShellState {
    viewer_count: AtomicU32,
    probe_opened: AtomicBool,
    /// Set if a viewer ever reaches a native command. Must stay false.
    probe_breached: AtomicBool,
    /// What the probe page saw, delivered by navigating to `/probe-done`
    /// rather than over IPC (which the viewer cannot use).
    probe_results: Mutex<Option<Vec<ProbeResult>>>,
    /// What the embedded-image self-test observed, reported the same way.
    inline_results: Mutex<Option<Vec<ProbeResult>>>,
    viewer_documents: Mutex<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct ProbeResult {
    name: String,
    ok: bool,
    detail: String,
}

#[derive(Serialize)]
struct FoundationReport {
    checks: Vec<Check>,
}

/// Defence in depth for Tauri's documented Linux limitation: capability checks
/// cannot tell frames apart, so every command also checks the calling webview.
pub(crate) fn require_main(webview: &tauri::Webview) -> Result<(), String> {
    if webview.label() == MAIN {
        Ok(())
    } else {
        Err("command not available to this view".into())
    }
}

fn secret_store() -> Box<dyn SecretStore> {
    #[cfg(target_os = "linux")]
    {
        Box::new(pigeon_core::secrets::SecretServiceStore)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(pigeon_core::secrets::MemoryStore::new(
            KeyringState::Unavailable("Secret Service is only supported on Linux".into()),
        ))
    }
}

#[tauri::command]
async fn foundation_report(
    app: AppHandle,
    webview: tauri::Webview,
) -> Result<FoundationReport, String> {
    require_main(&webview)?;
    Ok(FoundationReport {
        checks: collect_checks(&app).await?,
    })
}

async fn collect_checks(app: &AppHandle) -> Result<Vec<Check>, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let db_path = data_dir.join("mail.db");

    let mut checks = tauri::async_runtime::spawn_blocking(move || store_checks(&db_path))
        .await
        .map_err(|e| e.to_string())?;
    checks.extend(viewer_checks(&app.state::<ShellState>()));
    Ok(checks)
}

fn store_checks(db_path: &std::path::Path) -> Vec<Check> {
    let secrets = secret_store();
    let mut checks = Vec::new();
    let state = secrets.state();
    checks.push(Check::new(
        "Keyring",
        state == KeyringState::Unlocked,
        match &state {
            KeyringState::Unlocked => "unlocked".to_owned(),
            KeyringState::Locked => "locked: unlock the desktop keyring to continue".to_owned(),
            KeyringState::Unavailable(why) => format!("unavailable: {why}"),
        },
    ));
    match open_store(secrets.as_ref(), db_path) {
        UnlockOutcome::Opened { store, created } => {
            checks.push(Check::new(
                "Encrypted store opened",
                true,
                if created {
                    "created with a new keyring-held key"
                } else {
                    "existing store"
                },
            ));
            match store.diagnostics() {
                Ok(more) => checks.extend(more),
                Err(e) => checks.push(Check::new("Store diagnostics", false, e.to_string())),
            }
        }
        UnlockOutcome::Blocked(reason) => {
            checks.push(Check::new(
                "Encrypted store opened",
                false,
                format!("{reason:?}"),
            ));
        }
    }
    checks
}

fn viewer_checks(state: &ShellState) -> Vec<Check> {
    let breached = state.probe_breached.load(Ordering::SeqCst);
    let results = state.probe_results.lock().unwrap().clone();
    let mut checks = vec![Check::new(
        "Viewer native-command denial",
        !breached && results.is_some(),
        match (
            state.probe_opened.load(Ordering::SeqCst),
            breached,
            &results,
        ) {
            (_, true, _) => "BREACH: a viewer reached a native command",
            (_, false, Some(_)) => "probe completed; no native command reached from the viewer",
            (true, false, None) => "probe opened; waiting for it to finish",
            (false, false, None) => "probe not run yet",
        },
    )];
    for result in results.into_iter().flatten() {
        checks.push(Check::new(
            format!("Probe: {}", result.name),
            result.ok,
            result.detail,
        ));
    }
    // Only the self-test opens this view, so it is reported only once run.
    if let Some(results) = state.inline_results.lock().unwrap().clone() {
        checks.push(Check::new(
            "Viewer: embedded image without network",
            !results.is_empty() && results.iter().all(|result| result.ok),
            "synthetic cid: image rendered from inline data; nothing fetched",
        ));
        for result in results {
            checks.push(Check::new(
                format!("Embedded image: {}", result.name),
                result.ok,
                result.detail,
            ));
        }
    }
    checks
}

#[tauri::command]
fn open_viewer_fixture(app: AppHandle, webview: tauri::Webview) -> Result<(), String> {
    require_main(&webview)?;
    open_viewer(&app, "fixture", "Hostile fixture (sanitised)").map(drop)
}

/// Opens a page that assumes the attacker already has script execution in the
/// viewer, then attempts native IPC. The capability layer must reject it.
#[tauri::command]
fn open_viewer_probe(app: AppHandle, webview: tauri::Webview) -> Result<(), String> {
    require_main(&webview)?;
    app.state::<ShellState>()
        .probe_opened
        .store(true, Ordering::SeqCst);
    open_viewer(&app, "probe", "Viewer isolation probe").map(drop)
}

/// Granted to no capability. If this body ever runs, isolation failed.
#[tauri::command]
fn viewer_probe_reached(app: AppHandle) {
    app.state::<ShellState>()
        .probe_breached
        .store(true, Ordering::SeqCst);
}

#[tauri::command]
fn quit_app(app: AppHandle, webview: tauri::Webview) -> Result<(), String> {
    require_main(&webview)?;
    app.exit(0);
    Ok(())
}

/// Opens a viewer webview for `page` and returns its label.
fn open_viewer(app: &AppHandle, page: &str, title: &str) -> Result<String, String> {
    let n = app
        .state::<ShellState>()
        .viewer_count
        .fetch_add(1, Ordering::SeqCst);
    let url =
        Url::parse(&format!("{VIEWER_SCHEME}://localhost/{page}")).map_err(|e| e.to_string())?;
    let handle = app.clone();
    let label = format!("{VIEWER_PREFIX}{n}");
    let window = WebviewWindowBuilder::new(app, label.clone(), WebviewUrl::CustomProtocol(url))
        .title(title)
        .inner_size(800.0, 700.0)
        .on_navigation(move |url| {
            if url.scheme() == VIEWER_SCHEME {
                return true;
            }
            // Links leave the app for the user's normal browser.
            if matches!(url.scheme(), "https" | "http" | "mailto") {
                let _ = handle.opener().open_url(url.as_str(), None::<&str>);
            }
            false
        })
        .build()
        .map_err(|e| e.to_string())?;
    let handle = app.clone();
    let page = page.to_owned();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            handle
                .state::<ShellState>()
                .viewer_documents
                .lock()
                .unwrap()
                .remove(&page);
        }
    });
    Ok(label)
}

fn viewer_response(app: &AppHandle, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let (body, csp) = match request.uri().path() {
        "/probe-done" => {
            if let Some(results) = probe_query(request) {
                *app.state::<ShellState>().probe_results.lock().unwrap() = Some(results);
            }
            (
                "<!doctype html><p>Probe complete. Re-run the foundation checks.</p>".to_owned(),
                viewer::content_security_policy(RemoteContent::Blocked),
            )
        }
        "/fixture" => (
            viewer::document(
                &viewer::sanitise(HOSTILE_FIXTURE, RemoteContent::Blocked),
                RemoteContent::Blocked,
            ),
            viewer::content_security_policy(RemoteContent::Blocked),
        ),
        "/probe" => (PROBE_PAGE.to_owned(), PROBE_CSP.to_owned()),
        // Built exactly as a message's formatted view is: same sanitiser,
        // document wrapper and CSP, with the cid: part already downloaded.
        "/inline-fixture" => {
            let images = InlineImages::from([(
                "logo@pigeon.test".to_owned(),
                InlineImage {
                    content_type: "image/png".into(),
                    data: SELFTEST_PNG.to_vec(),
                },
            )]);
            (
                viewer::document(
                    &viewer::sanitise_inline(INLINE_FIXTURE, RemoteContent::Blocked, &images),
                    RemoteContent::Blocked,
                ),
                viewer::content_security_policy(RemoteContent::Blocked),
            )
        }
        "/inline-done" => {
            if let Some(results) = probe_query(request) {
                *app.state::<ShellState>().inline_results.lock().unwrap() = Some(results);
            }
            (
                "<!doctype html><p>Embedded image check complete.</p>".to_owned(),
                viewer::content_security_policy(RemoteContent::Blocked),
            )
        }
        _ => {
            if let Some(document) = app
                .state::<ShellState>()
                .viewer_documents
                .lock()
                .unwrap()
                .get(request.uri().path().trim_start_matches('/'))
                .cloned()
            {
                return Response::builder()
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .header(
                        header::CONTENT_SECURITY_POLICY,
                        viewer::content_security_policy(RemoteContent::Blocked),
                    )
                    .header("Referrer-Policy", "no-referrer")
                    .header("X-Content-Type-Options", "nosniff")
                    .body(Cow::Owned(document.into_bytes()))
                    .unwrap();
            }
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Cow::Borrowed(&b""[..]))
                .unwrap();
        }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, csp)
        .header("Referrer-Policy", "no-referrer")
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Owned(body.into_bytes()))
        .unwrap()
}

fn probe_query(request: &Request<Vec<u8>>) -> Option<Vec<ProbeResult>> {
    Url::parse(&request.uri().to_string())
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "r")
                .map(|(_, v)| v.into_owned())
        })
        .and_then(|json| serde_json::from_str::<Vec<ProbeResult>>(&json).ok())
}

/// A real 1x1 PNG standing in for a downloaded `cid:` part.
const SELFTEST_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

const INLINE_FIXTURE: &str = r#"<p>Embedded image self-test</p>
<img src="cid:logo@pigeon.test" alt="embedded logo" width="16" height="16">
<img src="https://example.org/selftest-pixel.gif" alt="remote pixel">
<p style="background:url(https://example.org/selftest-bg.png)">Styled paragraph</p>
<img src="cid:absent@pigeon.test" alt="absent part">"#;

// Injected by the self-test through the webview API, not by the page: the
// document's own CSP forbids scripts. Reports by navigation, like the probe.
const INLINE_CHECK: &str = r#"(() => {
  if (location.pathname !== "/inline-fixture" || window.__pigeonInlineChecked) return;
  const run = () => {
    window.__pigeonInlineChecked = true;
    const images = [...document.images];
    const find = (alt) => images.find((image) => image.alt === alt);
    const logo = find("embedded logo");
    const remote = find("remote pixel");
    const absent = find("absent part");
    const fetched = performance.getEntriesByType("resource").map((entry) => entry.name);
    const results = [
      { name: "inline image rendered", ok: !!logo && logo.complete && logo.naturalWidth === 1 && logo.src.startsWith("data:image/png;base64,"), detail: logo ? "naturalWidth=" + logo.naturalWidth : "missing" },
      { name: "remote image not loaded", ok: !!remote && !remote.getAttribute("src") && remote.naturalWidth === 0, detail: remote ? "src=" + remote.getAttribute("src") : "missing" },
      { name: "unresolved cid dropped", ok: !!absent && !absent.getAttribute("src"), detail: absent ? "src=" + absent.getAttribute("src") : "missing" },
      { name: "no cid: reference remains", ok: !document.documentElement.outerHTML.includes("cid:"), detail: "document scanned" },
      { name: "no network resource requested", ok: fetched.length === 0, detail: fetched.length + " resource entries" },
    ];
    location.href = "/inline-done?r=" + encodeURIComponent(JSON.stringify(results));
  };
  if (document.readyState === "complete") run(); else addEventListener("load", run, { once: true });
})();"#;

// Scripts and IPC connections are permitted here on purpose, so the probe
// exercises the capability layer rather than being stopped by CSP first.
const PROBE_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; \
    connect-src ipc: http://ipc.localhost; style-src 'unsafe-inline'";

const PROBE_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>probe</title>
<style>body{font-family:monospace;margin:1rem}.ok{color:#1a7f37}.bad{color:#cf222e}</style></head>
<body><h1>Viewer isolation probe</h1><ul id="out"></ul>
<script>
const out = document.getElementById("out");
const results = [];
function report(name, ok, detail) {
  results.push({ name, ok, detail });
  const li = document.createElement("li");
  li.className = ok ? "ok" : "bad";
  li.textContent = (ok ? "PASS " : "FAIL ") + name + ": " + detail;
  out.appendChild(li);
}
async function attempt(name, cmd, args) {
  const ipc = window.__TAURI_INTERNALS__;
  if (!ipc) { report(name, true, "no IPC bridge injected"); return; }
  try {
    const value = await ipc.invoke(cmd, args || {});
    report(name, false, "command ran: " + JSON.stringify(value));
  } catch (e) {
    report(name, true, "denied: " + String(e));
  }
}
(async () => {
  await attempt("viewer_probe_reached", "viewer_probe_reached");
  await attempt("foundation_report", "foundation_report");
  await attempt("quit_app", "quit_app");
  await attempt("desktop settings", "desktop_settings");
  await attempt("set desktop settings", "set_desktop_settings", { settings: { closeToBackground: false } });
  await attempt("core window close", "plugin:window|close", { label: "main" });
  await attempt("mail state", "mail_state");
  await attempt("search mail", "search_mail", { query: { text: "" }, requestId: "probe" });
  await attempt("cancel search", "cancel_mail_search", { requestId: "probe" });
  await attempt("read mail", "read_mail_message", { id: 1 });
  await attempt("sync mail", "sync_mail_account", { id: "probe" });
  await attempt("cancel sync", "cancel_mail_sync", { id: "probe" });
  await attempt("unlock mail", "unlock_mail");
  await attempt("open mail", "open_mail_message", { id: 1 });
  await attempt("save search", "save_mail_search", { name: "Probe", query: { text: "" } });
  await attempt("delete search", "delete_mail_search", { id: "probe" });
  await attempt("remove account", "remove_mail_account", { id: "probe" });
  await attempt("update password", "update_mail_password", { id: "probe", password: "synthetic-probe-value" });
  await attempt("queue mail change", "queue_mail_actions", { ids: [1], kind: { type: "delete_permanently" } });
  await attempt("list mail changes", "mail_actions");
  await attempt("retry mail change", "retry_mail_action", { id: 1 });
  await attempt("discard mail change", "discard_mail_action", { id: 1 });
  await attempt("send state", "send_state");
  await attempt("save draft", "save_draft", { draft: { to: "probe@example.test" } });
  await attempt("delete draft", "delete_draft", { id: "probe" });
  await attempt("send draft", "send_draft", { draft: { to: "probe@example.test" } });
  await attempt("return to drafts", "return_to_drafts", { id: "probe" });
  await attempt("retry send", "retry_send", { id: "probe" });
  await attempt("retry sent copy", "retry_sent_copy", { id: "probe" });
  await attempt("resolve send", "resolve_send", { id: "probe", decision: "send_again" });
  await attempt("dismiss outbox", "dismiss_outbox", { id: "probe" });
  await attempt("update mail sending", "update_mail_sending", { id: "probe", settings: {} });
  await attempt("set undo send seconds", "set_undo_send_seconds", { seconds: 0 });
  await attempt("add account", "add_mail_account", { config: { name: "Probe", address: "probe@example.test", host: "example.test", port: 993, username: "probe", security: "tls", ca_certificate: null }, password: "synthetic-probe-value" });
  try {
    await fetch("https://example.org/");
    report("remote fetch", false, "request succeeded");
  } catch (e) {
    report("remote fetch", true, "blocked: " + String(e));
  }
  // Report back by navigation, the only channel a viewer has.
  location.href = "/probe-done?r=" + encodeURIComponent(JSON.stringify(results));
})();
</script></body></html>"#;

/// Headless foundation check for CI and native verification: runs the probe in
/// a real viewer webview, prints every check as JSON, and exits non-zero on
/// any failure. Uses the normal data directory and keyring, so run it with a
/// throwaway HOME and keyring session.
fn start_selftest(app: AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<ShellState>();
        state.probe_opened.store(true, Ordering::SeqCst);
        let opened = {
            let app = app.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = app.clone().run_on_main_thread(move || {
                let _ = tx.send(open_viewer(&app, "probe", "Viewer isolation probe"));
            });
            rx.recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| Err("timed out".into()))
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while opened.is_ok()
            && state.probe_results.lock().unwrap().is_none()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(200));
        }
        let inline = run_inline_check(&app, &state);
        let mut checks = tauri::async_runtime::block_on(collect_checks(&app))
            .unwrap_or_else(|e| vec![Check::new("Collect checks", false, e)]);
        if let Err(e) = opened {
            checks.push(Check::new("Open probe viewer", false, e));
        }
        if let Err(e) = inline {
            checks.push(Check::new(
                "Viewer: embedded image without network",
                false,
                e,
            ));
        }
        let ok = checks.iter().all(|c| c.ok);
        println!("{}", serde_json::to_string_pretty(&checks).unwrap());
        app.exit(if ok { 0 } else { 1 });
    });
}

/// Opens the embedded-image fixture in a real viewer webview and waits for
/// the injected check to report back by navigation.
fn run_inline_check(app: &AppHandle, state: &ShellState) -> Result<(), String> {
    let label = {
        let app = app.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = app.clone().run_on_main_thread(move || {
            let _ = tx.send(open_viewer(&app, "inline-fixture", "Embedded image check"));
        });
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| Err("timed out opening the viewer".into()))?
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while state.inline_results.lock().unwrap().is_none() {
        if Instant::now() > deadline {
            return Err("the embedded image check did not report".into());
        }
        // Retried until the document has loaded; the script runs only once.
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.eval(INLINE_CHECK);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

/// Prints a redacted sync report and exits. Needs no window, and shares the
/// normal store and keyring, so it retries exactly what "Sync now" would.
fn start_sync_diagnostic(app: AppHandle, engine: Arc<MailEngine>) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.hide();
    }
    std::thread::spawn(move || {
        let code = match engine.diagnose_sync() {
            Ok(report) => {
                println!("{report}");
                0
            }
            Err(error) => {
                eprintln!("Pigeon sync diagnostic failed: {error}");
                1
            }
        };
        app.exit(code);
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // A running instance swallows this process via single-instance, silently.
    if std::env::args().any(|arg| arg == DIAGNOSE_SYNC) {
        println!("Running sync diagnostic. If no report follows, quit Pigeon and run this again.");
    }
    tauri::Builder::default()
        // Relaunching from the desktop launcher reopens the running instance.
        // A second `--background` start (login racing a running instance) stays hidden.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().any(|arg| arg == desktop::BACKGROUND) {
                desktop::show_main(app);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .manage(ShellState::default())
        .register_uri_scheme_protocol(VIEWER_SCHEME, |ctx, request| {
            viewer_response(ctx.app_handle(), &request)
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let engine = Arc::new(MailEngine::new(
                data_dir.join("mail.db"),
                Arc::from(secret_store()),
            ));
            app.manage(engine.clone());
            app.manage(desktop::DesktopState::load(app.handle())?);
            if std::env::var_os("PIGEON_SELFTEST").is_some() {
                start_selftest(app.handle().clone());
            } else if std::env::args().any(|arg| arg == DIAGNOSE_SYNC) {
                start_sync_diagnostic(app.handle().clone(), engine);
            } else {
                // The window starts hidden so a login start never flashes it.
                if !std::env::args().any(|arg| arg == desktop::BACKGROUND) {
                    desktop::show_main(app.handle());
                }
                // A single background coordinator keeps account refreshes from
                // multiplying when windows or routes mount. Network work never
                // holds the store mutex; the interface can keep reading/searching.
                let operations = engine.clone();
                // One worker runs queued server changes in order. It idles
                // while the store is locked and wakes when work is queued.
                std::thread::spawn(move || {
                    loop {
                        match operations.run_next_operation() {
                            Ok(true) => {}
                            Ok(false) => operations.wait_for_operations(Duration::from_secs(1)),
                            Err(_) => std::thread::sleep(Duration::from_secs(2)),
                        }
                    }
                });
                std::thread::spawn(move || {
                    loop {
                        if let Ok(accounts) = engine.accounts() {
                            for account in accounts {
                                let _ = engine.sync_account(&account.id);
                            }
                        }
                        std::thread::sleep(Duration::from_secs(60));
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window keeps Pigeon running in the background
            // unless the user turned that off; Quit Pigeon always exits.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == MAIN
            {
                let app = window.app_handle();
                if app.state::<desktop::DesktopState>().close_to_background() {
                    let _ = window.hide();
                    api.prevent_close();
                } else {
                    app.exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            foundation_report,
            open_viewer_fixture,
            open_viewer_probe,
            viewer_probe_reached,
            quit_app,
            desktop::desktop_settings,
            desktop::set_desktop_settings,
            mail::mail_state,
            mail::add_mail_account,
            mail::sync_mail_account,
            mail::cancel_mail_sync,
            mail::search_mail,
            mail::cancel_mail_search,
            mail::read_mail_message,
            mail::save_mail_search,
            mail::delete_mail_search,
            mail::remove_mail_account,
            mail::update_mail_password,
            mail::unlock_mail,
            mail::open_mail_message,
            mail::queue_mail_actions,
            mail::mail_actions,
            mail::retry_mail_action,
            mail::discard_mail_action,
            mail::send_state,
            mail::save_draft,
            mail::delete_draft,
            mail::send_draft,
            mail::return_to_drafts,
            mail::retry_send,
            mail::retry_sent_copy,
            mail::resolve_send,
            mail::dismiss_outbox,
            mail::update_mail_sending,
            mail::set_undo_send_seconds,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pigeon");
}
