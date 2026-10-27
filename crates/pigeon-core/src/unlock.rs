//! Opening the encrypted store through the keyring, with explicit outcomes for
//! every state the interface must show.

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;

use crate::secrets::{KeyringState, SecretError, SecretStore};
use crate::store::{DbKey, Store, StoreError};

pub const DB_KEY_NAME: &str = "database-key";
const DB_KEY_LABEL: &str = "Pigeon mail store key";
// Engine polling and diagnostics can initialise storage concurrently. Do not
// let both observe a missing key and overwrite one another's generated key.
static OPEN_LOCK: Mutex<()> = Mutex::new(());

/// Why the store could not be opened. Each variant is a distinct interface
/// state; none of them falls back to an unencrypted or regenerated store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum StoreBlocked {
    KeyringUnavailable(String),
    KeyringLocked,
    /// The store exists but the keyring has no key for it. Recovery needs the
    /// backup recovery material; never generate a replacement key.
    KeyMissing,
    WrongKey,
    Failed(String),
}

pub enum UnlockOutcome {
    Opened { store: Store, created: bool },
    Blocked(StoreBlocked),
}

pub fn open_store(secrets: &dyn SecretStore, path: &Path) -> UnlockOutcome {
    let _guard = match OPEN_LOCK.lock() {
        Ok(guard) => guard,
        Err(_) => {
            return blocked(StoreBlocked::Failed(
                "store initialisation lock unavailable".into(),
            ));
        }
    };
    match secrets.state() {
        KeyringState::Unavailable(why) => return blocked(StoreBlocked::KeyringUnavailable(why)),
        KeyringState::Locked => return blocked(StoreBlocked::KeyringLocked),
        KeyringState::Unlocked => {}
    }

    let stored = match secrets.get(DB_KEY_NAME) {
        Ok(stored) => stored,
        Err(e) => return blocked(from_secret_error(e)),
    };

    let (key, created) = match stored {
        Some(bytes) => match DbKey::from_bytes(&bytes) {
            Ok(key) => (key, false),
            Err(_) => return blocked(StoreBlocked::WrongKey),
        },
        None if path.exists() => return blocked(StoreBlocked::KeyMissing),
        None => {
            let key = match DbKey::generate() {
                Ok(key) => key,
                Err(e) => return blocked(StoreBlocked::Failed(e.to_string())),
            };
            // Persist the key before creating any data it protects.
            if let Err(e) = secrets.put(DB_KEY_NAME, DB_KEY_LABEL, key.as_bytes()) {
                return blocked(from_secret_error(e));
            }
            (key, true)
        }
    };

    match Store::open(path, &key) {
        Ok(store) => UnlockOutcome::Opened { store, created },
        Err(StoreError::WrongKey) => blocked(StoreBlocked::WrongKey),
        Err(e) => blocked(StoreBlocked::Failed(e.to_string())),
    }
}

fn blocked(reason: StoreBlocked) -> UnlockOutcome {
    UnlockOutcome::Blocked(reason)
}

fn from_secret_error(e: SecretError) -> StoreBlocked {
    match e {
        SecretError::Unavailable(why) => StoreBlocked::KeyringUnavailable(why),
        SecretError::Locked | SecretError::Dismissed => StoreBlocked::KeyringLocked,
        SecretError::Backend(why) => StoreBlocked::Failed(why),
    }
}
