//! Setu library — re-exports testable modules.
//!
//! The binary entry point is `main.rs`; this crate exposes the core logic
//! so unit tests can run on the host (Linux) without linking the full
//! Windows GUI / tray dependencies.

pub mod auth;
pub mod config;
pub mod db;
pub mod google_api;
pub mod server;
pub mod tls;
pub mod vault;
pub mod vcard;
