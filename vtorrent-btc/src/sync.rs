//! Live Bitcoin header sync and Bloom-filter UTXO scan.

use crate::error::{BtcError, Result};
use crate::filters::{
    FilterHeaderStore, CFHEADERS_RANGE_LIMIT, CFILTERS_RANGE_LIMIT, CHECKPOINT_INTERVAL,
};
use crate::headers::HeaderChain;
use crate::p2p::BtcPeer;
use crate::utxo::{Utxo, UtxoSet};
use bitcoin::consensus::encode::serialize;
use bitcoin::hashes::Hash;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::{GetHeadersMessage, Inventory};
use bitcoin::p2p::message_filter::{GetCFCheckpt, GetCFHeaders, GetCFilters};
use parking_lot::Mutex;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

/// Bitcoin DNS seeds (mainnet).
pub const DNS_SEEDS: &[&str] = &[
    "seed.bitcoin.sipa.be",
    "dnsseed.bluematt.me",
    "dnsseed.bitcoin.dashjr.org",
];

/// Required funding depth before revealing an atomic-swap secret.
pub const BTC_SWAP_CONFIRMATIONS: u32 = 6;
/// Minimum time before BTC refund eligibility when revealing the secret.
pub const MIN_BTC_CLAIM_WINDOW: u32 = 3600;
/// Bound each claim scan; older funding fails closed and needs separate recovery.
const SWAP_SCAN_BLOCKS: u32 = 1_008;
const MAX_SWAP_TIP_AGE: u64 = 2 * 3600;

mod swap_observation;
pub use swap_observation::{SwapAnchor, SwapScan};

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

    // Fan out to up to 3 peers concurrently for reliability.
    let fan_out = addrs.len().min(3);
    let addrs_slice = &addrs[..fan_out];

    let handles: Vec<_> = addrs_slice
        .iter()
        .map(|addr| {
            let tx = tx.clone();
            let addr = *addr;
            tokio::spawn(async move {
                match crate::p2p::BtcPeer::connect_with_network(addr, network).await {
                    Ok(mut peer) => peer.broadcast_tx(&tx).await.map(|_| addr),
                    Err(e) => Err(e),
                }
            })
        })
        .collect();

    let mut last_err = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(addr)) => {
                tracing::info!("BTC broadcast accepted by peer {}", addr);
                return Ok(txid);
            }
            Ok(Err(e)) => last_err = Some(e),
            Err(e) => last_err = Some(BtcError::P2p(e.to_string())),
        }
    }
    Err(last_err.unwrap_or_else(|| BtcError::P2p("no reachable peers".into())))
}

/// A Bitcoin SPV sync engine.
pub struct BtcSync {
    headers: Arc<Mutex<HeaderChain>>,
    utxos: Arc<Mutex<UtxoSet>>,
    filter_headers: Arc<Mutex<FilterHeaderStore>>,
    addresses: Vec<String>,
    network: bitcoin::Network,
    coinbase_txids: Mutex<std::collections::HashSet<String>>,
    swap_scan: Mutex<Option<swap_observation::SwapScanTracker>>,
}

impl BtcSync {
    pub(crate) async fn verify_swap_funding(
        &self,
        htlc: &crate::htlc::BtcHtlc,
        funding_txid: [u8; 32],
        addrs: &[SocketAddr],
        now: u64,
    ) -> Result<()> {
        self.scan_swap_contract(htlc, funding_txid, addrs, now, true)
            .await
            .map(|_| ())
    }

