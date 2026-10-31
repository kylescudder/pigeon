//! Pigeon's mail engine core: encrypted storage, secrets, and content safety.
//! The Tauri shell is a thin adapter over this crate.

pub mod backup;
pub mod desktop;
pub mod diagnostics;
pub mod mail;
pub mod secrets;
pub mod store;
pub mod unlock;
pub mod viewer;
