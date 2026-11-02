//! Senders whose remote images load without asking. Trust is by the `From`
//! address, which is not verified, so a forged message from a trusted address
//! loads its images too. Loading still only tells that image's server that
//! the message was opened; the viewer can do nothing else on the network.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::reply::first_address;
use super::{MailEngine, MailError, now, storage_error};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TrustedSender {
    pub email: String,
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
                .prepare("SELECT email,added FROM image_trust ORDER BY email")
                .map_err(storage_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok(TrustedSender {
                        email: row.get(0)?,
                        added: row.get(1)?,
                    })
                })
                .map_err(storage_error)?;
            rows.collect::<Result<_, _>>().map_err(storage_error)
        })
    }

    /// Whether this `From` header names a trusted sender.
    pub fn is_trusted_sender(&self, from: &str) -> Result<bool, MailError> {
        let Some(email) = sender_address(from) else {
            return Ok(false);
        };
        self.with_store(|store| {
            store
                .connection()
                .query_row("SELECT 1 FROM image_trust WHERE email=?1", [email], |_| {
                    Ok(())
                })
                .optional()
                .map(|found| found.is_some())
                .map_err(storage_error)
        })
    }

    /// Trusts the sender of a cached message. Taking the message rather than
    /// an address means only a sender the user is looking at can be added.
    pub fn trust_sender_of(&self, message: i64) -> Result<TrustedSender, MailError> {
        let from = self.message(message)?.summary.sender;
        let email = sender_address(&from).ok_or_else(|| {
            MailError::InvalidConfig("this message has no sender address to trust".into())
        })?;
        let added = now();
        self.with_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO image_trust(email,added) VALUES (?1,?2) ON CONFLICT(email) DO NOTHING",
                    params![email, added],
                )
                .map_err(storage_error)
        })?;
        Ok(TrustedSender { email, added })
    }

    pub fn distrust_sender(&self, email: &str) -> Result<(), MailError> {
        self.with_store(|store| {
            store
                .connection()
                .execute("DELETE FROM image_trust WHERE email=?1", [email])
                .map_err(storage_error)?;
            Ok(())
        })
    }
}
