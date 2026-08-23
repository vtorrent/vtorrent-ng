/// Atomic Swap / HTLC Module for vTorrent 2.0
///
/// Implements Hash Time-Locked Contracts (HTLCs) for trustless P2P trading.
///
/// An atomic swap between two parties (Alice and Bob) works as follows:
///
/// 1. Alice generates a random secret `s` and computes `h = SHA256(s)`.
/// 2. Alice creates an HTLC on the VTR chain:
///    - Locks her VTR with: "pay to Bob if he reveals `s`, OR refund to Alice after timeout"
/// 3. Bob sees the HTLC on the VTR chain and creates a matching HTLC on the other chain
///    (e.g., BTC, LTC) using the same hash `h`:
///    - Locks his BTC with: "pay to Alice if she reveals `s`, OR refund to Bob after timeout"
/// 4. Alice claims Bob's BTC by revealing `s` — this also reveals `s` to Bob.
/// 5. Bob uses `s` to claim Alice's VTR.
///
/// If either party fails to act, the timelock ensures funds are returned.
///
/// This module implements the VTR side of the swap.
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    block::{Transaction, TxInput, TxOutput, TxType},
    error::{NodeError, Result},
};

/// Default HTLC locktime: 48 hours in seconds.
pub const DEFAULT_HTLC_LOCKTIME: u32 = 48 * 3600;

/// Minimum HTLC locktime: 1 hour.
pub const MIN_HTLC_LOCKTIME: u32 = 3600;

/// Maximum HTLC locktime: 7 days.
pub const MAX_HTLC_LOCKTIME: u32 = 7 * 24 * 3600;

/// An HTLC (Hash Time-Locked Contract) for atomic swaps.
#[derive(Debug, Clone)]
pub struct Htlc {
    /// SHA256 hash of the secret preimage.
    pub hash_lock: [u8; 32],
    /// The recipient address (can claim by revealing the preimage).
    pub recipient: String,
    /// The refund address (can reclaim after timeout).
    pub refund_address: String,
    /// Unix timestamp after which the refund is valid.
    pub expiry: u32,
    /// The amount locked in satoshis.
    pub amount: u64,
    /// The transaction ID of the HTLC funding transaction (set after broadcast).
    pub funding_txid: Option<[u8; 32]>,
}

