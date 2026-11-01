//! Website icons shown beside senders, once the user turns them on.
//!
//! A message must not decide where Pigeon connects: a request to the sender's
//! own server would tell them their mail was seen, and a forged `From` could
//! aim it at a private address. So every icon comes through one fixed service,
//! which learns a registrable domain and never an address. Subdomains are
//! dropped for the same reason, since a sender can mint one per recipient.
//!
//! Icons are kept in the encrypted store and reach the interface as `data:`
//! URLs, so the main window's content security policy stays closed to the
//! network. They are decoration: a `From` address is not verified, so an icon
//! says nothing about who really sent a message.

use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use super::{MailEngine, MailError, now, storage_error};

const SERVICE: &str = "https://icons.duckduckgo.com/ip3";
const MAX_ICON_BYTES: u64 = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const ICON_LIFETIME: i64 = 30 * 24 * 60 * 60;
const NO_ICON_LIFETIME: i64 = 7 * 24 * 60 * 60;
/// Shared by the people using them, so an icon would only name their provider.
const PERSONAL_MAIL: &[&str] = &[
    "aol.com",
    "btinternet.com",
    "fastmail.com",
    "fastmail.fm",
    "gmail.com",
    "gmx.com",
    "gmx.de",
    "gmx.net",
    "googlemail.com",
    "hey.com",
    "hotmail.co.uk",
    "hotmail.com",
    "icloud.com",
    "live.co.uk",
    "live.com",
    "mac.com",
    "mail.com",
    "mail.ru",
    "me.com",
    "msn.com",
    "outlook.com",
    "pm.me",
    "proton.me",
    "protonmail.com",
    "qq.com",
    "sky.com",
    "tuta.io",
    "tutanota.com",
    "virginmedia.com",
    "web.de",
    "yahoo.co.uk",
    "yahoo.com",
    "yandex.com",
    "yandex.ru",
    "ymail.com",
    "zoho.com",
];

/// Where icons come from. `Ok(None)` means the domain has none, which is
/// remembered; an error means ask again later, and nothing is remembered.
pub trait IconSource: Send + Sync {
    fn fetch(&self, domain: &str) -> Result<Option<Vec<u8>>, MailError>;
}

/// The fixed icon service, over TLS verified against the system's roots.
pub(super) struct IconService {
    agent: ureq::Agent,
}

impl Default for IconService {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent("Pigeon")
            .timeout_global(Some(REQUEST_TIMEOUT))
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::NativeTls)
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .new_agent();
        Self { agent }
    }
}

impl IconSource for IconService {
    fn fetch(&self, domain: &str) -> Result<Option<Vec<u8>>, MailError> {
        let response = self
            .agent
            .get(format!("{SERVICE}/{domain}.ico"))
            .header("Accept", "image/*")
            .call()
            .map_err(|_| MailError::Connection)?;
        read_icon(response)
    }
}

/// Only a 200 is an icon. A missing one arrives as a 404 carrying a placeholder
/// image, which must not be shown as though it were the sender's.
fn read_icon(mut response: ureq::http::Response<ureq::Body>) -> Result<Option<Vec<u8>>, MailError> {
    match response.status().as_u16() {
        200 => match response
            .body_mut()
            .with_config()
            .limit(MAX_ICON_BYTES)
            .read_to_vec()
        {
            Ok(data) => Ok(Some(data)),
            Err(ureq::Error::BodyExceedsLimit(_)) => Ok(None),
            Err(_) => Err(MailError::Connection),
        },
        404 => Ok(None),
        _ => Err(MailError::Connection),
    }
}

/// The name to look up for a sender's domain, or `None` if it should not be
/// looked up at all: private or reserved names, addresses, and personal mail.
fn icon_domain(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let well_formed = domain.len() <= 253
        && domain.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if !well_formed {
        return None;
    }
    let registrable = psl::domain(domain.as_bytes())?;
    if !registrable.suffix().is_known() {
        return None;
    }
    let name = std::str::from_utf8(registrable.as_bytes()).ok()?;
    (!PERSONAL_MAIL.contains(&name)).then(|| name.to_owned())
}

/// Identified from the bytes, as the viewer's images are limited to raster
/// types: nothing here can carry script, and a service's label is not trusted.
fn image_type(data: &[u8]) -> Option<&'static str> {
    match data {
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Some("image/png"),
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        [0, 0, 1, 0, ..] => Some("image/x-icon"),
        _ => None,
    }
}

