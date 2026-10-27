use pigeon_core::secrets::{KeyringState, MemoryStore, SecretStore};
use pigeon_core::unlock::{DB_KEY_NAME, StoreBlocked, UnlockOutcome, open_store};

fn blocked(outcome: UnlockOutcome) -> StoreBlocked {
    match outcome {
        UnlockOutcome::Blocked(reason) => reason,
        UnlockOutcome::Opened { .. } => panic!("expected the store to stay closed"),
    }
}

#[test]
fn first_run_generates_and_persists_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let keyring = MemoryStore::new(KeyringState::Unlocked);

    match open_store(&keyring, &path) {
        UnlockOutcome::Opened { created, .. } => assert!(created),
        UnlockOutcome::Blocked(reason) => panic!("{reason:?}"),
    }
    assert!(keyring.get(DB_KEY_NAME).unwrap().is_some());
    match open_store(&keyring, &path) {
        UnlockOutcome::Opened { created, .. } => assert!(!created),
        UnlockOutcome::Blocked(reason) => panic!("{reason:?}"),
    }
}

#[test]
fn unavailable_keyring_blocks_without_creating_anything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let keyring = MemoryStore::new(KeyringState::Unavailable("no session bus".into()));
    assert_eq!(
        blocked(open_store(&keyring, &path)),
        StoreBlocked::KeyringUnavailable("no session bus".into())
    );
    assert!(!path.exists(), "must not create an unprotected store");
}

#[test]
fn locked_keyring_blocks_until_unlocked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let keyring = MemoryStore::new(KeyringState::Unlocked);
    assert!(matches!(
        open_store(&keyring, &path),
        UnlockOutcome::Opened { .. }
    ));

    keyring.set_state(KeyringState::Locked);
    assert_eq!(
        blocked(open_store(&keyring, &path)),
        StoreBlocked::KeyringLocked
    );

    keyring.unlock().unwrap();
    assert!(matches!(
        open_store(&keyring, &path),
        UnlockOutcome::Opened { created: false, .. }
    ));
}

#[test]
fn missing_key_for_existing_store_is_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let keyring = MemoryStore::new(KeyringState::Unlocked);
    assert!(matches!(
        open_store(&keyring, &path),
        UnlockOutcome::Opened { .. }
    ));
    let before = std::fs::read(&path).unwrap();

    keyring.delete(DB_KEY_NAME).unwrap();
    assert_eq!(
        blocked(open_store(&keyring, &path)),
        StoreBlocked::KeyMissing
    );
    assert!(
        keyring.get(DB_KEY_NAME).unwrap().is_none(),
        "no replacement key stored"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "store left untouched"
    );
}

#[test]
fn mismatched_key_reports_wrong_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let keyring = MemoryStore::new(KeyringState::Unlocked);
    assert!(matches!(
        open_store(&keyring, &path),
        UnlockOutcome::Opened { .. }
    ));

    keyring.put(DB_KEY_NAME, "test", &[7u8; 32]).unwrap();
    assert_eq!(blocked(open_store(&keyring, &path)), StoreBlocked::WrongKey);
}

#[test]
fn concurrent_first_opens_share_one_durable_key() {
    use pigeon_core::secrets::{Secret, SecretError};
    use std::sync::Arc;
    struct SlowKeyring(MemoryStore);
    impl SecretStore for SlowKeyring {
        fn state(&self) -> KeyringState {
            self.0.state()
        }
        fn unlock(&self) -> Result<(), SecretError> {
            self.0.unlock()
        }
        fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
            let result = self.0.get(name)?;
            if result.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            Ok(result)
        }
        fn put(&self, name: &str, label: &str, secret: &[u8]) -> Result<(), SecretError> {
            self.0.put(name, label, secret)
        }
        fn delete(&self, name: &str) -> Result<(), SecretError> {
            self.0.delete(name)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let keyring = Arc::new(SlowKeyring(MemoryStore::new(KeyringState::Unlocked)));
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| match open_store(keyring.as_ref(), &path) {
                    UnlockOutcome::Opened { .. } => {}
                    UnlockOutcome::Blocked(reason) => {
                        panic!("concurrent open lost the key: {reason:?}")
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
    assert!(matches!(
        open_store(keyring.as_ref(), &path),
        UnlockOutcome::Opened { created: false, .. }
    ));
}
