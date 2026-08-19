//! Live Bitcoin header sync and Bloom-filter UTXO scan.

use crate::error::{BtcError, Result};
use crate::headers::HeaderChain;
use crate::p2p::BtcPeer;
use crate::utxo::{Utxo, UtxoSet};
use bitcoin::consensus::encode::serialize;
use bitcoin::hashes::Hash;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::{GetHeadersMessage, Inventory};
use bitcoin::p2p::message_bloom::FilterLoad;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use vtorrent_spv::BloomFilter;

/// Bitcoin DNS seeds (mainnet).
pub const DNS_SEEDS: &[&str] = &[
    "seed.bitcoin.sipa.be",
    "dnsseed.bluematt.me",
    "dnsseed.bitcoin.dashjr.org",
];

/// Resolve DNS seeds to socket addresses.
pub async fn resolve_seeds() -> Result<Vec<SocketAddr>> {
    let mut addrs = Vec::new();
    for seed in DNS_SEEDS {
        match tokio::net::lookup_host((*seed, 8333)).await {
            Ok(iter) => addrs.extend(iter),
            Err(e) => tracing::warn!("DNS seed {} failed: {}", seed, e),
        }
    }
    if addrs.is_empty() {
        return Err(BtcError::Dns("no DNS seeds resolved".into()));
    }
    Ok(addrs)
}

/// Broadcast a raw transaction to the Bitcoin network via the first reachable
/// seed peer. Returns the txid on success.
pub async fn broadcast_tx(raw: &[u8]) -> Result<[u8; 32]> {
    let tx: bitcoin::Transaction = bitcoin::consensus::encode::deserialize(raw)
        .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let txid = tx.compute_txid().to_byte_array();
    let addrs = resolve_seeds().await?;
    let mut last_err = None;
    for addr in addrs {
        match crate::p2p::BtcPeer::connect(addr).await {
            Ok(mut peer) => match peer.broadcast_tx(&tx).await {
                Ok(()) => return Ok(txid),
                Err(e) => last_err = Some(e),
            },
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| BtcError::P2p("no reachable peers".into())))
}

/// A Bitcoin SPV sync engine.
pub struct BtcSync {
    headers: Arc<Mutex<HeaderChain>>,
    utxos: Arc<Mutex<UtxoSet>>,
    addresses: Vec<String>,
}

impl BtcSync {
    pub fn new(
        headers: Arc<Mutex<HeaderChain>>,
        utxos: Arc<Mutex<UtxoSet>>,
        addresses: Vec<String>,
    ) -> Self {
        Self {
            headers,
            utxos,
            addresses,
        }
    }

    /// Build a BIP37 Bloom filter from the wallet's addresses.
    ///
    /// BIP37 matches against the serialized scriptPubKey of each output, so
    /// the filter must contain the actual script bytes (e.g. `OP_0 <20-byte
    /// pubkey hash>` for P2WPKH), not the bech32 string.
    pub fn build_filter(&self) -> BloomFilter {
        let mut filter = BloomFilter::new(self.addresses.len().max(1), 0.001, 0);
        for addr in &self.addresses {
            if let Ok(a) = bitcoin::Address::from_str(addr) {
                if let Ok(a) = a.require_network(bitcoin::Network::Bitcoin) {
                    filter.insert_script(&a.script_pubkey().to_bytes());
                }
            }
        }
        filter
    }

    /// Build a `getheaders` message from the current tip.
    pub fn build_getheaders(&self) -> GetHeadersMessage {
        let headers = self.headers.lock().unwrap();
        let locator = if let Some(best) = headers.best_hash() {
            vec![bitcoin::BlockHash::from_byte_array(best)]
        } else {
            vec![]
        };
        GetHeadersMessage {
            version: 70016,
            locator_hashes: locator,
            stop_hash: bitcoin::BlockHash::all_zeros(),
        }
    }

    /// Build a `filterload` message from the wallet's addresses.
    pub fn build_filterload(&self) -> FilterLoad {
        let filter = self.build_filter();
        let (data, hash_funcs, tweak, _flags) = filter.to_wire();
        FilterLoad {
            filter: data,
            hash_funcs,
            tweak,
            flags: bitcoin::p2p::message_bloom::BloomFlags::All,
        }
    }

    /// Run one sync pass against a single peer.
    pub async fn sync_once(&self, peer: &mut BtcPeer) -> Result<usize> {
        peer.send(NetworkMessage::FilterLoad(self.build_filterload()))
            .await?;
        peer.send(NetworkMessage::GetHeaders(self.build_getheaders()))
            .await?;

        let mut added = 0usize;
        loop {
            match peer.recv().await? {
                NetworkMessage::Headers(hdrs) => {
                    for h in hdrs {
                        let raw = serialize(&h);
                        let height = {
                            let chain = self.headers.lock().unwrap();
                            chain.best_height() + 1
                        };
                        self.headers.lock().unwrap().add_header(&raw, height)?;
                        added += 1;
                    }
                    break;
                }
                NetworkMessage::Verack | NetworkMessage::Version(_) => continue,
                _ => continue,
            }
        }
        Ok(added)
    }

    /// Extract matched txids from a merkleblock, verifying the merkle root.
    pub fn extract_matched_txids(
        &self,
        block: &bitcoin::merkle_tree::MerkleBlock,
    ) -> Result<Vec<bitcoin::Txid>> {
        let mut matches = Vec::new();
        let mut indexes = Vec::new();
        block
            .extract_matches(&mut matches, &mut indexes)
            .map_err(|e| BtcError::Sync(e.to_string()))?;
        Ok(matches)
    }