impl Htlc {
    /// Create a new HTLC.
    pub fn new(
        hash_lock: [u8; 32],
        recipient: String,
        refund_address: String,
        locktime_seconds: u32,
        amount: u64,
    ) -> Result<Self> {
        if locktime_seconds < MIN_HTLC_LOCKTIME {
            return Err(NodeError::AtomicSwap(format!(
                "Locktime {} is below minimum {}",
                locktime_seconds, MIN_HTLC_LOCKTIME
            )));
        }
        if locktime_seconds > MAX_HTLC_LOCKTIME {
            return Err(NodeError::AtomicSwap(format!(
                "Locktime {} exceeds maximum {}",
                locktime_seconds, MAX_HTLC_LOCKTIME
            )));
        }
        if amount == 0 {
            return Err(NodeError::AtomicSwap("HTLC amount cannot be zero".into()));
        }
        // Reject invalid addresses up front: an unparseable address would
        // otherwise silently lock funds to a zero hash160 (unspendable).
        if vtorrent_core::address::Address::parse(&recipient).is_err() {
            return Err(NodeError::AtomicSwap(format!(
                "Invalid recipient address: {}",
                recipient
            )));
        }
        if vtorrent_core::address::Address::parse(&refund_address).is_err() {
            return Err(NodeError::AtomicSwap(format!(
                "Invalid refund address: {}",
                refund_address
            )));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        let expiry = now.checked_add(locktime_seconds).ok_or_else(|| {
            NodeError::AtomicSwap(
                "HTLC expiry overflow: timestamp + locktime exceeds u32::MAX".into(),
            )
        })?;

        Ok(Self {
            hash_lock,
            recipient,
            refund_address,
            expiry,
            amount,
            funding_txid: None,
        })
    }

    /// Create an HTLC with an explicit expiry timestamp (for reconstructing a
    /// previously-funded HTLC so its script matches exactly).
    pub fn with_expiry(
        hash_lock: [u8; 32],
        recipient: String,
        refund_address: String,
        expiry: u32,
        amount: u64,
    ) -> Result<Self> {
        if amount == 0 {
            return Err(NodeError::AtomicSwap("HTLC amount cannot be zero".into()));
        }
        if vtorrent_core::address::Address::parse(&recipient).is_err() {
            return Err(NodeError::AtomicSwap(format!(
                "Invalid recipient address: {}",
                recipient
            )));
        }
        if vtorrent_core::address::Address::parse(&refund_address).is_err() {
            return Err(NodeError::AtomicSwap(format!(
                "Invalid refund address: {}",
                refund_address
            )));
        }
        Ok(Self {
            hash_lock,
            recipient,
            refund_address,
            expiry,
            amount,
            funding_txid: None,
        })
    }

    /// Build the HTLC script (locking script / scriptPubKey).
    ///
    /// Script logic:
    /// ```text
    /// OP_IF
    ///   OP_SHA256 <hash_lock> OP_EQUALVERIFY
    ///   OP_DUP OP_HASH160 <recipient_hash160> OP_EQUALVERIFY OP_CHECKSIG
    /// OP_ELSE
    ///   <expiry> OP_CHECKLOCKTIMEVERIFY OP_DROP
    ///   OP_DUP OP_HASH160 <refund_hash160> OP_EQUALVERIFY OP_CHECKSIG
    /// OP_ENDIF
    /// ```
    pub fn build_script(&self) -> Result<Vec<u8>> {
        let recipient_hash = address_to_hash160(&self.recipient).ok_or_else(|| {
            NodeError::AtomicSwap(format!("invalid recipient address: {}", self.recipient))
        })?;
        let refund_hash = address_to_hash160(&self.refund_address).ok_or_else(|| {
            NodeError::AtomicSwap(format!("invalid refund address: {}", self.refund_address))
        })?;
        let expiry_bytes = self.expiry.to_le_bytes();

        let mut script = Vec::new();

        // OP_IF branch (claim with preimage)
        script.push(0x63); // OP_IF
        script.push(0xa8); // OP_SHA256
        script.push(0x20); // push 32 bytes
        script.extend_from_slice(&self.hash_lock);
        script.push(0x88); // OP_EQUALVERIFY
        script.push(0x76); // OP_DUP
        script.push(0xa9); // OP_HASH160
        script.push(0x14); // push 20 bytes
        script.extend_from_slice(&recipient_hash);
        script.push(0x88); // OP_EQUALVERIFY
        script.push(0xac); // OP_CHECKSIG

        // OP_ELSE branch (refund after timeout)
        script.push(0x67); // OP_ELSE
        script.push(0x04); // push 4 bytes (expiry timestamp)
        script.extend_from_slice(&expiry_bytes);
        script.push(0xb1); // OP_CHECKLOCKTIMEVERIFY
        script.push(0x75); // OP_DROP
        script.push(0x76); // OP_DUP
        script.push(0xa9); // OP_HASH160
        script.push(0x14); // push 20 bytes
        script.extend_from_slice(&refund_hash);
        script.push(0x88); // OP_EQUALVERIFY
        script.push(0xac); // OP_CHECKSIG

        script.push(0x68); // OP_ENDIF

        Ok(script)
    }

    /// Build the funding transaction that locks VTR into the HTLC.
    pub fn build_funding_tx(
        &self,
        input_txid: [u8; 32],
        input_vout: u32,
        input_value: u64,
        fee: u64,
    ) -> Result<Transaction> {
        // Use checked arithmetic so a huge amount/fee cannot overflow and mint
        // a bogus change output.
        let required = self
            .amount
            .checked_add(fee)
            .ok_or_else(|| NodeError::AtomicSwap("amount + fee overflow".into()))?;
        if input_value < required {
            return Err(NodeError::AtomicSwap(format!(
                "Insufficient input: {} < {} + {} (fee)",
                input_value, self.amount, fee
            )));
        }

        let mut outputs = vec![TxOutput {
            value: self.amount,
            script_pubkey: self.build_script()?,
        }];

        // Change output
        let change = input_value - required;
        if change > 0 {
            outputs.push(TxOutput {
                value: change,
                script_pubkey: p2pkh_script(&self.refund_address).ok_or_else(|| {
                    NodeError::AtomicSwap(format!(
                        "invalid refund address: {}",
                        self.refund_address
                    ))
                })?,
            });
        }

        Ok(Transaction {
            version: 1,
            tx_type: TxType::AtomicSwap,
            inputs: vec![TxInput {
                prev_txid: input_txid,
                prev_vout: input_vout,
                script_sig: Vec::new(), // filled in by wallet when signing
                sequence: u32::MAX - 1, // enable CLTV
            }],
            outputs,
            lock_time: 0,
            claim_address: Some(self.recipient.clone()),
            claim_signature: None,
        })
    }

    /// Build the claim transaction (recipient reveals the preimage).
    pub fn build_claim_tx(
        &self,
        funding_txid: [u8; 32],
        preimage: &[u8; 32],
        recipient_pubkey: &[u8],
        recipient_sig: &[u8],
        fee: u64,
    ) -> Result<Transaction> {
        let mut tx = self.build_claim_tx_unsigned(funding_txid, preimage, fee)?;

        // Build the scriptSig for claiming:
        // <sig> <pubkey> <preimage> OP_TRUE (OP_1)
        let mut script_sig = Vec::new();
        script_sig.push(recipient_sig.len() as u8);
        script_sig.extend_from_slice(recipient_sig);
        script_sig.push(recipient_pubkey.len() as u8);
        script_sig.extend_from_slice(recipient_pubkey);
        script_sig.push(0x20); // push 32 bytes
        script_sig.extend_from_slice(preimage);
        script_sig.push(0x51); // OP_1 (true branch)
        tx.inputs[0].script_sig = script_sig;

        Ok(tx)
    }

    /// Build an unsigned claim transaction (empty scriptSig) for signing.
    pub fn build_claim_tx_unsigned(
        &self,
        funding_txid: [u8; 32],
        preimage: &[u8; 32],
        fee: u64,
    ) -> Result<Transaction> {
        // Verify the preimage matches the hash lock
        let hash = sha256(preimage);
        if hash != self.hash_lock {
            return Err(NodeError::AtomicSwap(
                "Preimage does not match hash lock".into(),
            ));
        }

        Ok(Transaction {
            version: 1,
            tx_type: TxType::AtomicSwap,
            inputs: vec![TxInput {
                prev_txid: funding_txid,
                prev_vout: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value: self.amount.saturating_sub(fee),
                script_pubkey: p2pkh_script(&self.recipient).ok_or_else(|| {
                    NodeError::AtomicSwap(format!("invalid recipient address: {}", self.recipient))
                })?,
            }],
            lock_time: 0,
            claim_address: Some(self.recipient.clone()),
            claim_signature: None,
        })
    }

    /// Build the refund transaction (initiator reclaims after timeout).
    pub fn build_refund_tx(
        &self,
        funding_txid: [u8; 32],
        refund_pubkey: &[u8],
        refund_sig: &[u8],
        fee: u64,
    ) -> Result<Transaction> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        if now < self.expiry {
            return Err(NodeError::AtomicSwap(format!(
                "HTLC has not expired yet. Expires at {}, current time {}",
                self.expiry, now
            )));
        }

        let mut tx = self.build_refund_tx_unsigned(funding_txid, fee)?;

        // Build the scriptSig for refunding:
        // <sig> <pubkey> OP_FALSE (OP_0)
        let mut script_sig = Vec::new();
        script_sig.push(refund_sig.len() as u8);
        script_sig.extend_from_slice(refund_sig);
        script_sig.push(refund_pubkey.len() as u8);
        script_sig.extend_from_slice(refund_pubkey);
        script_sig.push(0x00); // OP_0 (false branch)
        tx.inputs[0].script_sig = script_sig;

        Ok(tx)
    }

