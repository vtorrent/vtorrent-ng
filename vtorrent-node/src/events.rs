/// Lightweight node event types emitted by the P2P node.
///
/// These are broadcast over a `tokio::sync::broadcast` channel so that any
/// subscriber (e.g. the RPC WebSocket layer) can react to node events without
/// a circular crate dependency.
///
/// `vtorrent-node` defines the events; `vtorrent-rpc` subscribes to them and
/// re-broadcasts them to WebSocket clients.  The daemon also uses `NewBlock`
/// to persist blocks via `vtorrent-store`.
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::block::Block;
use crate::chain::Utxo;

/// An event emitted by the node.
#[derive(Debug, Clone)]
pub enum NodeEvent {
    /// A new block was accepted onto the main chain.
    NewBlock {
        height: u32,
        hash: [u8; 32],
        tx_count: usize,
        timestamp: u32,
        size_bytes: usize,
        /// Full block (Arc-wrapped so cloning is cheap).
        block: Arc<Block>,
        /// UTXOs created by this block (for BlockStore persistence).
        utxos_added: Vec<Utxo>,
        /// UTXOs spent by this block: (txid, vout) pairs.
        utxos_removed: Vec<([u8; 32], u32)>,
        /// Legacy addresses claimed by this block.
        claimed_addresses: Vec<String>,
    },
    /// A transaction was confirmed in a block.
    TxConfirmed {
        txid: [u8; 32],
        block_height: u32,
        block_hash: [u8; 32],
    },
    /// A new unconfirmed transaction entered the mempool.
    TxUnconfirmed {
        txid: [u8; 32],
        fee_sats: u64,
        size_bytes: usize,
    },
    /// A peer completed the handshake and is now connected.
    PeerConnected {
        addr: std::net::SocketAddr,
        user_agent: String,
        version: u32,
        height: u32,
    },
    /// A peer disconnected.
    PeerDisconnected { addr: std::net::SocketAddr },
    /// A chain reorganisation occurred.
    Reorg {
        old_tip: [u8; 32],
        new_tip: [u8; 32],
        depth: u32,
        /// Abandoned main-chain blocks, tip first, with disk-undo data.
        rolled_back_blocks: Vec<crate::chain::RolledBackBlock>,
        /// Fork blocks now canonical, ascending, with disk data.
        applied_fork_blocks: Vec<crate::chain::AppliedForkBlock>,
    },
    /// A staking reward was earned.
    StakingReward {
        block_height: u32,
        reward_sats: u64,
        address: String,
    },
}

/// Type alias for the broadcast sender used by the node.
pub type EventSender = broadcast::Sender<Arc<NodeEvent>>;

/// Create a new event channel with the given capacity.
pub fn channel(capacity: usize) -> (EventSender, broadcast::Receiver<Arc<NodeEvent>>) {
    broadcast::channel(capacity)
}
