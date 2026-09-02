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
    /// Sync progress as a percentage (0.0–100.0). 100.0 means fully synced.
    pub sync_percent: f64,
    /// Number of transactions currently in the mempool.
    pub mempool_size: usize,
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

/// Detailed transaction data for `GET /api/v1/blockchain/tx/:txid`.
#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionLookupResponse {
    pub txid: String,
    /// `None` when the transaction is still in the mempool.
    pub block_hash: Option<String>,
    /// `None` when the transaction is still in the mempool.
    pub block_height: Option<u32>,
    pub version: u32,
    pub tx_type: String,
    pub inputs: Vec<TransactionInputResponse>,
    pub outputs: Vec<TransactionOutputResponse>,
    pub lock_time: u32,
    pub claim_address: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionInputResponse {
    pub prev_txid: String,
    pub prev_vout: u32,
    pub script_sig: String,
    pub sequence: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionOutputResponse {
    pub value_satoshis: u64,
    pub script_pubkey: String,
}

/// Request for `POST /api/v1/blockchain/broadcast`.
/// `raw_tx` is the hexadecimal bincode serialization of a vTorrent transaction.
#[derive(Debug, Serialize, Deserialize)]
pub struct BroadcastTransactionRequest {
    pub raw_tx: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BroadcastTransactionResponse {
    pub txid: String,
    pub accepted: bool,
    /// True when a live node accepted the transaction for P2P relay.
    pub relayed: bool,
}

#[derive(Debug, Deserialize)]
pub struct WalletUtxosQuery {
    /// Optional public address to inspect; defaults to the imported hot-wallet address.
    pub address: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletUtxoResponse {
    pub txid: String,
    pub vout: u32,
    pub value_satoshis: u64,
    pub script_pubkey: String,
    pub block_height: u32,
    pub block_timestamp: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletUtxosResponse {
    pub address: String,
    pub total_satoshis: u64,
    pub utxos: Vec<WalletUtxoResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeeEstimateResponse {
    pub recommended_sat_per_byte: u64,
    pub minimum_sat_per_byte: u64,
    pub median_sat_per_byte: u64,
    pub mempool_transactions: usize,
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
    /// Expected staking reward per day, in satoshis (divide by COIN for VTR).
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
    /// HTLC funding transaction ID once the maker has funded the VTR side.
    pub funding_txid: Option<String>,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    pub maker_address: String,
    /// The maker's BTC address (where they receive BTC when claiming).
    pub maker_btc_address: Option<String>,
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
    /// The VTR HTLC funding address. `None` until a taker matches the order and
    /// supplies the recipient address (the HTLC redeem script needs it).
    pub htlc_address: Option<String>,
    pub hash_lock: String,
    pub funding_txid: Option<String>,
    pub status: String,
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

// ─── Wallet Import ───────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/wallet/import`.
/// Imports a WIF-encoded private key into the hot wallet for signing.
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportWalletRequest {
    /// WIF-encoded private key.
    pub wif: String,
    /// Passphrase used to encrypt the imported key. Required to unlock the
    /// wallet and to sign transactions afterwards.
    pub passphrase: String,
    /// Optional Base32-encoded TOTP secret. When set, unlock and send require
    /// a valid TOTP code in addition to the passphrase.
    pub otp_secret: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImportWalletResponse {
    /// The derived vTorrent address for this key.
    pub address: String,
    pub success: bool,
}

// ─── DEX Matching ───────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/dex/match`.
#[derive(Debug, Serialize, Deserialize)]
pub struct MatchOrderRequest {
    /// Hex-encoded order ID to match.
    pub order_id: String,
    /// The taker's VTR address.
    pub taker_address: String,
    /// The wallet passphrase (re-verified before signing).
    pub passphrase: String,
    /// The 6-digit TOTP code (required when 2FA is enabled).
    pub otp_code: Option<String>,
}

/// Response for a successful order match.
#[derive(Debug, Serialize, Deserialize)]
pub struct MatchOrderResponse {
    /// Hex-encoded order ID.
    pub order_id: String,
    /// The maker's VTR address.
    pub maker_address: String,
    /// VTR amount being swapped.
    pub vtr_amount: u64,
    /// Target asset (e.g. "BTC").
    pub target_asset: String,
    /// Target asset amount.
    pub target_amount: u64,
    /// Hex-encoded hash lock for the HTLC.
    pub hash_lock: String,
    /// Order expiry timestamp.
    pub expiry: u32,
    /// vTorrent HTLC funding transaction ID accepted into the local mempool.
    pub funding_txid: String,
}

// ─── SPV ─────────────────────────────────────────────────────────────────────

/// Response for `GET /api/v1/spv/status`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SpvStatusResponse {
    /// Number of headers stored in the SPV chain.
    pub header_count: usize,
    /// Best chain height (0 if no headers).
    pub best_height: u32,
    /// Hex-encoded best chain tip hash (empty string if no headers).
    pub best_hash: String,
}

/// A single block header submitted to the SPV chain.
#[derive(Debug, Serialize, Deserialize)]
pub struct SpvHeaderInput {
    pub version: u32,
    /// Hex-encoded previous block hash (64 hex chars).
    pub prev_hash: String,
    /// Hex-encoded Merkle root (64 hex chars).
    pub merkle_root: String,
    #[serde(default)]
    pub utxo_root: Option<String>,
    pub timestamp: u32,
    pub bits: u32,
    pub nonce: u32,
    #[serde(default)]
    pub stake_modifier: Option<u64>,
    pub height: u32,
}

/// Request body for `POST /api/v1/spv/headers`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SpvAddHeadersRequest {
    pub headers: Vec<SpvHeaderInput>,
}

/// Response for `POST /api/v1/spv/headers`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SpvAddHeadersResponse {
    /// Number of headers successfully added.
    pub added: usize,
    /// New best height after adding headers.
    pub best_height: u32,
    /// Hex-encoded new best hash.
    pub best_hash: String,
}

/// Response for `GET /api/v1/spv/proof/:hash` — PoS StakeProof (if available).
#[derive(Debug, Serialize, Deserialize)]
pub struct SpvProofResponse {
    pub block_hash: String,
    pub proof: Option<serde_json::Value>,
}

// ─── Generic ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

// ─── Peers ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct PeerInfoResponse {
    pub addr: String,
    pub user_agent: String,
    pub services: u64,
    pub best_height: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeersResponse {
    pub count: usize,
    pub peers: Vec<PeerInfoResponse>,
}

// ─── Bitcoin wallet ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct BtcStatusResponse {
    pub initialized: bool,
    pub balance_satoshis: u64,
    pub address: Option<String>,
    pub best_height: u32,
    pub synced: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BtcSendRequest {
    pub to_address: String,
    pub amount_satoshis: u64,
    pub fee_satoshis: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BtcSendResponse {
    pub txid: String,
    pub raw_tx: String,
}

// ─── Swap Orchestration ───────────────────────────────────────────────────────

/// Request body for `POST /api/v1/swap/btc-fund`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BtcFundRequest {
    /// Hex-encoded order ID.
    pub order_id: String,
    /// The taker's BTC refund address.
    pub btc_refund_address: String,
}

/// Response for a successful BTC funding.
#[derive(Debug, Serialize, Deserialize)]
pub struct BtcFundResponse {
    pub order_id: String,
    pub btc_funding_txid: String,
    pub status: String,
}

/// Request body for `POST /api/v1/swap/vtr-claim`.
#[derive(Debug, Serialize, Deserialize)]
pub struct VtrClaimRequest {
    /// Hex-encoded order ID.
    pub order_id: String,
    /// The secret preimage (revealed by the taker).
    pub preimage: String,
    /// The taker's WIF private key, used to sign the claim transaction.
    pub taker_wif: String,
}

/// Request body for `POST /api/v1/swap/btc-claim`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BtcClaimRequest {
    /// Hex-encoded order ID.
    pub order_id: String,
}

/// Request body for `POST /api/v1/swap/refund`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SwapRefundRequest {
    /// Hex-encoded order ID.
    pub order_id: String,
}

/// Generic swap action response.
#[derive(Debug, Serialize, Deserialize)]
pub struct SwapActionResponse {
    pub order_id: String,
    pub txid: String,
    pub status: String,
}

// ─── GetTxOut ─────────────────────────────────────────────────────────────────

/// Response for `GET /api/v1/blockchain/utxo/:txid/:vout`.
#[derive(Debug, Serialize, Deserialize)]
pub struct GetTxOutResponse {
    pub txid: String,
    pub vout: u32,
    pub value_satoshis: u64,
    pub script_pubkey: String,
    pub height: u32,
    pub coinbase: bool,
}

// ─── Regtest Faucet ───────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/faucet` (regtest only).
#[derive(Debug, Serialize, Deserialize)]
pub struct FaucetRequest {
    /// The vTorrent address to credit.
    pub address: String,
    /// Amount in satoshis (defaults to 100 VTR if omitted).
    pub amount_satoshis: Option<u64>,
}

/// Response for `POST /api/v1/faucet`.
#[derive(Debug, Serialize, Deserialize)]
pub struct FaucetResponse {
    pub address: String,
    pub amount_satoshis: u64,
    pub txid: String,
    pub block_height: u64,
}
