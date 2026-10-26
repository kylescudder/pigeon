//! Message HTML sanitisation and the viewer's content security policy. This is
//! one layer; the viewer also runs in an unprivileged webview with no native
//! capabilities, so a sanitiser bypass alone must not reach native commands.

use std::borrow::Cow;

use ammonia::{Builder, UrlRelative};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteContent {
    Blocked,
    Allowed,
}

/// Sanitises untrusted message HTML. Scripts, frames, forms, event handlers,
/// and styles are removed. With remote content blocked, only `data:` and
/// `cid:` images survive.
pub fn sanitise(html: &str, remote: RemoteContent) -> String {
    let mut builder = Builder::default();
    builder
        .url_relative(UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .add_url_schemes(&["cid", "data"])
        .attribute_filter(move |element, attribute, value| {
            filter_attribute(remote, element, attribute, value)
        });
    builder.clean(html).to_string()
}

fn filter_attribute<'a>(
    remote: RemoteContent,
    element: &str,
    attribute: &str,
    value: &'a str,
) -> Option<Cow<'a, str>> {
    let lower = value.trim_start().to_ascii_lowercase();
    match (element, attribute) {
        ("a", "href") => (lower.starts_with("https:")
            || lower.starts_with("http:")
            || lower.starts_with("mailto:"))
        .then_some(Cow::Borrowed(value)),
        ("img", "src") => {
            let inline = lower.starts_with("cid:") || lower.starts_with("data:image/");
            let remote_ok = remote == RemoteContent::Allowed && lower.starts_with("https:");
            (inline || remote_ok).then_some(Cow::Borrowed(value))
        }
        // `data:` is only permitted for images; drop it anywhere else.
        _ if lower.starts_with("data:") || lower.starts_with("cid:") => None,
        _ => Some(Cow::Borrowed(value)),
    }
}

/// CSP served with every viewer document. No scripts, no connections, no
/// frames, no form submission; images limited by the remote-content choice.
pub fn content_security_policy(remote: RemoteContent) -> String {
    let img = match remote {
        RemoteContent::Blocked => "img-src data:",
        RemoteContent::Allowed => "img-src data: https:",
    };
    format!(
        "default-src 'none'; {img}; style-src 'unsafe-inline'; script-src 'none'; \
         connect-src 'none'; frame-src 'none'; object-src 'none'; form-action 'none'; \
         base-uri 'none'; frame-ancestors 'none'"
    )
}

/// Wraps sanitised HTML in a complete viewer document.
pub fn document(sanitised_body: &str, remote: RemoteContent) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <style>body{{font-family:system-ui,sans-serif;margin:1rem;overflow-wrap:anywhere}}\
         img{{max-width:100%}}</style></head><body>{sanitised_body}</body></html>",
        csp = content_security_policy(remote),
    )
}
