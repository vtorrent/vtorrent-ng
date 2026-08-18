//! Live Bitcoin header sync and Bloom-filter UTXO scan.

use crate::error::{BtcError, Result};
use crate::headers::HeaderChain;
use crate::p2p::BtcPeer;
use crate::utxo::{Utxo, UtxoSet};
use bitcoin::consensus::encode::serialize;
use bitcoin::hashes::Hash;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::GetHeadersMessage;
use bitcoin::p2p::message_bloom::FilterLoad;
use std::net::SocketAddr;
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
    pub fn build_filter(&self) -> BloomFilter {
        let mut filter = BloomFilter::new(self.addresses.len().max(1), 0.001, 0);
        for addr in &self.addresses {
            filter.insert(addr.as_bytes());
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_filter_nonempty() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec!["bc1qtest".to_string()],
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
            vec!["bc1qtest".to_string()],
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
        sync.record_utxo("11".repeat(32).as_str(), 0, 5000, "bc1qtest", 100);
        assert_eq!(utxos.lock().unwrap().total(), 5000);
    }
}