    /// Build an unsigned refund transaction (empty scriptSig) for signing.
    ///
    /// The expiry check is performed by the caller (which may use a mock clock
    /// in regtest), so this builder does not consult the wall clock.
    pub fn build_refund_tx_unsigned(
        &self,
        funding_txid: [u8; 32],
        fee: u64,
    ) -> Result<Transaction> {
        Ok(Transaction {
            version: 1,
            tx_type: TxType::AtomicSwap,
            inputs: vec![TxInput {
                prev_txid: funding_txid,
                prev_vout: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX - 1,
            }],
            outputs: vec![TxOutput {
                value: self.amount.saturating_sub(fee),
                script_pubkey: p2pkh_script(&self.refund_address).ok_or_else(|| {
                    NodeError::AtomicSwap(format!(
                        "invalid refund address: {}",
                        self.refund_address
                    ))
                })?,
            }],
            lock_time: self.expiry,
            claim_address: Some(self.refund_address.clone()),
            claim_signature: None,
        })
    }

    /// Check if the HTLC has expired.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        now >= self.expiry
    }

    /// Seconds remaining until expiry (0 if already expired).
    pub fn seconds_until_expiry(&self) -> u32 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        self.expiry.saturating_sub(now)
    }
}

/// A swap order posted to the P2P DEX order book.
#[derive(Debug, Clone)]
pub struct SwapOrder {
    /// Unique order ID (SHA256 of the order data).
    pub order_id: [u8; 32],
    /// The maker's VTR address.
    pub maker_address: String,
    /// The maker's BTC address (where they receive BTC when claiming).
    pub maker_btc_address: Option<String>,
    /// Amount of VTR the maker is offering.
    pub vtr_amount: u64,
    /// The target asset (e.g., "BTC", "LTC").
    pub target_asset: String,
    /// Amount of the target asset requested.
    pub target_amount: u64,
    /// The HTLC hash lock (set when the maker creates the HTLC after matching).
    pub hash_lock: Option<[u8; 32]>,
    /// The funding transaction ID once the maker's VTR HTLC is in the mempool.
    pub funding_txid: Option<[u8; 32]>,
    /// The taker's VTR address (the HTLC recipient), set once matched.
    pub taker_address: Option<String>,
    /// Secret preimage retained locally until the swap claim is executed.
    pub preimage: Option<[u8; 32]>,
    /// Order expiry timestamp.
    pub expiry: u32,
    /// Unix timestamp when the order was created.
    pub created_at: u64,
    /// Order status.
    pub status: OrderStatus,
}