    /// Record a confirmed output into the UTXO set.
    pub fn record_utxo(&self, txid: &str, vout: u32, value: u64, address: &str, height: u32) {
        self.utxos.lock().unwrap().add(Utxo {
            txid: txid.to_string(),
            vout,
            value,
            address: address.to_string(),
            height,
        });
    }

    /// Scan blocks from `start_height` to the current tip for outputs paying
    /// the wallet's addresses, using BIP37 `merkleblock` messages.
    ///
    /// For each block hash, a `getdata` request for a filtered block
    /// (`MSG_FILTERED_BLOCK`) is sent; the peer replies with a `merkleblock`
    /// (and, for matched transactions, `tx` messages). Matched outputs are
    /// recorded into the UTXO set. Returns the number of blocks scanned.
    pub async fn scan_utxos(&self, peer: &mut BtcPeer, start_height: u32) -> Result<usize> {
        let hashes: Vec<[u8; 32]> = self.headers.lock().unwrap().hashes_from(start_height);
        if hashes.is_empty() {
            return Ok(0);
        }

        // Request filtered blocks in batches to bound memory and stay within
        // the peer's `getdata` limits.
        const BATCH: usize = 64;
        let mut scanned = 0usize;
        for chunk in hashes.chunks(BATCH) {
            let inv: Vec<Inventory> = chunk
                .iter()
                .map(|h| Inventory::Unknown {
                    inv_type: 3, // MSG_FILTERED_BLOCK
                    hash: *h,
                })
                .collect();
            peer.send(NetworkMessage::GetData(inv)).await?;

            // Collect the merkleblocks (and any tx messages) for this batch.
            let mut pending: VecDeque<bitcoin::merkle_tree::MerkleBlock> = VecDeque::new();
            let mut txs: Vec<bitcoin::Transaction> = Vec::new();
            let mut received = 0usize;
            while received < chunk.len() {
                match peer.recv().await? {
                    NetworkMessage::MerkleBlock(mb) => {
                        pending.push_back(mb);
                        received += 1;
                    }
                    NetworkMessage::Tx(tx) => txs.push(tx),
                    NetworkMessage::NotFound(_) => received += 1,
                    NetworkMessage::Inv(_) | NetworkMessage::Ping(_) | NetworkMessage::Pong(_) => {
                        continue
                    }
                    _ => continue,
                }
            }

            // Process each merkleblock: verify the merkle root and record
            // matched outputs that pay one of our addresses.
            for mb in pending {
                let height = {
                    let chain = self.headers.lock().unwrap();
                    let hash: [u8; 32] = mb.header.block_hash().to_byte_array();
                    chain.get(&hash).map(|h| h.height)
                };
                let Some(height) = height else { continue };
                let matched = self.extract_matched_txids(&mb)?;
                for txid in matched {
                    if let Some(tx) = txs.iter().find(|t| t.compute_txid() == txid) {
                        self.record_matching_outputs(tx, height);
                    }
                }
                scanned += 1;
            }
        }
        Ok(scanned)
    }

    /// Record every output of `tx` that pays one of the wallet's addresses.
    fn record_matching_outputs(&self, tx: &bitcoin::Transaction, height: u32) {
        let txid = tx.compute_txid().to_string();
        for (vout, out) in tx.output.iter().enumerate() {
            let script = out.script_pubkey.to_bytes();
            for addr in &self.addresses {
                if let Ok(a) = bitcoin::Address::from_str(addr) {
                    if let Ok(a) = a.require_network(bitcoin::Network::Bitcoin) {
                        if a.script_pubkey().to_bytes() == script {
                            self.record_utxo(&txid, vout as u32, out.value.to_sat(), addr, height);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_filter_nonempty() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec!["bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh".to_string()],
        );
        let filter = sync.build_filter();
        assert!(!filter.is_empty());
    }

    #[test]
    fn test_build_getheaders_empty_locator() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec![],
        );
        let msg = sync.build_getheaders();
        assert!(msg.locator_hashes.is_empty());
    }

    #[test]
    fn test_build_filterload() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec!["bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh".to_string()],
        );
        let fl = sync.build_filterload();
        assert!(!fl.filter.is_empty());
    }

    #[test]
    fn test_record_utxo() {
        let utxos = Arc::new(Mutex::new(UtxoSet::new()));
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            utxos.clone(),
            vec![],
        );
        sync.record_utxo(
            "11".repeat(32).as_str(),
            0,
            5000,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            100,
        );
        assert_eq!(utxos.lock().unwrap().total(), 5000);
    }

    #[test]
    fn test_record_matching_outputs() {
        let addr = "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";
        let utxos = Arc::new(Mutex::new(UtxoSet::new()));
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            utxos.clone(),
            vec![addr.to_string()],
        );

        // Build a tx with one output paying our address and one paying a
        // different address.
        let our_script = bitcoin::Address::from_str(addr)
            .unwrap()
            .require_network(bitcoin::Network::Bitcoin)
            .unwrap()
            .script_pubkey();
        let other_addr = crate::keys::derive_address(&[9u8; 64], 0).unwrap();
        let other_script = bitcoin::Address::from_str(&other_addr)
            .unwrap()
            .require_network(bitcoin::Network::Bitcoin)
            .unwrap()
            .script_pubkey();
        let tx = bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![
                bitcoin::TxOut {
                    value: bitcoin::Amount::from_sat(1234),
                    script_pubkey: our_script,
                },
                bitcoin::TxOut {
                    value: bitcoin::Amount::from_sat(9999),
                    script_pubkey: other_script,
                },
            ],
        };

        sync.record_matching_outputs(&tx, 42);
        let set = utxos.lock().unwrap();
        assert_eq!(set.list().len(), 1);
        assert_eq!(set.list()[0].value, 1234);
        assert_eq!(set.list()[0].height, 42);
    }
}
