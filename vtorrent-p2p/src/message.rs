/// P2P message types for the vTorrent 2.0 network protocol.
///
/// Message framing: [4-byte magic][4-byte command][4-byte payload_len][4-byte checksum][payload]
/// This is compatible with the Bitcoin P2P message format used in the legacy client.
use serde::{Deserialize, Serialize};

/// Network magic bytes for vTorrent 2.0 mainnet.
pub const NETWORK_MAGIC: [u8; 4] = [0x56, 0x54, 0x52, 0x32]; // "VTR2"

/// Maximum allowed message payload size (4 MB).
///
/// Consensus blocks are capped at 1 MB (`MAX_BLOCK_SIZE`), so 4 MB leaves
/// generous headroom for serialization overhead while bounding the memory a
/// single malicious peer can pin in the node's event queue.
pub const MAX_PAYLOAD_SIZE: u32 = 4 * 1024 * 1024;

/// A P2P network message.
#[derive(Debug, Clone)]
pub struct NetMessage {
    /// The command name (12 bytes, null-padded).
    pub command: [u8; 12],
    /// The message payload.
    pub payload: Vec<u8>,
}

impl NetMessage {
    /// Create a new message with the given command string and payload.
    pub fn new(command: &str, payload: Vec<u8>) -> Self {
        let mut cmd = [0u8; 12];
        let bytes = command.as_bytes();
        let len = bytes.len().min(12);
        cmd[..len].copy_from_slice(&bytes[..len]);
        Self {
            command: cmd,
            payload,
        }
    }

    /// Get the command as a string (trimmed of null bytes).
    pub fn command_str(&self) -> &str {
        let end = self.command.iter().position(|&b| b == 0).unwrap_or(12);
        std::str::from_utf8(&self.command[..end]).unwrap_or("unknown")
    }
}

/// Version handshake message — sent when a new connection is established.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMsg {
    /// Protocol version.
    pub version: u32,
    /// Services bitfield (1 = NODE_NETWORK, 2 = NODE_TORRENT, 4 = NODE_DEX).
    pub services: u64,
    /// Unix timestamp.
    pub timestamp: i64,
    /// Receiving peer address (as string).
    pub addr_recv: String,
    /// Sending peer address.
    pub addr_from: String,
    /// Random nonce to detect self-connections.
    pub nonce: u64,
    /// User agent string.
    pub user_agent: String,
    /// Best block height known to this peer.
    pub start_height: u32,
    /// Whether to relay unconfirmed transactions.
    pub relay: bool,
}

impl VersionMsg {
    pub fn new(best_height: u32, addr_from: &str) -> Self {
        use rand::Rng;
        let nonce: u64 = rand::thread_rng().gen();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        Self {
            version: PROTOCOL_VERSION,
            services: NODE_NETWORK | NODE_TORRENT,
            timestamp,
            addr_recv: String::new(),
            addr_from: addr_from.to_string(),
            nonce,
            user_agent: format!("/vTorrent:{}/", env!("CARGO_PKG_VERSION")),
            start_height: best_height,
            relay: true,
        }
    }
}

/// Service flags.
pub const NODE_NETWORK: u64 = 1;
pub const NODE_TORRENT: u64 = 2;
pub const NODE_DEX: u64 = 4;

/// Current protocol version.
pub const PROTOCOL_VERSION: u32 = 70001;

/// Inventory vector types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum InvType {
    Error = 0,
    Transaction = 1,
    Block = 2,
    FilteredBlock = 3,
    ClaimTransaction = 10, // vTorrent-specific: legacy claim tx
    AtomicSwap = 11,       // vTorrent-specific: DEX atomic swap
}

/// An inventory item (type + 32-byte hash).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvItem {
    pub inv_type: InvType,
    pub hash: [u8; 32],
}

/// Inventory message — announces new transactions or blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvMsg {
    pub items: Vec<InvItem>,
}

/// GetData message — requests specific items by inventory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetDataMsg {
    pub items: Vec<InvItem>,
}