/// A public, serializable view of a swap order for P2P gossip.
///
/// Excludes the secret preimage and the private funding txid, which must never
/// leave the maker's node.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrderAnnouncement {
    pub order_id: [u8; 32],
    pub maker_address: String,
    pub maker_btc_address: Option<String>,
    pub vtr_amount: u64,
    pub target_asset: String,
    pub target_amount: u64,
    pub hash_lock: Option<[u8; 32]>,
    pub expiry: u32,
}

impl OrderAnnouncement {
    /// Build a public announcement from a full order.
    pub fn from_order(order: &SwapOrder) -> Self {
        Self {
            order_id: order.order_id,
            maker_address: order.maker_address.clone(),
            maker_btc_address: order.maker_btc_address.clone(),
            vtr_amount: order.vtr_amount,
            target_asset: order.target_asset.clone(),
            target_amount: order.target_amount,
            hash_lock: order.hash_lock,
            expiry: order.expiry,
        }
    }

    /// Reconstruct a `SwapOrder` from an announcement (no preimage/funding).
    pub fn to_order(&self) -> SwapOrder {
        SwapOrder {
            order_id: self.order_id,
            maker_address: self.maker_address.clone(),
            maker_btc_address: self.maker_btc_address.clone(),
            vtr_amount: self.vtr_amount,
            target_asset: self.target_asset.clone(),
            target_amount: self.target_amount,
            hash_lock: self.hash_lock,
            funding_txid: None,
            taker_address: None,
            preimage: None,
            expiry: self.expiry,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            status: OrderStatus::Open,
        }
    }
}

/// Status of a swap order.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderStatus {
    /// Order is open and waiting for a taker.
    Open,
    /// Maker funding transaction is being built and admitted to the mempool.
    Funding,
    /// A taker has been found and the maker HTLC is funded.
    Matched,
    /// The swap is in progress (HTLCs are funded).
    InProgress,
    /// The swap completed successfully.
    Completed,
    /// The swap was cancelled or timed out.
    Cancelled,
}

/// Status of a cross-chain swap across both chains.
#[derive(Debug, Clone, PartialEq)]
pub enum SwapStatus {
    /// The maker's VTR HTLC is being funded.
    Funding,
    /// The maker's VTR HTLC is funded.
    VtrFunded,
    /// The taker's BTC HTLC is funded.
    BtcFunded,
    /// The swap completed (both sides claimed).
    Claimed,
    /// The swap was refunded after expiry.
    Refunded,
}

