//! Network parameters for vTorrent chains.

/// Legacy vTorrent network parameters (original chain, pre-revival).
/// These are used exclusively for parsing and migrating old wallet.dat files.
pub mod legacy {
    /// Base58Check version byte for legacy P2PKH addresses.
    /// Produces addresses starting with 'V' (version byte 70).
    pub const PUBKEY_ADDRESS_PREFIX: u8 = 70;

    /// Base58Check version byte for legacy WIF private keys.
    /// Produces WIF keys starting with '7' (version byte 198).
    pub const SECRET_KEY_PREFIX: u8 = 198;

    /// Legacy P2SH address prefix (version byte 28).
    pub const SCRIPT_ADDRESS_PREFIX: u8 = 28;

    /// Legacy network magic bytes (used in P2P messages).
    pub const NETWORK_MAGIC: [u8; 4] = [0x19, 0x3b, 0x2f, 0x5a];

    /// Legacy P2P port.
    pub const P2P_PORT: u16 = 22524;

    /// Legacy RPC port.
    pub const RPC_PORT: u16 = 22523;

    /// Maximum supply of legacy VTR (20 million coins, 8 decimal places).
    pub const MAX_SUPPLY: u64 = 20_000_000 * 100_000_000;

    /// Coin unit (1 VTR = 100,000,000 satoshis).
    pub const COIN: u64 = 100_000_000;
}

/// New vTorrent 2.0 network parameters (revived chain).
pub mod mainnet {
    /// Base58Check version byte for new P2PKH addresses.
    /// Produces addresses starting with 'V' (same as legacy for UX continuity).
    pub const PUBKEY_ADDRESS_PREFIX: u8 = 70;

    /// Base58Check version byte for new WIF private keys.
    pub const SECRET_KEY_PREFIX: u8 = 198;

    /// New network magic bytes.
    pub const NETWORK_MAGIC: [u8; 4] = [0x56, 0x54, 0x52, 0x32]; // "VTR2"

    /// New P2P port.
    pub const P2P_PORT: u16 = 22526;

    /// New RPC port.
    pub const RPC_PORT: u16 = 22527;

    /// Maximum supply of new VTR2 (20 million coins, 8 decimal places).
    /// Mirrors the legacy supply for 1:1 claim ratio.
    pub const MAX_SUPPLY: u64 = 20_000_000 * 100_000_000;

    /// Coin unit.
    pub const COIN: u64 = 100_000_000;

    /// PoS annual interest rate (5% as per original spec).
    pub const POS_INTEREST_RATE: f64 = 0.05;

    /// Minimum stake age in seconds (6 hours).
    pub const MIN_STAKE_AGE: u64 = 6 * 60 * 60;

    /// Maximum stake age in seconds (6 days).
    pub const MAX_STAKE_AGE: u64 = 6 * 24 * 60 * 60;
}

/// Testnet parameters.
pub mod testnet {
    pub const PUBKEY_ADDRESS_PREFIX: u8 = 111;
    pub const SECRET_KEY_PREFIX: u8 = 239;
    pub const NETWORK_MAGIC: [u8; 4] = [0x56, 0x54, 0x52, 0x54]; // "VTRT"
    pub const P2P_PORT: u16 = 22525;
    pub const RPC_PORT: u16 = 22521;
}
