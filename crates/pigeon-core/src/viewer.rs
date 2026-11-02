//! Message HTML sanitisation and the viewer's content security policy. This is
//! one layer; the viewer also runs in an unprivileged webview with no native
//! capabilities, so a sanitiser bypass alone must not reach native commands.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use ammonia::{Builder, UrlRelative};

/// Largest single embedded image placed in a viewer document.
pub const MAX_INLINE_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Combined embedded-image budget for one viewer document.
pub const MAX_INLINE_DOCUMENT_BYTES: usize = 25 * 1024 * 1024;
/// Raster types only: they are decoded as images and never run script.
const INLINE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
];

/// A downloaded MIME part a message refers to with `cid:`.
#[derive(Debug, Clone)]
pub struct InlineImage {
    pub content_type: String,
    pub data: Vec<u8>,
}

/// Embedded images keyed by normalised Content-ID (see [`normalise_cid`]).
pub type InlineImages = HashMap<String, InlineImage>;

/// The comparable form of a Content-ID or `cid:` URL: percent-decoded, without
/// the scheme or angle brackets.
pub fn normalise_cid(value: &str) -> String {
    let value = value.trim();
    let value = match value.get(..4) {
        Some(scheme) if scheme.eq_ignore_ascii_case("cid:") => &value[4..],
        _ => value,
    };
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_owned()
}

/// Which remote images a document may load. Only https images ever load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteContent {
    Blocked,
    Allowed,
    /// Only images from these hosts, as returned by [`image_host`].
    Hosts(BTreeSet<String>),
}

impl RemoteContent {
    /// Everything either choice allows.
    pub fn union(self, other: RemoteContent) -> RemoteContent {
        match (self, other) {
            (RemoteContent::Allowed, _) | (_, RemoteContent::Allowed) => RemoteContent::Allowed,
            (RemoteContent::Blocked, other) | (other, RemoteContent::Blocked) => other,
            (RemoteContent::Hosts(mut a), RemoteContent::Hosts(b)) => {
                a.extend(b);
                RemoteContent::Hosts(a)
            }
        }
    }

    fn loads(&self, url: &str) -> bool {
        match self {
            RemoteContent::Blocked => false,
            RemoteContent::Allowed => image_host(url).is_some(),
            RemoteContent::Hosts(hosts) => {
                image_host(url).is_some_and(|host| hosts.contains(&host))
            }
        }
    }
}

/// The host (with any port) of an https image URL, lowercased, or `None` for
/// anything else. Deliberately strict: a URL whose host is not plain ASCII
/// letters, digits, dots, hyphens and a port is never loaded, so the host
/// shown to the user, the one compared here and the one in the CSP agree.
pub fn image_host(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])?;
    if rest.contains('\\') {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.to_ascii_lowercase();
    let (name, port) = match host.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host.as_str(), None),
    };
    let name_ok = !name.is_empty()
        && name.len() <= 253
        && !name.starts_with(['.', '-'])
        && !name.ends_with(['.', '-'])
        && name.contains('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    let port_ok = port.is_none_or(|port| {
        !port.is_empty() && port.len() <= 5 && port.chars().all(|c| c.is_ascii_digit())
    });
    (name_ok && port_ok).then_some(host)
}

/// Sanitises untrusted message HTML. Scripts, frames, forms, event handlers,
/// and styles are removed. With remote content blocked, only `data:` and
/// `cid:` images survive.
pub fn sanitise(html: &str, remote: RemoteContent) -> String {
    clean(html, move |element, attribute, value| {
        filter_attribute(&remote, element, attribute, value)
    })
}

/// Content-IDs of the images a message would show, as they survive sanitising.
pub fn referenced_cids(html: &str) -> Vec<String> {
    let found = Arc::new(Mutex::new(Vec::new()));
    let sink = found.clone();
    clean(html, move |element, attribute, value| {
        let kept = filter_attribute(&RemoteContent::Blocked, element, attribute, value);
        if (element, attribute) == ("img", "src")
            && kept.is_some()
            && value.trim_start().to_ascii_lowercase().starts_with("cid:")
            && let Ok(mut found) = sink.lock()
        {
            found.push(normalise_cid(value));
        }
        kept
    });
    std::mem::take(&mut *found.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Each host a message's remote images could load from, with how many
/// images it serves, in the order they first appear.
pub fn remote_image_hosts(html: &str) -> Vec<(String, u32)> {
    let found: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = found.clone();
    clean(html, move |element, attribute, value| {
        if (element, attribute) == ("img", "src")
            && let Some(host) = image_host(value)
            && let Ok(mut found) = sink.lock()
        {
            match found.iter_mut().find(|(known, _)| *known == host) {
                Some((_, count)) => *count += 1,
                None => found.push((host, 1)),
            }
        }
        filter_attribute(&RemoteContent::Blocked, element, attribute, value)
    });
    std::mem::take(&mut *found.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Like [`sanitise`], but resolves `cid:` images to `data:` URLs from parts
/// already downloaded. Unresolved, non-raster, or over-budget images lose
/// their source and show only their alt text; no `cid:` reference remains.
pub fn sanitise_inline(html: &str, remote: RemoteContent, inline: &InlineImages) -> String {
    let inline = Arc::new(inline.clone());
    let budget = Mutex::new(MAX_INLINE_DOCUMENT_BYTES);
    clean(html, move |element, attribute, value| {
        let kept = filter_attribute(&remote, element, attribute, value)?;
        if (element, attribute) != ("img", "src")
            || !value.trim_start().to_ascii_lowercase().starts_with("cid:")
        {
            return Some(kept);
        }
        let image = inline.get(&normalise_cid(value))?;
        let content_type = image.content_type.to_ascii_lowercase();
        if !INLINE_TYPES.contains(&content_type.as_str())
            || image.data.len() > MAX_INLINE_IMAGE_BYTES
        {
            return None;
        }
        let mut left = budget.lock().unwrap_or_else(|e| e.into_inner());
        *left = left.checked_sub(image.data.len())?;
        Some(Cow::Owned(format!(
            "data:{content_type};base64,{}",
            data_encoding::BASE64.encode(&image.data)
        )))
    })
}

fn clean(
    html: &str,
    filter: impl for<'u> Fn(&str, &str, &'u str) -> Option<Cow<'u, str>> + Send + Sync + 'static,
) -> String {
    let mut builder = Builder::default();
    builder
        .url_relative(UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .add_url_schemes(&["cid", "data"])
        .attribute_filter(filter);
    builder.clean(html).to_string()
}

fn filter_attribute<'a>(
    remote: &RemoteContent,
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
            let remote_ok = remote.loads(value);
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
        RemoteContent::Blocked => "img-src data:".to_owned(),
        RemoteContent::Allowed => "img-src data: https:".to_owned(),
        // Hosts are re-checked here: the CSP must never take anything else.
        RemoteContent::Hosts(hosts) => hosts
            .iter()
            .filter(|host| image_host(&format!("https://{host}/")).as_deref() == Some(host))
            .fold("img-src data:".to_owned(), |csp, host| {
                csp + " https://" + host
            }),
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