/// Tracks a swap's lifecycle across the VTR and BTC chains.
#[derive(Debug, Clone)]
pub struct SwapState {
    /// The order this swap belongs to.
    pub order_id: [u8; 32],
    /// The hash lock shared by both HTLCs.
    pub hash_lock: [u8; 32],
    /// The secret preimage (held by the maker until the taker claims VTR).
    pub preimage: Option<[u8; 32]>,
    /// The maker's VTR HTLC funding txid.
    pub vtr_funding_txid: Option<[u8; 32]>,
    /// The taker's BTC HTLC funding txid.
    pub btc_funding_txid: Option<[u8; 32]>,
    /// The maker's BTC address (recipient of the BTC claim).
    pub maker_btc_address: Option<String>,
    /// The taker's BTC refund address.
    pub taker_btc_refund_address: Option<String>,
    /// The BTC amount locked in the HTLC (satoshis).
    pub btc_amount: u64,
    /// The BTC HTLC expiry (unix timestamp).
    pub btc_expiry: u32,
    /// Current status.
    pub status: SwapStatus,
}

impl SwapState {
    pub fn new(order_id: [u8; 32], hash_lock: [u8; 32]) -> Self {
        Self {
            order_id,
            hash_lock,
            preimage: None,
            vtr_funding_txid: None,
            btc_funding_txid: None,
            maker_btc_address: None,
            taker_btc_refund_address: None,
            btc_amount: 0,
            btc_expiry: 0,
            status: SwapStatus::Funding,
        }
    }
}

impl SwapOrder {
    /// Create a new swap order.
    pub fn new(
        maker_address: String,
        vtr_amount: u64,
        target_asset: String,
        target_amount: u64,
        locktime_seconds: u32,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;

        // Compute order ID as SHA256 of the order's identifying fields.
        let mut data = Vec::new();
        data.extend_from_slice(maker_address.as_bytes());
        data.extend_from_slice(&vtr_amount.to_le_bytes());
        data.extend_from_slice(target_asset.as_bytes());
        data.extend_from_slice(&target_amount.to_le_bytes());
        data.extend_from_slice(&now.to_le_bytes());
        let order_id = sha256_bytes(&data);

        Self {
            order_id,
            maker_address,
            maker_btc_address: None,
            vtr_amount,
            target_asset,
            target_amount,
            hash_lock: None,
            funding_txid: None,
            taker_address: None,
            preimage: None,
            expiry: now.saturating_add(locktime_seconds),
            created_at: now as u64,
            status: OrderStatus::Open,
        }
    }

    /// Exchange rate: target_amount / vtr_amount.
    pub fn rate(&self) -> f64 {
        if self.vtr_amount == 0 {
            return 0.0;
        }
        self.target_amount as f64 / self.vtr_amount as f64
    }
}

// ─── Order Book ─────────────────────────────────────────────────────────────

/// Maximum number of orders the book will hold. Oldest cancelled/expired
/// orders are evicted first when this limit is reached.
const MAX_ORDERS: usize = 10_000;

/// An in-memory order book for the P2P DEX.
#[derive(Debug, Default)]

pub struct SwapOrderBook {
    orders: Vec<SwapOrder>,
}

impl SwapOrderBook {
    pub fn new() -> Self {
        SwapOrderBook { orders: Vec::new() }
    }

    /// Add a new order to the book. Evicts expired/cancelled orders if the
    /// book exceeds `MAX_ORDERS`.
    pub fn add_order(&mut self, order: SwapOrder) {
        if self.orders.len() >= MAX_ORDERS {
            self.evict();
        }
        self.orders.push(order);
    }

    /// Evict expired and cancelled orders, keeping the most recently updated.
    fn evict(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        // Remove cancelled orders first, then expired ones
        self.orders.retain(|o| {
            o.status != OrderStatus::Cancelled && (now < o.expiry || o.status != OrderStatus::Open)
        });
        // If still over limit, remove oldest open orders by expiry
        if self.orders.len() >= MAX_ORDERS {
            self.orders.sort_by_key(|o| std::cmp::Reverse(o.expiry));
            self.orders.truncate(MAX_ORDERS * 3 / 4);
        }
    }

    /// List all open orders.
    pub fn list_open_orders(&self) -> Vec<&SwapOrder> {
        self.orders
            .iter()
            .filter(|o| o.status == OrderStatus::Open)
            .collect()
    }

