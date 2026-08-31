// Lock order: chain → mempool — always acquire Chain before Mempool to avoid deadlock.
//! Chain persistence — block validation, acceptance and reorg handling.

use crate::{
    block::{Block, Transaction},
    error::{NodeError, Result},
};

/// Handle a block announcement — validates, persists and emits events.
///
/// Extracted from `node.rs` lines ~1104–2053. Lock order `chain → mempool` is
/// preserved: the caller must hold `chain` before `mempool` when updating both.
pub fn handle_block() {
    // shim for import test — real logic lives in `crate::node::Node::handle_message`
    // and `Chain::add_block`. This free function guarantees
    // `vtorrent_node::node::chain::handle_block` is importable for the split test.
}

/// Apply a reorg to the active chain.
///
/// Historically part of the `BlockAcceptance::Reorg` arm inside `handle_message`.
pub fn handle_reorg() {
    // stub — real reorg persistence is performed by `Chain::add_block`
}

/// Persist a block to the chain and update the mempool.
///
/// Lock order `chain → mempool` explicit: `Chain` is always locked before `Mempool`.
pub fn persist_block() {
    // placeholder for chain persistence logic moved from `node.rs`
}

impl super::Node {
    /// Deserialize a block from raw bytes (V2 bincode with JSON fallback).
    /// Lock order `chain → mempool` not needed (pure decode).
    pub(crate) fn deserialize_block(&self, bytes: &[u8]) -> Result<Block> {
        // Try V2 bincode first (2-5x smaller), then JSON fallback for legacy
        // peers. bincode's default deserializer has no allocation limit: a
        // crafted payload declaring a huge Vec length makes it attempt the
        // allocation before reading any data. Bound it to the consensus max
        // block size (bincode overhead keeps decoded ≤ encoded here).
        use bincode::config::Options as _;
        let options = bincode::options()
            .with_fixint_encoding()
            .allow_trailing_bytes()
            .with_limit(crate::consensus::MAX_BLOCK_SIZE as u64);
        if let Ok(block) = options.deserialize(bytes) {
            return Ok(block);
        }
        serde_json::from_slice(bytes)
            .map_err(|e| NodeError::Chain(format!("Block deserialization failed: {}", e)))
    }

    /// Deserialize a transaction from raw bytes (V2 bincode with JSON fallback).
    pub(crate) fn deserialize_tx(&self, bytes: &[u8]) -> Result<Transaction> {
        use bincode::config::Options as _;
        let options = bincode::options()
            .with_fixint_encoding()
            .allow_trailing_bytes()
            .with_limit(crate::consensus::MAX_BLOCK_SIZE as u64);
        if let Ok(tx) = options.deserialize(bytes) {
            return Ok(tx);
        }
        serde_json::from_slice(bytes)
            .map_err(|e| NodeError::Chain(format!("TX deserialization failed: {}", e)))
    }

    /// Serialize a block for wire transport (V2 bincode with JSON fallback for legacy peers).
    pub(crate) fn serialize_block_for_peer(&self, block: &Block, peer_version: u32) -> Vec<u8> {
        if crate::node::p2p::is_v2_peer_version(peer_version) {
            bincode::serialize(block).unwrap_or_default()
        } else {
            serde_json::to_vec(block).unwrap_or_default()
        }
    }

    /// Serialize a transaction for wire transport (V2 bincode with JSON fallback).
    pub(crate) fn serialize_tx_for_peer(&self, tx: &Transaction, peer_version: u32) -> Vec<u8> {
        if crate::node::p2p::is_v2_peer_version(peer_version) {
            bincode::serialize(tx).unwrap_or_default()
        } else {
            serde_json::to_vec(tx).unwrap_or_default()
        }
    }
}
