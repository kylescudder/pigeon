use pigeon_core::viewer::{
    InlineImage, InlineImages, MAX_INLINE_IMAGE_BYTES, RemoteContent, content_security_policy,
    document, referenced_cids, sanitise, sanitise_inline,
};

const HOSTILE: &str = include_str!("fixtures/hostile.html");
const HOSTILE_INLINE: &str = include_str!("fixtures/hostile-inline.html");
// A real 1x1 PNG, so the native self-test can also check it renders.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn inline_images() -> InlineImages {
    let mut images = InlineImages::new();
    for (cid, content_type, data) in [
        ("logo@pigeon.test", "image/png", PNG.to_vec()),
        ("banner@pigeon.test", "image/jpeg", b"jpeg-bytes".to_vec()),
        ("vector@pigeon.test", "image/svg+xml", b"<svg/>".to_vec()),
        ("document@pigeon.test", "text/html", b"<script>".to_vec()),
        (
            "huge@pigeon.test",
            "image/png",
            vec![0; MAX_INLINE_IMAGE_BYTES + 1],
        ),
    ] {
        images.insert(
            cid.into(),
            InlineImage {
                content_type: content_type.into(),
                data,
            },
        );
    }
    images
}

#[test]
fn referenced_cids_lists_only_image_sources_after_sanitising() {
    let mut cids = referenced_cids(HOSTILE_INLINE);
    cids.sort();
    cids.dedup();
    assert_eq!(
        cids,
        [
            "banner@pigeon.test",
            "document@pigeon.test",
            "huge@pigeon.test",
            "logo@pigeon.test",
            "missing@pigeon.test",
            "vector@pigeon.test",
        ]
    );
}

#[test]
fn embedded_images_become_data_urls_without_network_references() {
    let clean = sanitise_inline(HOSTILE_INLINE, RemoteContent::Blocked, &inline_images());
    let lower = clean.to_ascii_lowercase();
    assert!(
        clean.contains("src=\"data:image/png;base64,iVBORw0KGgo"),
        "{clean}"
    );
    // The percent-encoded, bracketed and upper-case form still matches.
    assert!(
        clean.contains("src=\"data:image/jpeg;base64,anBlZy1ieXRlcw==\""),
        "{clean}"
    );
    for forbidden in [
        "cid:",
        "image/svg",
        "text/html",
        "<script",
        "onerror",
        "tracker.example",
        "srcset=",
        "background=",
        "<style",
    ] {
        assert!(!lower.contains(forbidden), "{forbidden} survived: {clean}");
    }
    // Unresolved, unsafe and oversized images keep their alt text but no source.
    for alt in ["missing image", "vector", "document", "huge"] {
        assert!(
            clean.contains(&format!("<img alt=\"{alt}\">")),
            "{alt}: {clean}"
        );
    }
    assert!(clean.contains("cid link"));
}

#[test]
fn inline_images_share_a_document_budget() {
    let mut images = InlineImages::new();
    let mut html = String::new();
    for index in 0..4 {
        images.insert(
            format!("part{index}@pigeon.test"),
            InlineImage {
                content_type: "image/png".into(),
                data: vec![1; MAX_INLINE_IMAGE_BYTES],
            },
        );
        html.push_str(&format!("<img src=\"cid:part{index}@pigeon.test\">"));
    }
    let clean = sanitise_inline(&html, RemoteContent::Blocked, &images);
    let inlined = clean.matches("data:image/png").count();
    assert!((1..4).contains(&inlined), "{inlined} images inlined");
}

#[test]
fn remote_policy_is_unchanged_by_inline_images() {
    let clean = sanitise_inline(HOSTILE_INLINE, RemoteContent::Allowed, &inline_images());
    assert!(clean.contains("https://tracker.example/pixel.gif"));
    assert!(clean.contains("data:image/png;base64,"));
    assert!(!clean.contains("cid:"));
}

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