    /// Cancel an open order by hex-encoded order_id. Funded or in-progress
    /// swaps must be settled through their HTLC path rather than cancelled.
    pub fn cancel_order(&mut self, id: &str) -> bool {
        for order in self.orders.iter_mut() {
            if hex::encode(order.order_id) == id && order.status == OrderStatus::Open {
                order.status = OrderStatus::Cancelled;
                return true;
            }
        }
        false
    }

    /// Get an order by hex-encoded order_id.
    pub fn get_order(&self, id: &str) -> Option<&SwapOrder> {
        self.orders.iter().find(|o| hex::encode(o.order_id) == id)
    }

    /// Update the status of an order by hex-encoded order_id.
    pub fn update_order_status(&mut self, id: &str, status: OrderStatus) -> bool {
        for order in self.orders.iter_mut() {
            if hex::encode(order.order_id) == id {
                order.status = status;
                return true;
            }
        }
        false
    }

    /// Set the hash_lock on a matched order (called after the maker creates the HTLC).
    pub fn set_hash_lock(&mut self, id: &str, hash_lock: [u8; 32]) -> bool {
        for order in self.orders.iter_mut() {
            if hex::encode(order.order_id) == id {
                order.hash_lock = Some(hash_lock);
                return true;
            }
        }
        false
    }

    /// Set the maker's BTC address on an order (where they receive BTC on claim).
    pub fn set_maker_btc_address(&mut self, id: &str, btc_address: String) -> bool {
        for order in self.orders.iter_mut() {
            if hex::encode(order.order_id) == id {
                order.maker_btc_address = Some(btc_address);
                return true;
            }
        }
        false
    }

    /// Expire all orders whose expiry timestamp has passed.
    /// Returns the number of orders expired.
    pub fn expire_orders(&mut self) -> usize {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        let mut count = 0;
        for order in self.orders.iter_mut() {
            if order.status == OrderStatus::Open && now >= order.expiry {
                order.status = OrderStatus::Cancelled;
                count += 1;
            }
        }
        count
    }

    /// Reserve an open order while the maker's HTLC funding transaction is built.
    pub fn begin_funding(&mut self, order_id: &str) -> Option<SwapOrder> {
        let order = self
            .orders
            .iter_mut()
            .find(|o| hex::encode(o.order_id) == order_id && o.status == OrderStatus::Open)?;
        order.status = OrderStatus::Funding;
        Some(order.clone())
    }

    /// Return a funding reservation to the open order book after a local failure.
    pub fn release_funding(&mut self, order_id: &str) -> bool {
        let Some(order) = self
            .orders
            .iter_mut()
            .find(|o| hex::encode(o.order_id) == order_id)
        else {
            return false;
        };
        if order.status != OrderStatus::Funding {
            return false;
        }
        order.status = OrderStatus::Open;
        true
    }

    /// Mark a reserved order as funded and matched after its HTLC funding
    /// transaction has been accepted into the local mempool.
    pub fn fund_and_match_order(
        &mut self,
        order_id: &str,
        taker_address: String,
        preimage: [u8; 32],
        hash_lock: [u8; 32],
        funding_txid: [u8; 32],
    ) -> Option<MatchResult> {
        let order = self
            .orders
            .iter_mut()
            .find(|o| hex::encode(o.order_id) == order_id && o.status == OrderStatus::Funding)?;

        order.status = OrderStatus::Matched;
        order.hash_lock = Some(hash_lock);
        order.preimage = Some(preimage);
        order.funding_txid = Some(funding_txid);
        order.taker_address = Some(taker_address.clone());
        let matched_order = order.clone();

        Some(MatchResult {
            order: matched_order,
            preimage,
            hash_lock,
            taker_address,
        })
    }

    /// Legacy in-memory matcher retained for callers without a funding wallet.
    /// Production RPC flow uses `fund_and_match_order` after on-chain funding.
    pub fn match_order(&mut self, order_id: &str, taker_address: String) -> Option<MatchResult> {
        let swap = AtomicSwap::new();
        self.begin_funding(order_id)?;
        self.fund_and_match_order(
            order_id,
            taker_address,
            swap.preimage,
            swap.hash_lock,
            [0u8; 32],
        )
    }

    /// List all orders for a specific maker address.
    pub fn orders_by_maker(&self, maker_address: &str) -> Vec<&SwapOrder> {
        self.orders
            .iter()
            .filter(|o| o.maker_address == maker_address)
            .collect()
    }

