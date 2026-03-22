use serde::{Deserialize, Serialize};

// ─── Node / Chain ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeInfoResponse {
    pub version: String,
    pub network: String,
    pub block_height: u64,
    pub best_block_hash: String,
    pub connections: usize,
    pub syncing: bool,
    pub uptime_secs: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockHeightResponse {
    pub height: u64,
    pub hash: String,
    pub timestamp: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockResponse {
    pub hash: String,
    pub height: u64,
    pub version: u32,
    pub prev_hash: String,
    pub merkle_root: String,
    pub timestamp: u32,
    pub bits: u32,
    pub nonce: u32,
    pub tx_count: usize,
    pub size_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MempoolResponse {
    pub count: usize,
    pub size_bytes: usize,
    pub txids: Vec<String>,
}

// ─── Wallet ───────────────────────────────────────────────────────────────────

/// A single confirmed transaction summary for the wallet history endpoint.
#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionResponse {
    pub txid: String,
    pub block_height: u32,
    pub timestamp: u32,
    pub tx_type: String,
    pub amount_satoshis: u64,
    pub display: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub confirmed: u64,
    pub unconfirmed: u64,
    pub staking: u64,
    pub display: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AddressesResponse {
    pub addresses: Vec<AddressInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AddressInfo {
    pub address: String,
    pub label: Option<String>,
    pub balance: u64,
    pub is_change: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendRequest {
    pub to_address: String,
    pub amount_satoshis: u64,
    pub passphrase: String,
    pub otp_code: Option<String>,
    pub memo: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendResponse {
    pub txid: String,
    pub amount_satoshis: u64,
    pub fee_satoshis: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnlockRequest {
    pub passphrase: String,
    pub otp_code: Option<String>,
    /// Seconds to keep unlocked (0 = until manually locked).
    pub timeout_secs: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnlockResponse {
    pub success: bool,
    pub expires_at: Option<u64>,
}

// ─── Staking ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct StakingStatusResponse {
    pub enabled: bool,
    pub staking_address: Option<String>,
    pub eligible_utxos: usize,
    pub total_staking_satoshis: u64,
    pub expected_reward_per_day: f64,
    pub last_stake_time: Option<u32>,
    pub blocks_staked: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StakingStartRequest {
    pub address: String,
    pub passphrase: String,
    pub otp_code: Option<String>,
}

// ─── Torrent ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct TorrentSessionResponse {
    pub id: String,
    pub name: String,
    pub info_hash: String,
    pub state: String,
    pub progress: f64,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub peer_count: usize,
    pub vtr_earned_satoshis: u64,
    pub vtr_paid_satoshis: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AddTorrentRequest {
    /// Either a magnet URI or base64-encoded .torrent file bytes.
    pub source: String,
    /// "magnet" or "file"
    pub source_type: String,
    /// VTR wallet address for incentive payments.
    pub wallet_address: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AddTorrentResponse {
    pub session_id: String,
    pub info_hash: String,
    pub name: String,
}

// ─── DEX / Atomic Swaps ───────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct DexOrderResponse {
    pub id: String,
    pub maker_address: String,
    pub offer_amount_satoshis: u64,
    pub offer_asset: String,
    pub request_amount_satoshis: u64,
    pub request_asset: String,
    pub rate: f64,
    pub status: String,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    pub maker_address: String,
    pub offer_amount_satoshis: u64,
    pub offer_asset: String,
    pub request_amount_satoshis: u64,
    pub request_asset: String,
    /// Expiry in seconds from now.
    pub expiry_secs: u64,
    pub passphrase: String,
    pub otp_code: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaceOrderResponse {
    pub order_id: String,
    pub htlc_address: String,
    pub hash_lock: String,
}

// ─── Legacy Claim ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaimCheckRequest {
    /// Legacy VTR address (starts with V).
    pub legacy_address: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaimCheckResponse {
    pub address: String,
    pub claimable_satoshis: u64,
    pub display: String,
    pub already_claimed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaimSubmitRequest {
    /// WIF-encoded private key for the legacy address.
    pub wif_private_key: String,
    /// New chain address to receive the claimed VTR.
    pub recipient_address: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaimSubmitResponse {
    pub txid: String,
    pub claimed_satoshis: u64,
    pub recipient_address: String,
}

// ─── Generic ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}
