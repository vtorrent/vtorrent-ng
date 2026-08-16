/// vTorrent 2.0 Tauri Backend
///
/// This crate exposes Tauri IPC commands that the React frontend can call
/// via `invoke()`. All sensitive operations (key extraction, encryption,
/// TOTP verification) happen here in Rust — never in JavaScript.
pub mod commands;
pub mod error;
pub mod state;
