//! Pigeon's mail engine core: encrypted storage, secrets, and content safety.
//! The Tauri shell is a thin adapter over this crate.

pub mod diagnostics;
pub mod secrets;
pub mod store;
pub mod unlock;
pub mod viewer;
