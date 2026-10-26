use pigeon_core::viewer::{RemoteContent, content_security_policy, document, sanitise};

const HOSTILE: &str = include_str!("fixtures/hostile.html");

#[test]
fn blocked_policy_strips_active_and_remote_content() {
    let clean = sanitise(HOSTILE, RemoteContent::Blocked).to_ascii_lowercase();
    for forbidden in [
        "<script",
        "onload",
        "onerror",
        "javascript:",
        "<iframe",
        "<object",
        "<form",
        "<input",
        "<base",
        "<meta",
        "<link",
        "<style",
        "style=",
        "tracker.example",
        "evil.example",
        "data:text/html",
    ] {
        assert!(!clean.contains(forbidden), "{forbidden} survived: {clean}");
    }
    assert!(clean.contains("src=\"cid:logo@pigeon.test\""));
    assert!(clean.contains("src=\"data:image/png;base64,"));
    assert!(clean.contains("href=\"https://example.org/ok\""));
    assert!(clean.contains("rel=\"noopener noreferrer\""));
}

#[test]
fn allowed_policy_loads_https_images_only() {
    let clean = sanitise(HOSTILE, RemoteContent::Allowed);
    assert!(clean.contains("https://tracker.example/pixel.gif"));
    assert!(!clean.contains("http://tracker.example/plain.gif"));
    assert!(!clean.contains("<script"));
}

#[test]
fn csp_denies_scripts_connections_and_frames() {
    let csp = content_security_policy(RemoteContent::Blocked);
    for directive in [
        "default-src 'none'",
        "script-src 'none'",
        "connect-src 'none'",
        "frame-src 'none'",
        "form-action 'none'",
        "img-src data:;",
    ] {
        assert!(csp.contains(directive), "{directive} missing from {csp}");
    }
    assert!(!csp.contains("https:"));
    assert!(document("<p>x</p>", RemoteContent::Blocked).contains(&csp));
}
