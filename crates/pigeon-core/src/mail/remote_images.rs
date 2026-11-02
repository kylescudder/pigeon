//! Senders whose remote images load without asking, either from every host
//! (`image_trust`) or only from chosen hosts (`image_trust_hosts`). Trust is
//! by the `From` address, which is not verified, so a forged message from a
//! trusted address loads the same images. Loading still only tells that
//! image's server that the message was opened; the viewer can do nothing
//! else on the network.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::reply::first_address;
use super::{MailEngine, MailError, now, storage_error};
use crate::viewer::{RemoteContent, image_host, remote_image_hosts};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TrustedSender {
    pub email: String,
    /// Images load from every host.
    pub all: bool,
    /// Hosts trusted for this sender alone. Empty when `all` is set.
    pub hosts: Vec<String>,
    pub added: i64,
}

/// The address a `From` header names, as trust is keyed.
pub fn sender_address(from: &str) -> Option<String> {
    first_address(from).map(|address| address.to_lowercase())
}

impl MailEngine {
    pub fn trusted_senders(&self) -> Result<Vec<TrustedSender>, MailError> {
        self.with_store(|store| {
            let conn = store.connection();
            let mut statement = conn
                .prepare(
                    "SELECT email,'',added FROM image_trust
                     UNION ALL SELECT email,host,added FROM image_trust_hosts
                     ORDER BY 1 COLLATE NOCASE, 2",
                )
                .map_err(storage_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(storage_error)?;
            let mut senders: Vec<TrustedSender> = Vec::new();
            for row in rows {
                let (email, host, added) = row.map_err(storage_error)?;
                let sender = match senders.last_mut() {
                    Some(last) if last.email.eq_ignore_ascii_case(&email) => last,
                    _ => {
                        senders.push(TrustedSender {
                            email,
                            all: false,
                            hosts: Vec::new(),
                            added,
                        });
                        senders.last_mut().expect("just pushed")
                    }
                };
                sender.added = sender.added.min(added);
                match host.is_empty() {
                    true => sender.all = true,
                    false => sender.hosts.push(host),
                }
            }
            for sender in &mut senders {
                if sender.all {
                    sender.hosts.clear();
                }
            }
            Ok(senders)
        })
    }

    /// The remote images this `From` header's sender is trusted for.
    pub fn sender_trust(&self, from: &str) -> Result<RemoteContent, MailError> {
        let Some(email) = sender_address(from) else {
            return Ok(RemoteContent::Blocked);
        };
        self.with_store(|store| {
            let conn = store.connection();
            let all = conn
                .query_row("SELECT 1 FROM image_trust WHERE email=?1", [&email], |_| {
                    Ok(())
                })
                .optional()
                .map_err(storage_error)?
                .is_some();
            if all {
                return Ok(RemoteContent::Allowed);
            }
            let mut statement = conn
                .prepare("SELECT host FROM image_trust_hosts WHERE email=?1")
                .map_err(storage_error)?;
            let hosts = statement
                .query_map([&email], |row| row.get::<_, String>(0))
                .map_err(storage_error)?
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(storage_error)?;
            Ok(match hosts.is_empty() {
                true => RemoteContent::Blocked,
                false => RemoteContent::Hosts(hosts),
            })
        })
    }

    /// Trusts the sender of a cached message, for every host or for `hosts`
    /// only. Taking the message rather than an address means only a sender
    /// the user is looking at can be added, and only for hosts its message
    /// actually loads from.
    pub fn trust_sender_of(
        &self,
        message: i64,
        hosts: Option<Vec<String>>,
    ) -> Result<TrustedSender, MailError> {
        let message = self.message(message)?;
        let email = sender_address(&message.summary.sender).ok_or_else(|| {
            MailError::InvalidConfig("this message has no sender address to trust".into())
        })?;
        let hosts = match hosts {
            None => None,
            Some(wanted) => {
                let present: BTreeSet<String> = message
                    .body_html
                    .as_deref()
                    .map(remote_image_hosts)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(host, _)| host)
                    .collect();
                let mut chosen = BTreeSet::new();
                for host in wanted {
                    let host = image_host(&format!("https://{}/", host.trim()))
                        .filter(|host| present.contains(host))
                        .ok_or_else(|| {
                            MailError::InvalidConfig(
                                "choose image sources this message loads from".into(),
                            )
                        })?;
                    chosen.insert(host);
                }
                if chosen.is_empty() {
                    return Err(MailError::InvalidConfig(
                        "choose at least one image source".into(),
                    ));
                }
                Some(chosen)
            }
        };
        let added = now();
        self.with_store(|store| {
            let tx = store.connection().unchecked_transaction().map_err(storage_error)?;
            match &hosts {
                // Every host makes the per-host list redundant.
                None => {
                    tx.execute(
                        "INSERT INTO image_trust(email,added) VALUES (?1,?2) ON CONFLICT(email) DO NOTHING",
                        params![email, added],
                    )
                    .map_err(storage_error)?;
                    tx.execute("DELETE FROM image_trust_hosts WHERE email=?1", [&email])
                        .map_err(storage_error)?;
                }
                Some(hosts) => {
                    for host in hosts {
                        tx.execute(
                            "INSERT INTO image_trust_hosts(email,host,added) VALUES (?1,?2,?3) ON CONFLICT(email,host) DO NOTHING",
                            params![email, host, added],
                        )
                        .map_err(storage_error)?;
                    }
                }
            }
            tx.commit().map_err(storage_error)
        })?;
        self.trusted_senders()?
            .into_iter()
            .find(|sender| sender.email.eq_ignore_ascii_case(&email))
            .ok_or(MailError::NotFound)
    }

    /// Stops trusting a sender entirely, or only for one host.
    pub fn distrust_sender(&self, email: &str, host: Option<&str>) -> Result<(), MailError> {
        self.with_store(|store| {
            let conn = store.connection();
            match host {
                Some(host) => conn
                    .execute(
                        "DELETE FROM image_trust_hosts WHERE email=?1 AND host=?2",
                        [email, host],
                    )
                    .map_err(storage_error)?,
                None => {
                    conn.execute("DELETE FROM image_trust WHERE email=?1", [email])
                        .map_err(storage_error)?;
                    conn.execute("DELETE FROM image_trust_hosts WHERE email=?1", [email])
                        .map_err(storage_error)?
                }
            };
            Ok(())
        })
    }
}