    /// Count open orders.
    pub fn open_order_count(&self) -> usize {
        self.orders
            .iter()
            .filter(|o| o.status == OrderStatus::Open)
            .count()
    }
}

/// Result of a successful order match.
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// The matched order (clone with status updated to Matched).
    pub order: SwapOrder,
    /// The preimage the taker must keep secret until they fund their side.
    pub preimage: [u8; 32],
    /// The hash lock the maker will use in the HTLC script.
    pub hash_lock: [u8; 32],
    /// The taker's address (for the maker to send to).
    pub taker_address: String,
}

/// Convenience wrapper for creating a new atomic swap (generates a random preimage).
pub struct AtomicSwap {
    pub preimage: [u8; 32],
    pub hash_lock: [u8; 32],
}

impl Default for AtomicSwap {
    fn default() -> Self {
        Self::new()
    }
}

impl AtomicSwap {
    pub fn new() -> Self {
        use rand::RngCore;
        let mut preimage = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut preimage);
        let hash_lock = {
            let mut hasher = Sha256::new();
            hasher.update(preimage);
            let result = hasher.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&result);
            out
        };
        AtomicSwap {
            preimage,
            hash_lock,
        }
    }
}

// ─── Helper functions ────────────────────────────────────────────────────────

/// SHA256 of a 32-byte input.
fn sha256(data: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// SHA256 of an arbitrary-length input.
fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Decode a Base58Check address to its 20-byte hash160 payload.
/// Returns `None` if the address is malformed.
fn address_to_hash160(address: &str) -> Option<[u8; 20]> {
    vtorrent_core::address::Address::parse(address)
        .ok()
        .map(|addr| addr.hash)
}

/// Build a standard P2PKH scriptPubKey from an address.
/// Returns `None` if the address is invalid.
fn p2pkh_script(address: &str) -> Option<Vec<u8>> {
    let hash160 = address_to_hash160(address)?;
    let mut script = Vec::with_capacity(25);
    script.push(0x76); // OP_DUP
    script.push(0xa9); // OP_HASH160
    script.push(0x14); // push 20 bytes
    script.extend_from_slice(&hash160);
    script.push(0x88); // OP_EQUALVERIFY
    script.push(0xac); // OP_CHECKSIG
    Some(script)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_htlc() -> Htlc {
        let preimage = [42u8; 32];
        let hash_lock = sha256(&preimage);
        Htlc::new(
            hash_lock,
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            "VU3QSqAqM7tP3QXZ8sT7v8sQSdAxUZvqdS".to_string(),
            DEFAULT_HTLC_LOCKTIME,
            100_000_000, // 1 VTR
        )
        .unwrap()
    }

    #[test]
    fn test_htlc_creation() {
        let htlc = make_htlc();
        assert_eq!(htlc.amount, 100_000_000);
        assert!(!htlc.is_expired());
        assert!(htlc.seconds_until_expiry() > 0);
    }

    #[test]
    fn test_htlc_script_not_empty() {
        let htlc = make_htlc();
        let script = htlc.build_script().unwrap();
        assert!(!script.is_empty());
        assert_eq!(script[0], 0x63); // OP_IF
        assert_eq!(*script.last().unwrap(), 0x68); // OP_ENDIF
    }

    #[test]
    fn test_htlc_script_contains_hash_lock() {
        let preimage = [42u8; 32];
        let hash_lock = sha256(&preimage);
        let htlc = Htlc::new(
            hash_lock,
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            "VU3QSqAqM7tP3QXZ8sT7v8sQSdAxUZvqdS".to_string(),
            DEFAULT_HTLC_LOCKTIME,
            100_000_000,
        )
        .unwrap();
        let script = htlc.build_script().unwrap();
        // hash_lock should appear in the script
        let script_hex = hex::encode(&script);
        let hash_hex = hex::encode(hash_lock);
        assert!(script_hex.contains(&hash_hex));
    }

    #[test]
    fn test_htlc_wrong_preimage_rejected() {
        let htlc = make_htlc();
        let wrong_preimage = [99u8; 32];
        let result = htlc.build_claim_tx([0u8; 32], &wrong_preimage, &[0u8; 33], &[0u8; 71], 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_htlc_refund_before_expiry_rejected() {
        let htlc = make_htlc();
        let result = htlc.build_refund_tx([0u8; 32], &[0u8; 33], &[0u8; 71], 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_htlc_funding_tx_insufficient_input() {
        let htlc = make_htlc();
        let result = htlc.build_funding_tx([0u8; 32], 0, 50_000, 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_htlc_funding_tx_valid() {
        let htlc = make_htlc();
        let result = htlc.build_funding_tx([1u8; 32], 0, 200_000_000, 10_000);
        assert!(result.is_ok());
        let tx = result.unwrap();
        assert_eq!(tx.tx_type, TxType::AtomicSwap);
        assert_eq!(tx.outputs[0].value, 100_000_000);
        assert_eq!(tx.outputs[1].value, 200_000_000 - 100_000_000 - 10_000);
    }

    #[test]
    fn test_swap_order_creation() {
        let order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000_000, // 10 VTR
            "BTC".to_string(),
            100_000, // 0.001 BTC
            DEFAULT_HTLC_LOCKTIME,
        );
        assert_eq!(order.status, OrderStatus::Open);
        assert!(order.rate() > 0.0);
        assert!(!order.is_expired());
    }

    #[test]
    fn test_funding_reservation_records_htlc_transaction() {
        let mut book = SwapOrderBook::new();
        let order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000,
            "BTC".to_string(),
            100_000,
            DEFAULT_HTLC_LOCKTIME,
        );
        let order_id = hex::encode(order.order_id);
        book.add_order(order);

        assert!(book.begin_funding(&order_id).is_some());
        assert!(!book.cancel_order(&order_id));
        let matched = book
            .fund_and_match_order(
                &order_id,
                "VU3QSqAqM7tP3QXZ8sT7v8sQSdAxUZvqdS".to_string(),
                [7u8; 32],
                [8u8; 32],
                [9u8; 32],
            )
            .expect("funding reservation should complete");
        assert_eq!(matched.order.status, OrderStatus::Matched);
        assert_eq!(matched.order.funding_txid, Some([9u8; 32]));
        assert_eq!(matched.order.hash_lock, Some([8u8; 32]));
    }

    #[test]
    fn test_swap_order_rate() {
        let order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000_000, // 10 VTR
            "BTC".to_string(),
            1_000_000, // 0.01 BTC
            DEFAULT_HTLC_LOCKTIME,
        );
        // rate = 1_000_000 / 1_000_000_000 = 0.001
        assert!((order.rate() - 0.001).abs() < 1e-9);
    }

    #[test]
    fn test_swap_state_transitions() {
        let mut state = SwapState::new([1u8; 32], [2u8; 32]);
        assert_eq!(state.status, SwapStatus::Funding);

        state.vtr_funding_txid = Some([3u8; 32]);
        state.status = SwapStatus::VtrFunded;
        assert_eq!(state.status, SwapStatus::VtrFunded);

        state.btc_funding_txid = Some([4u8; 32]);
        state.status = SwapStatus::BtcFunded;
        assert_eq!(state.status, SwapStatus::BtcFunded);

        state.status = SwapStatus::Claimed;
        assert_eq!(state.status, SwapStatus::Claimed);
    }

    #[test]
    fn test_order_announcement_roundtrip() {
        let order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000_000,
            "BTC".to_string(),
            100_000,
            DEFAULT_HTLC_LOCKTIME,
        );
        let ann = OrderAnnouncement::from_order(&order);
        let json = serde_json::to_string(&ann).unwrap();
        let back: OrderAnnouncement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.order_id, ann.order_id);
        assert_eq!(back.maker_address, ann.maker_address);
        assert_eq!(back.vtr_amount, ann.vtr_amount);
        assert_eq!(back.target_asset, ann.target_asset);
        assert_eq!(back.target_amount, ann.target_amount);
        assert_eq!(back.expiry, ann.expiry);
    }

    #[test]
    fn test_order_announcement_excludes_preimage() {
        let mut order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000_000,
            "BTC".to_string(),
            100_000,
            DEFAULT_HTLC_LOCKTIME,
        );
        order.preimage = Some([7u8; 32]);
        order.funding_txid = Some([9u8; 32]);
        let ann = OrderAnnouncement::from_order(&order);
        let json = serde_json::to_string(&ann).unwrap();
        assert!(!json.contains("preimage"));
        assert!(!json.contains("funding_txid"));
    }

    impl SwapOrder {
        fn is_expired(&self) -> bool {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u32;
            now >= self.expiry
        }
    }
}
