//! Encrypted backup and restore of the engine's local-only data.

use std::path::{Path, PathBuf};

use super::{MailEngine, MailError, RESTORE_LEASE, now, storage_error};
use crate::backup::{self, BackupError, Summary};
use crate::secrets::{KeyringState, Secret};
use crate::store::{DbKey, Store};
use crate::unlock::{DB_KEY_LABEL, DB_KEY_NAME};

impl MailEngine {
    /// Writes an encrypted backup of local-only data to `dest`. Sync and
    /// reading carry on meanwhile: the backup reads one consistent snapshot
    /// through its own connection instead of holding the engine's store.
    pub fn create_backup(&self, dest: &Path, passphrase: &str) -> Result<Summary, MailError> {
        backup::check_passphrase(passphrase)?;
        let _recovery = self.recovery.lock().map_err(|_| MailError::Busy)?;
        // Opens and migrates the store, and reports a locked or missing key.
        self.with_store(|_| Ok(()))?;
        let key = self.store_key()?.ok_or(MailError::KeyMissing)?;
        let mut payload = {
            let snapshot =
                Store::open(&self.path, &key).map_err(|e| MailError::Storage(e.to_string()))?;
            let tx = snapshot
                .connection()
                .unchecked_transaction()
                .map_err(storage_error)?;
            backup::snapshot(&tx)?
        };
        for id in payload.account_ids() {
            let password = self
                .secrets
                .get(&backup::password_name(&id))
                .map_err(|_| MailError::Locked)?;
            // An account without a saved password asks for one after restore.
            if let Some(password) = password {
                payload.add_password(&id, &password);
            }
        }
        backup::write(&payload, dest, passphrase)?;
        Ok(payload.summary())
    }

    /// Replaces local-only data with the backup at `src`. Nothing changes
    /// unless the backup decrypts and loads completely. The replaced store is
    /// kept beside the new one rather than deleted.
    pub fn restore_backup(&self, src: &Path, passphrase: &str) -> Result<Summary, MailError> {
        let _recovery = self.recovery.lock().map_err(|_| MailError::Busy)?;
        let _lease = self.lease(RESTORE_LEASE).map_err(|error| match error {
            MailError::Busy => BackupError::Busy.into(),
            other => other,
        })?;
        if self.secrets.state() != KeyringState::Unlocked {
            return Err(MailError::Locked);
        }
        let payload = backup::read(src, passphrase)?;
        let passwords = payload.passwords()?;
        // Keep this device's key when there is one. Without it the old store
        // is unreadable anyway, so the restore starts a new key.
        let (key, new_key) = match self.store_key()? {
            Some(key) => (key, false),
            None => (
                DbKey::generate().map_err(|e| MailError::Storage(e.to_string()))?,
                true,
            ),
        };

        let staging = backup::sibling(&self.path, "restore")?;
        if let Err(error) = backup::build_store(&staging, &key, &payload) {
            remove_store_files(&staging);
            return Err(error.into());
        }
        let result = self.swap_in(&staging, &key, new_key, &passwords);
        if result.is_err() {
            remove_store_files(&staging);
        }
        let mut summary = payload.summary();
        summary.previous_store = result?;
        Ok(summary)
    }

    fn swap_in(
        &self,
        staging: &Path,
        key: &DbKey,
        new_key: bool,
        passwords: &[backup::KeyringItem],
    ) -> Result<Option<String>, MailError> {
        // Remember what the keyring held before the first write, so that a
        // failed restore leaves the live accounts' passwords as they were.
        let mut saved: Vec<(&str, Option<Secret>)> = Vec::with_capacity(passwords.len());
        for (name, _) in passwords {
            let current = self.secrets.get(name).map_err(|_| MailError::Locked)?;
            saved.push((name, current));
        }
        let result = self.swap_files(staging, key, new_key, passwords);
        if result.is_err() {
            for (name, previous) in &saved {
                let _ = match previous {
                    Some(password) => {
                        self.secrets
                            .put(name, "Pigeon mail account password", password)
                    }
                    None => self.secrets.delete(name),
                };
            }
        }
        result
    }

    fn swap_files(
        &self,
        staging: &Path,
        key: &DbKey,
        new_key: bool,
        passwords: &[backup::KeyringItem],
    ) -> Result<Option<String>, MailError> {
        for (name, password) in passwords {
            self.secrets
                .put(name, "Pigeon mail account password", password)
                .map_err(|_| MailError::Locked)?;
        }
        let mut guard = self
            .store
            .lock()
            .map_err(|_| MailError::Storage("mail store lock unavailable".into()))?;
        // Close the live store so its WAL is checkpointed before it moves.
        *guard = None;
        let undo = |previous: &Option<String>| {
            if let Some(name) = previous {
                let _ = move_store_files(&self.path.with_file_name(name), &self.path);
            }
        };
        let previous = if self.path.exists() {
            let name = format!(
                "{}.before-restore-{}",
                self.path.file_name().unwrap_or_default().to_string_lossy(),
                now()
            );
            if let Err(error) = move_store_files(&self.path, &self.path.with_file_name(&name)) {
                // Some of the store's files may already have moved.
                undo(&Some(name));
                return Err(BackupError::Io(error.to_string()).into());
            }
            Some(name)
        } else {
            None
        };
        if new_key && let Err(error) = self.secrets.put(DB_KEY_NAME, DB_KEY_LABEL, key.as_bytes()) {
            undo(&previous);
            return Err(MailError::Storage(error.to_string()));
        }
        if let Err(error) = std::fs::rename(staging, &self.path) {
            undo(&previous);
            if new_key {
                let _ = self.secrets.delete(DB_KEY_NAME);
            }
            return Err(BackupError::Io(error.to_string()).into());
        }
        backup::sync_parent(&self.path);
        Ok(previous)
    }

    fn store_key(&self) -> Result<Option<DbKey>, MailError> {
        match self
            .secrets
            .get(DB_KEY_NAME)
            .map_err(|_| MailError::Locked)?
        {
            Some(bytes) => DbKey::from_bytes(&bytes)
                .map(Some)
                .map_err(|e| MailError::Storage(e.to_string())),
            None => Ok(None),
        }
    }
}

fn store_files(path: &Path) -> [PathBuf; 3] {
    let with = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    [path.to_owned(), with("-wal"), with("-shm")]
}

fn move_store_files(from: &Path, to: &Path) -> std::io::Result<()> {
    for (from, to) in store_files(from).iter().zip(store_files(to)) {
        if from.exists() {
            std::fs::rename(from, to)?;
        }
    }
    Ok(())
}

fn remove_store_files(path: &Path) {
    for file in store_files(path) {
        let _ = std::fs::remove_file(file);
    }
}
