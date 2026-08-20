//! Live Bitcoin header sync and Bloom-filter UTXO scan.

use crate::error::{BtcError, Result};
use crate::headers::HeaderChain;
use crate::p2p::BtcPeer;
use crate::utxo::{Utxo, UtxoSet};
use bitcoin::consensus::encode::serialize;
use bitcoin::hashes::Hash;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::{GetHeadersMessage, Inventory};
use bitcoin::p2p::message_filter::GetCFilters;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

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
    broadcast_tx_to(raw, bitcoin::Network::Bitcoin, &resolve_seeds().await?).await
}

/// Broadcast a raw transaction to a specific peer on a specific network.
/// Returns the txid on success.
pub async fn broadcast_tx_to(
    raw: &[u8],
    network: bitcoin::Network,
    addrs: &[SocketAddr],
) -> Result<[u8; 32]> {
    let tx: bitcoin::Transaction = bitcoin::consensus::encode::deserialize(raw)
        .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let txid = tx.compute_txid().to_byte_array();
    tracing::debug!(
        "BTC broadcast: tx {} ({} bytes, {} in, {} out) to {:?}",
        hex::encode(txid),
        raw.len(),
        tx.input.len(),
        tx.output.len(),
        addrs
    );
    tracing::debug!("BTC broadcast raw: {}", hex::encode(raw));
    let mut last_err = None;
    for addr in addrs {
        match crate::p2p::BtcPeer::connect_with_network(*addr, network).await {
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
    network: bitcoin::Network,
}

impl BtcSync {
    pub fn new(
        headers: Arc<Mutex<HeaderChain>>,
        utxos: Arc<Mutex<UtxoSet>>,
        addresses: Vec<String>,
        network: bitcoin::Network,
    ) -> Self {
        Self {
            headers,
            utxos,
            addresses,
            network,
        }
    }

    /// Build a `getheaders` message from the current tip.
    pub fn build_getheaders(&self) -> GetHeadersMessage {
        let headers = self.headers.lock().unwrap();
        let locator = if let Some(best) = headers.best_hash() {
            vec![bitcoin::BlockHash::from_byte_array(best)]
        } else {
            // No headers yet: send a single all-zeros locator so the peer
            // responds from genesis (an empty locator is rejected by some
            // implementations).
            vec![bitcoin::BlockHash::all_zeros()]
        };
        GetHeadersMessage {
            version: 70016,
            locator_hashes: locator,
            stop_hash: bitcoin::BlockHash::all_zeros(),
        }
    }

    /// Run one sync pass against a single peer.
    ///
    /// Header sync does not send a `filterload`: modern Bitcoin Core nodes
    /// disable BIP-37 bloom filters by default and disconnect peers that send
    /// one. The filter is only used by the UTXO scan.
    pub async fn sync_once(&self, peer: &mut BtcPeer) -> Result<usize> {
        peer.send(NetworkMessage::GetHeaders(self.build_getheaders()))
            .await?;

        let mut added = 0usize;
        loop {
            let msg = peer.recv().await?;
            tracing::debug!("BTC sync: received {:?}", msg);
            match msg {
                NetworkMessage::Headers(hdrs) => {
                    let batch_len = hdrs.len();
                    for h in hdrs {
                        let raw = serialize(&h);
                        let height = {
                            let chain = self.headers.lock().unwrap();
                            chain.best_height() + 1
                        };
                        self.headers.lock().unwrap().add_header(&raw, height)?;
                        added += 1;
                    }
                    // Bitcoin Core caps each `headers` message at 2000 entries.
                    // Keep requesting until the peer sends a short batch (or
                    // none), so a single sync pass advances the full chain.
                    if batch_len < 2000 {
                        break;
                    }
                    peer.send(NetworkMessage::GetHeaders(self.build_getheaders()))
                        .await?;
                }
                NetworkMessage::Ping(nonce) => {
                    peer.send(NetworkMessage::Pong(nonce)).await?;
                }
                NetworkMessage::Verack | NetworkMessage::Version(_) => continue,
                _ => continue,
            }
        }
        Ok(added)
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

    /// Record every output of `tx` that pays one of the wallet's addresses, and
    /// remove any previously-recorded output that this tx spends.
    fn record_matching_outputs(&self, tx: &bitcoin::Transaction, height: u32) {
        let txid = tx.compute_txid().to_string();

        // Remove spent outputs: any input that references a UTXO we already
        // recorded is now spent and must be dropped from the set.
        for input in &tx.input {
            let prev_txid = input.previous_output.txid.to_string();
            let prev_vout = input.previous_output.vout;
            self.utxos.lock().unwrap().remove(&prev_txid, prev_vout);
        }

        for (vout, out) in tx.output.iter().enumerate() {
            let script = out.script_pubkey.to_bytes();
            for addr in &self.addresses {
                if let Ok(a) = bitcoin::Address::from_str(addr) {
                    if let Ok(a) = a.require_network(self.network) {
                        if a.script_pubkey().to_bytes() == script {
                            self.record_utxo(&txid, vout as u32, out.value.to_sat(), addr, height);
                        }
                    }
                }
            }
        }
    }

    /// Scan blocks from `start_height` to the tip using BIP-158 compact block
    /// filters (the modern, privacy-preserving alternative to BIP-37).
    ///
    /// For each block, download its `cfilter` and test whether any watched
    /// scriptPubKey matches. Only matching blocks are downloaded in full, so
    /// the full node never learns which addresses the client is watching.
    /// Returns the number of blocks scanned.
    pub async fn scan_utxos_bip158(&self, peer: &mut BtcPeer, start_height: u32) -> Result<usize> {
        use bitcoin::bip158::BlockFilter;

        let hashes: Vec<[u8; 32]> = self.headers.lock().unwrap().hashes_from(start_height);
        if hashes.is_empty() {
            return Ok(0);
        }

        // The watched scriptPubKeys (BIP-158 matches against the full script).
        let watched: Vec<Vec<u8>> = self
            .addresses
            .iter()
            .filter_map(|addr| {
                bitcoin::Address::from_str(addr)
                    .ok()?
                    .require_network(self.network)
                    .ok()
                    .map(|a| a.script_pubkey().to_bytes())
            })
            .collect();
        if watched.is_empty() {
            return Ok(0);
        }

        // BIP-158 basic filter type is 0x00.
        const FILTER_TYPE_BASIC: u8 = 0x00;

        // Request the full range of filters in one `getcfilters` message. The
        // peer replies with one `cfilter` per block, in ascending height order.
        let stop_hash = bitcoin::BlockHash::from_byte_array(*hashes.last().unwrap());
        peer.send(NetworkMessage::GetCFilters(GetCFilters {
            filter_type: FILTER_TYPE_BASIC,
            start_height,
            stop_hash,
        }))
        .await?;

        let mut scanned = 0usize;
        for hash in hashes {
            // Read the next cfilter response (in order). The peer's blockfilter
            // index can lag behind the tip, so it may send fewer filters than
            // requested; time out rather than block forever.
            let filter = loop {
                match tokio::time::timeout(std::time::Duration::from_secs(10), peer.recv()).await {
                    Err(_) => return Ok(scanned),
                    Ok(Err(e)) => return Err(e),
                    Ok(Ok(NetworkMessage::CFilter(cf))) => break cf,
                    Ok(Ok(NetworkMessage::Ping(nonce))) => {
                        peer.send(NetworkMessage::Pong(nonce)).await?;
                    }
                    Ok(Ok(_)) => continue,
                }
            };

            // The cfilter response carries the authoritative block hash, which
            // is the SipHash key for the filter. Use it (not our header-chain
            // hash) so the match is keyed correctly.
            let block_hash = filter.block_hash;
            let bf = BlockFilter::new(&filter.filter);
            let matched = bf
                .match_any(&block_hash, watched.iter().map(|s| s.as_slice()))
                .map_err(|e| BtcError::Sync(e.to_string()))?;
            tracing::debug!(
                "BTC BIP-158: block {} (cfilter hash {}) filter {} bytes, matched={}",
                hex::encode(hash),
                hex::encode(filter.block_hash.to_byte_array()),
                filter.filter.len(),
                matched
            );

            if matched {
                // Download the full block and record matching outputs.
                peer.send(NetworkMessage::GetData(vec![Inventory::WitnessBlock(
                    block_hash,
                )]))
                .await?;
                let block = loop {
                    match peer.recv().await? {
                        NetworkMessage::Block(b) => break b,
                        NetworkMessage::Ping(nonce) => {
                            peer.send(NetworkMessage::Pong(nonce)).await?;
                        }
                        _ => continue,
                    }
                };
                let height = {
                    let chain = self.headers.lock().unwrap();
                    // Look up the height by the block hash we actually
                    // downloaded (the cfilter's authoritative hash), not the
                    // header-chain hash, so outputs are attributed to the
                    // correct height even if the chains diverge.
                    let h: [u8; 32] = block_hash.to_byte_array();
                    chain.get(&h).map(|h| h.height)
                };
                if let Some(height) = height {
                    for tx in &block.txdata {
                        self.record_matching_outputs(tx, height);
                    }
                }
            }
            scanned += 1;
        }
        Ok(scanned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_getheaders_empty_locator() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec![],
            bitcoin::Network::Bitcoin,
        );
        let msg = sync.build_getheaders();
        // A fresh chain sends a single all-zeros locator (not empty) so peers
        // respond from genesis.
        assert_eq!(msg.locator_hashes.len(), 1);
        assert_eq!(msg.locator_hashes[0], bitcoin::BlockHash::all_zeros());
    }

    #[test]
    fn test_record_utxo() {
        let utxos = Arc::new(Mutex::new(UtxoSet::new()));
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            utxos.clone(),
            vec![],
            bitcoin::Network::Bitcoin,
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
            bitcoin::Network::Bitcoin,
        );

        // Build a tx with one output paying our address and one paying a
        // different address.
        let our_script = bitcoin::Address::from_str(addr)
            .unwrap()
            .require_network(bitcoin::Network::Bitcoin)
            .unwrap()
            .script_pubkey();
        let other_addr =
            crate::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Bitcoin).unwrap();
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

    #[test]
    fn test_bip158_filter_matches_watched_script() {
        use bitcoin::bip158::BlockFilter;

        // Build a block whose coinbase pays our watched address.
        let addr = "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";
        let script = bitcoin::Address::from_str(addr)
            .unwrap()
            .require_network(bitcoin::Network::Bitcoin)
            .unwrap()
            .script_pubkey();
        let tx = bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![bitcoin::TxOut {
                value: bitcoin::Amount::from_sat(50_000_000),
                script_pubkey: script.clone(),
            }],
        };
        let block = bitcoin::Block {
            header: bitcoin::block::Header {
                version: bitcoin::block::Version::TWO,
                prev_blockhash: bitcoin::BlockHash::all_zeros(),
                merkle_root: bitcoin::TxMerkleNode::all_zeros(),
                time: 1_700_000_000,
                bits: bitcoin::CompactTarget::from_consensus(0x1d00ffff),
                nonce: 0,
            },
            txdata: vec![tx],
        };
        let block_hash = block.block_hash();

        // Build the filter the way a full node would.
        let mut out = Vec::new();
        {
            let mut writer = bitcoin::bip158::BlockFilterWriter::new(&mut out, &block);
            writer.add_output_scripts();
            writer.finish().unwrap();
        }
        let bf = BlockFilter::new(&out);

        // The watched script must match (this is the keying fix: the filter is
        // keyed by the block hash, not the header-chain hash).
        let matched = bf
            .match_any(&block_hash, std::iter::once(script.as_bytes()))
            .unwrap();
        assert!(matched, "watched script should match its own block filter");
    }
}