fn data_url(data: &[u8]) -> Option<String> {
    let content_type = image_type(data)?;
    Some(format!(
        "data:{content_type};base64,{}",
        data_encoding::BASE64.encode(data)
    ))
}

impl MailEngine {
    /// A sender domain's icon as a `data:` URL, from the store or else fetched.
    /// `None` means show the sender's initials.
    pub fn sender_image(&self, domain: &str) -> Result<Option<String>, MailError> {
        let Some(domain) = icon_domain(domain) else {
            return Ok(None);
        };
        let cached = self.with_store(|store| {
            store
                .connection()
                .query_row(
                    "SELECT data,checked FROM sender_images WHERE domain=?1",
                    [&domain],
                    |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(storage_error)
        })?;
        if let Some((data, checked)) = &cached {
            let lifetime = match data {
                Some(_) => ICON_LIFETIME,
                None => NO_ICON_LIFETIME,
            };
            if now() - checked < lifetime {
                return Ok(data.as_deref().and_then(data_url));
            }
        }
        // The store is free while this runs, so a slow request blocks no one.
        let data = match self.icons.fetch(&domain) {
            Ok(data) => data.filter(|data| image_type(data).is_some()),
            // Unreachable, an old answer still beats none.
            Err(_) if cached.is_some() => {
                return Ok(cached.and_then(|(data, _)| data_url(&data?)));
            }
            Err(error) => return Err(error),
        };
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT OR REPLACE INTO sender_images(domain,data,checked) VALUES (?1,?2,?3)",
                    params![domain, data, now()],
                )
                .map_err(storage_error)
        })?;
        Ok(data.as_deref().and_then(data_url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0];

    fn response(status: u16, body: Vec<u8>) -> ureq::http::Response<ureq::Body> {
        ureq::http::Response::builder()
            .status(status)
            .body(ureq::Body::builder().data(body))
            .unwrap()
    }

    #[test]
    fn subdomains_collapse_so_a_sender_cannot_mint_one_per_recipient() {
        assert_eq!(
            icon_domain("u8241.em.Example.co.uk.").as_deref(),
            Some("example.co.uk")
        );
        assert_eq!(
            icon_domain("notifications.github.com").as_deref(),
            Some("github.com")
        );
    }

    #[test]
    fn private_reserved_and_malformed_names_are_never_looked_up() {
        for domain in [
            "",
            "localhost",
            "router.local",
            "mail.internal",
            "pigeon.test",
            "com",
            "co.uk",
            "10.0.0.1",
            "127.0.0.1",
            "[192.168.1.1]",
            "[ipv6:::1]",
            "bücher.example",
            "a..example.com",
            "example.com/../../etc",
            "example.com?x=1",
            "example.com#fragment",
            "user@example.com",
            "example.com:8443",
        ] {
            assert_eq!(icon_domain(domain), None, "{domain:?}");
        }
    }

    #[test]
    fn personal_mail_domains_are_skipped_but_their_companies_are_not() {
        assert_eq!(icon_domain("gmail.com"), None);
        assert_eq!(icon_domain("mail.gmail.com"), None);
        assert_eq!(
            icon_domain("accounts.google.com").as_deref(),
            Some("google.com")
        );
    }

    #[test]
    fn a_missing_icons_placeholder_is_not_taken_for_an_icon() {
        assert_eq!(read_icon(response(404, PNG.to_vec())).unwrap(), None);
    }

    #[test]
    fn only_a_clear_answer_counts_as_having_no_icon() {
        assert!(read_icon(response(429, PNG.to_vec())).is_err());
        assert!(read_icon(response(503, PNG.to_vec())).is_err());
        // Redirects are not followed, so they must not read as an icon either.
        assert!(read_icon(response(302, PNG.to_vec())).is_err());
    }

    #[test]
    fn an_oversized_icon_is_treated_as_none() {
        let data = vec![0; MAX_ICON_BYTES as usize + 1];
        assert_eq!(read_icon(response(200, data)).unwrap(), None);
        let data = PNG.to_vec();
        assert_eq!(read_icon(response(200, data.clone())).unwrap(), Some(data));
    }

    #[test]
    fn only_raster_images_become_data_urls() {
        assert!(data_url(PNG).unwrap().starts_with("data:image/png;base64,"));
        assert!(
            data_url(&[0, 0, 1, 0, 2])
                .unwrap()
                .starts_with("data:image/x-icon;base64,")
        );
        assert_eq!(data_url(b"<svg onload='alert(1)'>"), None);
        assert_eq!(data_url(b"<html>"), None);
        assert_eq!(data_url(&[]), None);
    }
}
