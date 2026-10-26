//! Secret storage. The desktop keyring (Linux Secret Service) is the only
//! persistent backend; there is deliberately no plaintext fallback.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use zeroize::Zeroizing;

pub type Secret = Zeroizing<Vec<u8>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum KeyringState {
    /// No Secret Service is reachable (no daemon, no session bus).
    Unavailable(String),
    /// The service exists but the collection is locked; unlocking needs the user.
    Locked,
    Unlocked,
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("the desktop keyring is unavailable: {0}")]
    Unavailable(String),
    #[error("the desktop keyring is locked")]
    Locked,
    #[error("the user dismissed the keyring unlock prompt")]
    Dismissed,
    #[error("keyring error: {0}")]
    Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn state(&self) -> KeyringState;
    /// Asks the backend to unlock, which may show a system prompt. Only call
    /// in response to a user action.
    fn unlock(&self) -> Result<(), SecretError>;
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError>;
    fn put(&self, name: &str, label: &str, secret: &[u8]) -> Result<(), SecretError>;
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

/// Session-only storage: secrets live in process memory and vanish on exit.
/// Also used by tests to model keyring states.
pub struct MemoryStore {
    state: Mutex<KeyringState>,
    items: Mutex<HashMap<String, Secret>>,
}

impl MemoryStore {
    pub fn new(state: KeyringState) -> Self {
        Self {
            state: Mutex::new(state),
            items: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_state(&self, state: KeyringState) {
        *self.state.lock().unwrap() = state;
    }

    fn require_unlocked(&self) -> Result<(), SecretError> {
        match self.state() {
            KeyringState::Unlocked => Ok(()),
            KeyringState::Locked => Err(SecretError::Locked),
            KeyringState::Unavailable(why) => Err(SecretError::Unavailable(why)),
        }
    }
}

impl SecretStore for MemoryStore {
    fn state(&self) -> KeyringState {
        self.state.lock().unwrap().clone()
    }

    fn unlock(&self) -> Result<(), SecretError> {
        match self.state() {
            KeyringState::Unavailable(why) => Err(SecretError::Unavailable(why)),
            _ => {
                self.set_state(KeyringState::Unlocked);
                Ok(())
            }
        }
    }

    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        self.require_unlocked()?;
        Ok(self.items.lock().unwrap().get(name).cloned())
    }

    fn put(&self, name: &str, _label: &str, secret: &[u8]) -> Result<(), SecretError> {
        self.require_unlocked()?;
        self.items
            .lock()
            .unwrap()
            .insert(name.to_owned(), Zeroizing::new(secret.to_vec()));
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.require_unlocked()?;
        self.items.lock().unwrap().remove(name);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub use linux::SecretServiceStore;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;

    use secret_service::EncryptionType;
    use secret_service::blocking::{Collection, SecretService};
    use zeroize::Zeroizing;

    use super::{KeyringState, Secret, SecretError, SecretStore};

    const APP_ATTR: &str = "application";
    const APP_ID: &str = "uk.co.kylescudder.pigeon";
    const NAME_ATTR: &str = "pigeon-item";

    /// Linux Secret Service backend (GNOME Keyring, KWallet, KeePassXC...).
    /// Uses the default collection and never auto-unlocks.
    pub struct SecretServiceStore;

    impl SecretServiceStore {
        fn with_collection<T>(
            f: impl FnOnce(&Collection<'_>) -> Result<T, SecretError>,
        ) -> Result<T, SecretError> {
            let service = SecretService::connect(EncryptionType::Dh)
                .map_err(|e| SecretError::Unavailable(e.to_string()))?;
            let collection = service
                .get_default_collection()
                .map_err(|e| SecretError::Unavailable(e.to_string()))?;
            f(&collection)
        }

        fn ensure_unlocked(collection: &Collection<'_>) -> Result<(), SecretError> {
            match collection.is_locked() {
                Ok(false) => Ok(()),
                Ok(true) => Err(SecretError::Locked),
                Err(e) => Err(SecretError::Backend(e.to_string())),
            }
        }

        fn attributes(name: &str) -> HashMap<&str, &str> {
            HashMap::from([(APP_ATTR, APP_ID), (NAME_ATTR, name)])
        }
    }

    fn backend(e: secret_service::Error) -> SecretError {
        match e {
            secret_service::Error::Locked => SecretError::Locked,
            secret_service::Error::Prompt => SecretError::Dismissed,
            other => SecretError::Backend(other.to_string()),
        }
    }

    impl SecretStore for SecretServiceStore {
        fn state(&self) -> KeyringState {
            match Self::with_collection(|c| c.is_locked().map_err(backend)) {
                Ok(false) => KeyringState::Unlocked,
                Ok(true) => KeyringState::Locked,
                Err(SecretError::Unavailable(why)) => KeyringState::Unavailable(why),
                Err(e) => KeyringState::Unavailable(e.to_string()),
            }
        }

        fn unlock(&self) -> Result<(), SecretError> {
            Self::with_collection(|c| c.unlock().map_err(backend))
        }

        fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
            Self::with_collection(|c| {
                Self::ensure_unlocked(c)?;
                let items = c.search_items(Self::attributes(name)).map_err(backend)?;
                match items.first() {
                    None => Ok(None),
                    Some(item) => Ok(Some(Zeroizing::new(item.get_secret().map_err(backend)?))),
                }
            })
        }

        fn put(&self, name: &str, label: &str, secret: &[u8]) -> Result<(), SecretError> {
            Self::with_collection(|c| {
                Self::ensure_unlocked(c)?;
                c.create_item(
                    label,
                    Self::attributes(name),
                    secret,
                    true,
                    "application/octet-stream",
                )
                .map_err(backend)?;
                Ok(())
            })
        }

        fn delete(&self, name: &str) -> Result<(), SecretError> {
            Self::with_collection(|c| {
                Self::ensure_unlocked(c)?;
                for item in c.search_items(Self::attributes(name)).map_err(backend)? {
                    item.delete().map_err(backend)?;
                }
                Ok(())
            })
        }
    }
}
