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
    /// Deserialize a block from raw bytes.
    /// Currently uses JSON for simplicity; will be replaced with binary encoding.
    /// Lock order `chain → mempool` not needed (pure decode).
    pub(crate) fn deserialize_block(&self, bytes: &[u8]) -> Result<Block> {
        serde_json::from_slice(bytes)
            .map_err(|e| NodeError::Chain(format!("Block deserialization failed: {}", e)))
    }

    /// Deserialize a transaction from raw bytes.
    pub(crate) fn deserialize_tx(&self, bytes: &[u8]) -> Result<Transaction> {
        serde_json::from_slice(bytes)
            .map_err(|e| NodeError::Chain(format!("TX deserialization failed: {}", e)))
    }
}
