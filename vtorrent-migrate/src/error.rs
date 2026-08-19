use thiserror::Error;

#[derive(Debug, Error)]
pub enum MigrateError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Not a valid BerkeleyDB wallet.dat file (bad magic bytes)")]
    NotBerkeleyDb,

    #[error("Unsupported BerkeleyDB page size: {0}")]
    UnsupportedPageSize(u32),

    #[error("Unsupported BerkeleyDB version: {0}")]
    UnsupportedVersion(u32),

    #[error("BerkeleyDB page parse error: {0}")]
    PageParseError(String),

    #[error("Wallet is encrypted but no passphrase was provided")]
    EncryptedWalletNoPassphrase,

    #[error("Incorrect passphrase (decryption failed)")]
    IncorrectPassphrase,

    #[error("Unsupported key derivation method: {0}")]
    UnsupportedDerivationMethod(u32),

    #[error("Key derivation iterations exceed the safety limit: {0}")]
    ExcessiveIterations(u32),

    #[error("Wallet requires 2FA OTP code")]
    OtpRequired,

    #[error("Incorrect OTP code")]
    IncorrectOtp,

    #[error("No private keys found in wallet.dat")]
    NoKeysFound,

    #[error("Core error: {0}")]
    Core(#[from] vtorrent_core::error::CoreError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Hex decode error: {0}")]
    HexDecode(#[from] hex::FromHexError),
}

pub type Result<T> = std::result::Result<T, MigrateError>;
