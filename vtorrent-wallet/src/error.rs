use thiserror::Error;

#[derive(Debug, Error)]
pub enum WalletError {
    #[error("IO error: {0}")]
    Io(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Invalid OTP code")]
    OtpInvalidCode,

    #[error("Wallet file is corrupted or invalid")]
    CorruptedWallet,

    #[error("Incorrect passphrase")]
    IncorrectPassphrase,

    #[error("2FA is enabled — OTP code required")]
    OtpRequired,

    #[error("Incorrect OTP code (check your authenticator app)")]
    IncorrectOtp,

    #[error("OTP invalid code")]
    OtpInvalid,

    #[error("2FA is not enabled on this wallet")]
    OtpNotEnabled,

    #[error("Wallet is locked — unlock with passphrase first")]
    WalletLocked,

    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Duplicate key: {0}")]
    DuplicateKey(String),

    #[error("Encryption error: {0}")]
    EncryptionError(String),

    #[error("Core error: {0}")]
    Core(#[from] vtorrent_core::error::CoreError),

    #[error("JSON error: {0}")]
    Json(String),

    #[error("Insufficient funds: available {available} sats, required {required} sats")]
    InsufficientFunds { available: u64, required: u64 },

    #[error("Signing error: {0}")]
    Signing(String),

    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    #[error("Transaction build error: {0}")]
    BuildError(String),

    #[error("HD derivation error: {0}")]
    HdError(String),

    #[error("Mnemonic error: {0}")]
    MnemonicError(String),
}

pub type Result<T> = std::result::Result<T, WalletError>;