    pub(crate) async fn scan_swap_contract(
        &self,
        htlc: &crate::htlc::BtcHtlc,
        funding_txid: [u8; 32],
        addrs: &[SocketAddr],
        now: u64,
        require_claim_window: bool,
    ) -> Result<SwapScan> {
        let required = if self.network == bitcoin::Network::Regtest {
            1
        } else {
            2
        };
        if addrs
            .iter()
            .map(|addr| addr.ip())
            .collect::<std::collections::HashSet<_>>()
            .len()
            < required
        {
            return Err(BtcError::Sync(format!(
                "BTC contract verification requires {required} distinct compact-filter peers"
            )));
        }
        let mut peers = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for addr in addrs.iter().filter(|addr| seen.insert(addr.ip())).take(8) {
            let attempt = async {
                let mut peer = BtcPeer::connect_with_network(*addr, self.network).await?;
                if !peer.supports_compact_filters() {
                    return Err(BtcError::Sync("BTC peer lacks compact filters".into()));
                }
                self.sync_once(&mut peer).await?;
                Ok(peer)
            };
            if let Ok(Ok(peer)) =
                tokio::time::timeout(std::time::Duration::from_secs(15), attempt).await
            {
                peers.push(peer);
                if peers.len()
                    >= if self.network == bitcoin::Network::Regtest {
                        1
                    } else {
                        3
                    }
                {
                    break;
                }
            }
        }
        if peers.len() < required {
            return Err(BtcError::Sync(format!(
                "BTC contract verification requires {required} distinct compact-filter peers"
            )));
        }
        let (tip_hash, tip_height) = {
            let chain = self.headers.lock();
            let hash = chain
                .best_hash()
                .ok_or_else(|| BtcError::Sync("Missing BTC tip".into()))?;
            let tip = chain
                .get(&hash)
                .ok_or_else(|| BtcError::Sync("Missing BTC header".into()))?;
            if now.abs_diff(u64::from(tip.header.time)) > MAX_SWAP_TIP_AGE {
                return Err(BtcError::Sync(
                    "BTC tip is stale or too far in the future".into(),
                ));
            }
            // Bound median-time-past conservatively, including a tip whose
            // timestamp went backwards relative to recent headers.
            let mut cursor = hash;
            let mut latest_time = now;
            for _ in 0..11 {
                let header = chain
                    .get(&cursor)
                    .ok_or_else(|| BtcError::Sync("Missing BTC time ancestor".into()))?;
                latest_time = latest_time.max(u64::from(header.header.time));
                if header.height == 0 {
                    break;
                }
                cursor = header.header.prev_blockhash.to_byte_array();
            }
            if require_claim_window
                && (htlc.expiry < 500_000_000
                    || latest_time.saturating_add(u64::from(MIN_BTC_CLAIM_WINDOW))
                        >= u64::from(htlc.expiry))
            {
                return Err(BtcError::Sync(
                    "BTC chain time is too close to refund eligibility".into(),
                ));
            }
            (hash, tip.height)
        };
        let start = tip_height.saturating_sub(SWAP_SCAN_BLOCKS - 1).max(1);
        let mut last_error = BtcError::Sync("BTC contract scan did not complete".into());
        for peer in &mut peers {
            *self.utxos.lock() = UtxoSet::new();
            *self.swap_scan.lock() =
                Some(swap_observation::SwapScanTracker::new(htlc, funding_txid)?);
            match self.scan_utxos_bip158(peer, start).await {
                Ok(scanned) => {
                    if require_claim_window {
                        self.verify_scanned_swap(htlc, funding_txid, start, tip_hash, scanned)?;
                    }
                    return self.finish_swap_scan(start, tip_hash, scanned);
                }
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn verify_scanned_swap(
        &self,
        htlc: &crate::htlc::BtcHtlc,
        funding_txid: [u8; 32],
        start: u32,
        tip_hash: [u8; 32],
        scanned: usize,
    ) -> Result<()> {
        let chain = self.headers.lock();
        let tip = chain.best_height();
        let expected = tip.checked_sub(start).map(|n| u64::from(n) + 1);
        if chain.best_hash() != Some(tip_hash) || expected != Some(scanned as u64) {
            return Err(BtcError::Sync(
                "BTC contract scan is incomplete or its tip changed".into(),
            ));
        }
        let txid = bitcoin::Txid::from_byte_array(funding_txid).to_string();
        let address = htlc.address()?;
        let set = self.utxos.lock();
        let output = set
            .list()
            .iter()
            .find(|u| u.txid == txid && u.vout == 0)
            .ok_or_else(|| {
                BtcError::Sync(
                    "BTC funding output is missing, unconfirmed, spent, or outside the scan window"
                        .into(),
                )
            })?;
        if output.value != htlc.amount || output.address != address || output.height < start {
            return Err(BtcError::Sync(
                "BTC funding output does not match the swap contract".into(),
            ));
        }
        let confirmations = tip
            .checked_sub(output.height)
            .map(|n| u64::from(n) + 1)
            .unwrap_or(0);
        if confirmations < u64::from(BTC_SWAP_CONFIRMATIONS) {
            return Err(BtcError::Sync(format!("BTC funding requires {BTC_SWAP_CONFIRMATIONS} confirmations; found {confirmations}")));
        }
        if self.coinbase_txids.lock().contains(&txid) && confirmations < 100 {
            return Err(BtcError::Sync("BTC funding coinbase is immature".into()));
        }
        Ok(())
    }

    pub fn new(
        headers: Arc<Mutex<HeaderChain>>,
        utxos: Arc<Mutex<UtxoSet>>,
        addresses: Vec<String>,
        network: bitcoin::Network,
    ) -> Self {
        Self {
            headers,
            utxos,
            filter_headers: Arc::new(Mutex::new(FilterHeaderStore::default())),
            addresses,
            network,
            coinbase_txids: Mutex::new(std::collections::HashSet::new()),
            swap_scan: Mutex::new(None),
        }
    }

    pub(crate) fn new_with_filter_headers(
        headers: Arc<Mutex<HeaderChain>>,
        utxos: Arc<Mutex<UtxoSet>>,
        filter_headers: Arc<Mutex<FilterHeaderStore>>,
        addresses: Vec<String>,
        network: bitcoin::Network,
    ) -> Self {
        Self {
            headers,
            utxos,
            filter_headers,
            addresses,
            network,
            coinbase_txids: Mutex::new(std::collections::HashSet::new()),
            swap_scan: Mutex::new(None),
        }
    }

    /// Build a `getheaders` message from the current tip.
    pub fn build_getheaders(&self) -> GetHeadersMessage {
        let headers = self.headers.lock();
        let locator = headers.block_locator();
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
                    let last_hash = hdrs.last().map(|header| header.block_hash());
                    for h in hdrs {
                        let raw = serialize(&h);
                        let height = {
                            let chain = self.headers.lock();
                            chain
                                .get(&h.prev_blockhash.to_byte_array())
                                .and_then(|parent| parent.height.checked_add(1))
                                .ok_or_else(|| BtcError::Sync("Missing BTC header parent".into()))?
                        };
                        self.headers.lock().add_header(&raw, height)?;
                        added += 1;
                    }
                    // Bitcoin Core caps each `headers` message at 2000 entries.
                    // Keep requesting until the peer sends a short batch (or
                    // none), so a single sync pass advances the full chain.
                    if batch_len < 2000 {
                        break;
                    }
                    let mut request = self.build_getheaders();
                    if let Some(hash) = last_hash {
                        request.locator_hashes.insert(0, hash);
                    }
                    peer.send(NetworkMessage::GetHeaders(request)).await?;
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
        self.utxos.lock().add(Utxo {
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
            self.utxos.lock().remove(&prev_txid, prev_vout);
        }

        for (vout, out) in tx.output.iter().enumerate() {
            let script = out.script_pubkey.to_bytes();
            for addr in &self.addresses {
                if let Ok(a) = bitcoin::Address::from_str(addr) {
                    if let Ok(a) = a.require_network(self.network) {
                        if a.script_pubkey().to_bytes() == script {
                            if tx.is_coinbase() {
                                self.coinbase_txids.lock().insert(txid.clone());
                            }
                            self.record_utxo(&txid, vout as u32, out.value.to_sat(), addr, height);
                        }
                    }
                }
            }
        }
    }

    async fn authenticate_filter_headers(
        &self,
        peer: &mut BtcPeer,
        start_height: u32,
    ) -> Result<()> {
        use bitcoin::bip158::FilterHeader;

        if !peer.supports_compact_filters() {
            return Err(BtcError::Sync(format!(
                "peer {} does not advertise compact-filter support",
                peer.addr()
            )));
        }
        let (tip_height, tip_hash) = {
            let chain = self.headers.lock();
            let hash = chain
                .best_hash()
                .ok_or_else(|| BtcError::Sync("Bitcoin header chain is empty".into()))?;
            (
                chain.best_height(),
                bitcoin::BlockHash::from_byte_array(hash),
            )
        };

        const FILTER_TYPE_BASIC: u8 = 0;
        peer.send(NetworkMessage::GetCFCheckpt(GetCFCheckpt {
            filter_type: FILTER_TYPE_BASIC,
            stop_hash: tip_hash,
        }))
        .await?;
        let checkpoints = loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), peer.recv()).await {
                Err(_) => return Err(BtcError::Sync("compact-filter checkpoint timeout".into())),
                Ok(Err(error)) => return Err(error),
                Ok(Ok(NetworkMessage::CFCheckpt(checkpoints))) => break checkpoints,
                Ok(Ok(NetworkMessage::Ping(nonce))) => {
                    peer.send(NetworkMessage::Pong(nonce)).await?;
                }
                Ok(Ok(_)) => continue,
            }
        };
        if checkpoints.filter_type != FILTER_TYPE_BASIC || checkpoints.stop_hash != tip_hash {
            return Err(BtcError::Sync(
                "compact-filter checkpoint response does not match request".into(),
            ));
        }
        let mut candidate = FilterHeaderStore::default();
        candidate.reconcile_checkpoints(
            tip_hash.to_byte_array(),
            tip_height,
            &checkpoints.filter_headers,
        )?;

        let checkpoint_height = start_height
            .saturating_sub(1)
            .checked_div(CHECKPOINT_INTERVAL)
            .unwrap_or(0)
            * CHECKPOINT_INTERVAL;
        let (request_start, mut previous_header) = if checkpoint_height > 0 {
            let header = candidate
                .checkpoint(checkpoint_height)
                .ok_or_else(|| BtcError::Sync("missing compact-filter checkpoint anchor".into()))?;
            (checkpoint_height + 1, header)
        } else {
            (0, FilterHeader::all_zeros())
        };
        let block_hashes = self.headers.lock().hashes_from(request_start);
        if block_hashes.len() != tip_height.saturating_sub(request_start) as usize + 1 {
            return Err(BtcError::Sync(
                "Bitcoin main-chain header range is not contiguous".into(),
            ));
        }

        for (chunk_index, chunk) in block_hashes.chunks(CFHEADERS_RANGE_LIMIT).enumerate() {
            let chunk_start = request_start + (chunk_index * CFHEADERS_RANGE_LIMIT) as u32;
            let stop_hash = bitcoin::BlockHash::from_byte_array(*chunk.last().unwrap());
            peer.send(NetworkMessage::GetCFHeaders(GetCFHeaders {
                filter_type: FILTER_TYPE_BASIC,
                start_height: chunk_start,
                stop_hash,
            }))
            .await?;
            let response = loop {
                match tokio::time::timeout(std::time::Duration::from_secs(10), peer.recv()).await {
                    Err(_) => return Err(BtcError::Sync("compact-filter header timeout".into())),
                    Ok(Err(error)) => return Err(error),
                    Ok(Ok(NetworkMessage::CFHeaders(headers))) => break headers,
                    Ok(Ok(NetworkMessage::Ping(nonce))) => {
                        peer.send(NetworkMessage::Pong(nonce)).await?;
                    }
                    Ok(Ok(_)) => continue,
                }
            };
            if response.filter_type != FILTER_TYPE_BASIC
                || response.stop_hash != stop_hash
                || response.previous_filter_header != previous_header
                || response.filter_hashes.len() != chunk.len()
            {
                return Err(BtcError::Sync(format!(
                    "invalid compact-filter header response for heights {}..{}",
                    chunk_start,
                    chunk_start + chunk.len() as u32 - 1
                )));
            }
            previous_header = candidate.apply_range(
                chunk_start,
                chunk,
                &response.filter_hashes,
                previous_header,
            )?;
        }

        let required = if self.network == bitcoin::Network::Regtest {
            1
        } else {
            2
        };
        let agreement =
            self.filter_headers
                .lock()
                .observe_candidate(peer.addr(), candidate, required)?;
        if agreement < required {
            return Err(BtcError::Sync(format!(
                "compact-filter headers require agreement from {} peers (have {})",
                required, agreement
            )));
        }
        Ok(())
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

        let hashes: Vec<[u8; 32]> = self.headers.lock().hashes_from(start_height);
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

        self.authenticate_filter_headers(peer, start_height).await?;

        // BIP-158 basic filter type is 0x00.
        const FILTER_TYPE_BASIC: u8 = 0x00;

        let mut scanned = 0usize;
        let mut queued_filters = std::collections::VecDeque::new();
        for (chunk_index, chunk) in hashes.chunks(CFILTERS_RANGE_LIMIT).enumerate() {
            let chunk_start = start_height + (chunk_index * CFILTERS_RANGE_LIMIT) as u32;
            let stop_hash = bitcoin::BlockHash::from_byte_array(*chunk.last().unwrap());
            peer.send(NetworkMessage::GetCFilters(GetCFilters {
                filter_type: FILTER_TYPE_BASIC,
                start_height: chunk_start,
                stop_hash,
            }))
            .await?;

            for hash in chunk {
                let expected_block_hash = bitcoin::BlockHash::from_byte_array(*hash);
                // Read the next cfilter response (in order). The peer's blockfilter
                // index can lag behind the tip, so it may send fewer filters than
                // requested; time out rather than block forever.
                let filter = loop {
                    if let Some(filter) = queued_filters.pop_front() {
                        break filter;
                    }
                    match tokio::time::timeout(std::time::Duration::from_secs(10), peer.recv())
                        .await
                    {
                        Err(_) => return Ok(scanned),
                        Ok(Err(e)) => return Err(e),
                        Ok(Ok(NetworkMessage::CFilter(cf))) => break cf,
                        Ok(Ok(NetworkMessage::Ping(nonce))) => {
                            peer.send(NetworkMessage::Pong(nonce)).await?;
                        }
                        Ok(Ok(_)) => continue,
                    }
                };

                if filter.filter_type != FILTER_TYPE_BASIC
                    || filter.block_hash != expected_block_hash
                {
                    return Err(BtcError::Sync(format!(
                        "compact filter type/hash {} does not match requested {}",
                        filter.block_hash, expected_block_hash
                    )));
                }
                self.filter_headers
                    .lock()
                    .verify_filter(hash, &filter.filter)?;

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
                            NetworkMessage::CFilter(filter) => {
                                if queued_filters.len() >= CFILTERS_RANGE_LIMIT {
                                    return Err(BtcError::Sync(
                                        "Too many queued compact filters".into(),
                                    ));
                                }
                                queued_filters.push_back(filter);
                            }
                            NetworkMessage::Ping(nonce) => {
                                peer.send(NetworkMessage::Pong(nonce)).await?;
                            }
                            _ => continue,
                        }
                    };
                    if block.block_hash() != block_hash {
                        return Err(BtcError::Sync(format!(
                            "received block {} while requesting {}",
                            block.block_hash(),
                            block_hash
                        )));
                    }
                    if !block.check_merkle_root() || !block.check_witness_commitment() {
                        return Err(BtcError::Sync(format!(
                            "block {} has an invalid transaction commitment",
                            block_hash
                        )));
                    }
                    let height = {
                        let chain = self.headers.lock();
                        // Look up the height by the block hash we actually
                        // downloaded (the cfilter's authoritative hash), not the
                        // header-chain hash, so outputs are attributed to the
                        // correct height even if the chains diverge.
                        let h: [u8; 32] = block_hash.to_byte_array();
                        chain.get(&h).map(|h| h.height)
                    };
                    if let Some(height) = height {
                        for tx in &block.txdata {
                            if let Some(scan) = self.swap_scan.lock().as_mut() {
                                scan.record(tx, height, block_hash.to_byte_array());
                            }
                            self.record_matching_outputs(tx, height);
                        }
                    }
                }
                scanned += 1;
            }
        }
        Ok(scanned)
    }
}

#[cfg(test)]
#[path = "sync/swap_tests.rs"]
mod swap_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_getheaders_empty_locator() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::unanchored_for_tests())),
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
            Arc::new(Mutex::new(HeaderChain::unanchored_for_tests())),
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
        assert_eq!(utxos.lock().total(), 5000);
    }

    #[test]
    fn test_record_matching_outputs() {
        let addr = "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";
        let utxos = Arc::new(Mutex::new(UtxoSet::new()));
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::unanchored_for_tests())),
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
        let set = utxos.lock();
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
