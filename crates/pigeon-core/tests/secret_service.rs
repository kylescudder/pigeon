//! Runs against a real Secret Service daemon, so these are ignored by default.
//! Run them via tools/linux-dev/secret-service-test.sh, which provides a
//! throwaway session. Never point them at a real keyring.
#![cfg(target_os = "linux")]

use pigeon_core::secrets::{KeyringState, SecretError, SecretServiceStore, SecretStore};
use secret_service::EncryptionType;
use secret_service::blocking::SecretService;

#[test]
#[ignore = "needs a disposable Secret Service session"]
fn round_trip_then_locked_state_is_explicit() {
    let store = SecretServiceStore;
    assert_eq!(store.state(), KeyringState::Unlocked);

    let name = "test-item";
    store
        .put(name, "Pigeon test item", b"synthetic-secret")
        .unwrap();
    assert_eq!(
        store.get(name).unwrap().as_deref().map(Vec::as_slice),
        Some(&b"synthetic-secret"[..])
    );
    store.put(name, "Pigeon test item", b"replaced").unwrap();
    assert_eq!(
        store.get(name).unwrap().as_deref().map(Vec::as_slice),
        Some(&b"replaced"[..])
    );
    store.delete(name).unwrap();
    assert!(store.get(name).unwrap().is_none());

    let service = SecretService::connect(EncryptionType::Dh).unwrap();
    service.get_default_collection().unwrap().lock().unwrap();
    assert_eq!(store.state(), KeyringState::Locked);
    assert!(matches!(store.get(name), Err(SecretError::Locked)));
    assert!(matches!(
        store.put(name, "x", b"x"),
        Err(SecretError::Locked)
    ));
}

#[test]
#[ignore = "needs an environment without a session bus"]
fn missing_session_bus_is_unavailable() {
    assert!(matches!(
        SecretServiceStore.state(),
        KeyringState::Unavailable(_)
    ));
    assert!(matches!(
        SecretServiceStore.get("x"),
        Err(SecretError::Unavailable(_))
    ));
}
