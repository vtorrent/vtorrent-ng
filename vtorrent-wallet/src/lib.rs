/// vtorrent-wallet: Wallet management, 2FA/TOTP, and key encryption for vTorrent 2.0.
///
/// This crate provides:
/// - Wallet creation and management
/// - TOTP-based 2FA (compatible with Google Authenticator / Authy)
/// - Argon2id key derivation for strong passphrase protection
/// - ChaCha20-Poly1305 authenticated encryption for wallet storage
/// - Legacy wallet.dat migration support

pub mod error;
pub mod otp;
pub mod encryption;
pub mod wallet;
