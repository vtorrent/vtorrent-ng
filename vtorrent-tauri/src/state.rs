/// Application state managed by Tauri's state system.
///
/// This holds the in-memory wallet state. The wallet is only unlocked
/// while the app is running — keys are never stored in plaintext on disk.

use std::sync::Mutex;
use vtorrent_wallet::wallet::Wallet;

/// Global application state managed by Tauri.
pub struct AppState {
    /// The active wallet, if one has been loaded and unlocked.
    pub wallet: Mutex<Option<Wallet>>,
    /// Path to the wallet file on disk.
    pub wallet_path: Mutex<Option<std::path::PathBuf>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            wallet: Mutex::new(None),
            wallet_path: Mutex::new(None),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