/// GetBlocks message — requests block hashes starting from known locator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBlocksMsg {
    pub version: u32,
    /// Block locator hashes (from most recent to genesis).
    pub block_locator_hashes: Vec<[u8; 32]>,
    /// Stop hash (zero = get as many as possible).
    pub hash_stop: [u8; 32],
}

/// Ping/Pong messages for keepalive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingMsg {
    pub nonce: u64,
}

/// Address message — shares known peer addresses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddrMsg {
    pub addrs: Vec<PeerAddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerAddr {
    pub timestamp: u32,
    pub services: u64,
    pub addr: String,
    pub port: u16,
}

/// Service flag indicating the node supports compact block relay.
pub const NODE_COMPACT_BLOCKS: u64 = 0x08;

/// `sendcmpct` message — sent after version handshake to negotiate compact block relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendCmpctMsg {
    /// 1 = high-bandwidth mode, 0 = low-bandwidth mode.
    pub high_bandwidth: bool,
    /// Protocol version (must be 1).
    pub version: u64,
}

/// `cmpctblock` message — a compact block with short transaction IDs.
///
/// Short IDs are 6-byte SipHash-2-4 hashes of the txid, allowing the receiver
/// to look up transactions in their mempool without downloading them again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmpctBlockMsg {
    /// Block header fields.
    pub version: u32,
    pub prev_block_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub timestamp: u32,
    pub bits: u32,
    pub nonce: u32,
    /// Random nonce used in the SipHash key derivation.
    pub siphash_nonce: u64,
    /// Short transaction IDs (6 bytes each, packed into u64 with upper 2 bytes zero).
    pub short_ids: Vec<u64>,
    /// Prefilled transactions (always includes the coinbase at index 0).
    pub prefilled_txs: Vec<PrefilledTx>,
}

/// A prefilled transaction in a compact block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefilledTx {
    /// Differential index (position in the block's tx list).
    pub index: u16,
    /// The full serialized transaction bytes.
    pub tx_bytes: Vec<u8>,
}

/// `getblocktxn` message — request missing transactions from a compact block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBlockTxnMsg {
    /// Hash of the compact block we need transactions for.
    pub block_hash: [u8; 32],
    /// Sorted list of transaction indices we are missing.
    pub indexes: Vec<u16>,
}

/// `blocktxn` message — the requested transactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockTxnMsg {
    /// Hash of the block these transactions belong to.
    pub block_hash: [u8; 32],
    /// The requested transactions (serialized, in the order requested).
    pub transactions: Vec<Vec<u8>>,
}

/// `feefilter` — peer announces its minimum acceptable fee rate (satoshis per 1000 bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeFilterMsg {
    pub feerate: u64,
}

/// `getheaders` — request a run of block headers starting from a locator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetHeadersMsg {
    pub version: u32,
    /// Block locator hashes (most recent first).
    pub block_locator_hashes: Vec<[u8; 32]>,
    /// Stop hash (all-zeros = send as many as possible, up to 2000).
    pub hash_stop: [u8; 32],
}

/// A single block header entry in a `headers` message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderEntry {
    /// Serialised block header bytes.
    pub header: Vec<u8>,
    /// Transaction count (always 0 in `headers` messages per protocol spec).
    pub tx_count: u32,
}

/// `headers` — response to `getheaders`, up to 2000 block headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadersMsg {
    pub headers: Vec<HeaderEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_net_message_command_str() {
        let msg = NetMessage::new("version", vec![]);
        assert_eq!(msg.command_str(), "version");
    }

    #[test]
    fn test_net_message_command_padding() {
        let msg = NetMessage::new("ping", vec![]);
        assert_eq!(msg.command[4], 0); // null-padded
    }

    #[test]
    fn test_version_msg_creation() {
        let v = VersionMsg::new(100, "127.0.0.1:22526");
        assert_eq!(v.version, PROTOCOL_VERSION);
        assert_eq!(v.start_height, 100);
        assert!(v.services & NODE_NETWORK != 0);
        assert!(v.services & NODE_TORRENT != 0);
    }
}
