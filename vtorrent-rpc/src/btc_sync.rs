//! Shared Bitcoin SPV sync loop for the daemon and the desktop node.

use std::{
    collections::HashSet,
    net::SocketAddr,
    path::Path,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::btc_peers::resolve_btc_peers;
use crate::state::{AppState, BtcSyncHealth};
use tokio::sync::RwLock;
use vtorrent_btc::{p2p::BtcPeer, sync::network_group, wallet::BtcWallet};

async fn connect_filter_peers(
    candidates: &[SocketAddr],
    cursor: &mut usize,
    peers: &mut Vec<BtcPeer>,
    network: bitcoin::Network,
) {
    let target = if network == bitcoin::Network::Regtest {
        1
    } else {
        4
    };
    while peers.len() < target && *cursor < candidates.len() {
        let addr = candidates[*cursor];
        *cursor += 1;
        if peers
            .iter()
            .any(|peer| network_group(peer.addr().ip()) == network_group(addr.ip()))
        {
            continue;
        }
        match tokio::time::timeout(
            Duration::from_secs(5),
            BtcPeer::connect_with_network(addr, network),
        )
        .await
        {
            Ok(Ok(peer)) if peer.supports_compact_filters() => {
                tracing::info!("BTC compact-filter peer connected: {}", addr);
                peers.push(peer);
            }
            Ok(Ok(_)) => tracing::debug!("BTC peer {} does not advertise compact filters", addr),
            Ok(Err(error)) => tracing::warn!("BTC peer {} failed: {}", addr, error),
            Err(_) => tracing::warn!("BTC peer {} connect timed out", addr),
        }
    }
}

fn update_peer_group_metric(health: &BtcSyncHealth, peers: &[BtcPeer]) {
    let groups: HashSet<_> = peers
        .iter()
        .map(|peer| network_group(peer.addr().ip()))
        .collect();
    health
        .peer_groups
        .store(groups.len() as u64, Ordering::Relaxed);
}

fn update_wallet_metrics(health: &BtcSyncHealth, wallet: &BtcWallet) {
    health
        .best_height
        .store(wallet.best_height() as u64, Ordering::Relaxed);
    health
        .last_scanned_height
        .store(wallet.last_scanned_height() as u64, Ordering::Relaxed);
    health
        .retained_floor
        .store(wallet.retained_floor() as u64, Ordering::Relaxed);
    health
        .wallet_history_blocked
        .store(wallet.history_unavailable(), Ordering::Relaxed);
    health.synced.store(wallet.synced(), Ordering::Relaxed);
}

/// Publish the wallet's recovery state to the metrics singletons.
///
/// Runs before any peer resolution or connection, so a daemon that restarts
/// into a blocked wallet with no reachable peers still reports
/// `wallet_history_blocked` instead of publishing zero until a peer returns.
async fn publish_wallet_metrics(state: &AppState) {
    if let Some(wallet) = state.btc_wallet.read().await.as_ref() {
        update_wallet_metrics(&state.btc_health, wallet);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn load_peer_cache(path: Option<&Path>) -> Vec<SocketAddr> {
    let Some(path) = path else {
        return Vec::new();
    };
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!("BTC peer cache {} unreadable: {}", path.display(), error);
            return Vec::new();
        }
    };
    match serde_json::from_str::<Vec<String>>(&text) {
        Ok(peers) => {
            let peers: Vec<SocketAddr> = peers
                .into_iter()
                .filter_map(|peer| peer.parse().ok())
                .collect();
            if !peers.is_empty() {
                tracing::info!(
                    "Loaded {} cached BTC peer(s) from {}",
                    peers.len(),
                    path.display()
                );
            }
            peers
        }
        Err(error) => {
            tracing::warn!("BTC peer cache {} invalid: {}", path.display(), error);
            Vec::new()
        }
    }
}

async fn persist_peer_cache(path: &Path, peers: &[SocketAddr]) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(
        &peers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<String>>(),
    )
    .map_err(std::io::Error::other)?;
    let temporary = path.with_extension("tmp");
    tokio::fs::write(&temporary, text).await?;
    tokio::fs::rename(&temporary, path).await
}

async fn remember_connected_peers(
    cache: &RwLock<Vec<SocketAddr>>,
    path: Option<&Path>,
    peers: &[BtcPeer],
) {
    if peers.is_empty() {
        return;
    }
    let mut cache = cache.write().await;
    let previous = cache.clone();
    for peer in peers
        .iter()
        .rev()
        .filter(|peer| peer.compact_filters_validated())
    {
        let addr = peer.addr();
        if let Some(index) = cache.iter().position(|item| *item == addr) {
            cache.remove(index);
        }
        cache.insert(0, addr);
    }
    cache.truncate(50);
    if *cache == previous {
        return;
    }
    if let Some(path) = path {
        if let Err(error) = persist_peer_cache(path, &cache).await {
            tracing::warn!("BTC peer cache {} write failed: {}", path.display(), error);
        }
    }
}

/// Sync BTC headers and scan wallet UTXOs, then release expired input
/// reservations. Runs until the process exits.
pub async fn run_sync_loop(state: Arc<AppState>) {
    tracing::info!("Bitcoin SPV sync task started");
    loop {
        if state.btc_wallet.read().await.is_none() {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        }
        // Publish before peers are resolved or connected: a blocked wallet must
        // report its state even when the peer set ends up empty.
        publish_wallet_metrics(&state).await;
        let network = *state.btc_network.read().await;
        let health = state.btc_health.clone();
        let peer_cache = state.btc_peer_cache.clone();
        let peer_cache_path = state.btc_peer_cache_path.clone();
        if peer_cache.read().await.is_empty() {
            let loaded = load_peer_cache(peer_cache_path.as_deref()).await;
            if !loaded.is_empty() {
                *peer_cache.write().await = loaded;
            }
        }
        let mut candidates = match resolve_btc_peers(&state, network).await {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!("BTC peer resolution failed: {}", error);
                tokio::time::sleep(Duration::from_secs(300)).await;
                continue;
            }
        };
        candidates.truncate(64);
        let mut cursor = 0;

        let mut peers = Vec::new();
        connect_filter_peers(&candidates, &mut cursor, &mut peers, network).await;
        update_peer_group_metric(&health, &peers);
        if peers.is_empty() {
            tracing::warn!("No usable BTC peers; retrying in 300s");
            tokio::time::sleep(Duration::from_secs(300)).await;
            continue;
        }
        if let Some(wallet) = state.btc_wallet.write().await.as_mut() {
            update_wallet_metrics(&health, wallet);
            let synced = loop {
                match wallet.sync_with_peers(&mut peers).await {
                    Ok(n) => {
                        tracing::info!("BTC sync: {} headers", n);
                        connect_filter_peers(&candidates, &mut cursor, &mut peers, network).await;
                        update_peer_group_metric(&health, &peers);
                        update_wallet_metrics(&health, wallet);
                        break true;
                    }
                    Err(error) => {
                        tracing::warn!("BTC sync error: {}", error);
                        connect_filter_peers(&candidates, &mut cursor, &mut peers, network).await;
                        update_peer_group_metric(&health, &peers);
                        if peers.is_empty() {
                            break false;
                        }
                    }
                }
            };
            update_wallet_metrics(&health, wallet);
            let required = if network == bitcoin::Network::Regtest {
                1
            } else {
                2
            };
            if peers.len() < required {
                tracing::warn!(
                    "BTC scan requires {} independent peers; only {} connected",
                    required,
                    peers.len()
                );
            }
            let mut scan_retries = 0;
            while synced && peers.len() >= required {
                let retained_floor = wallet.retained_floor();
                let start = wallet.last_scanned_height().max(retained_floor);
                let tip = wallet.best_height();
                let tip_hash = wallet.best_hash();
                if start > tip {
                    // A covered reorg can land here with nothing above the fork
                    // to scan; that finishes the journal recovery.
                    wallet.finish_recovered_scan();
                    update_wallet_metrics(&health, wallet);
                    health
                        .last_success_unix
                        .store(unix_now(), Ordering::Relaxed);
                    break;
                }
                if wallet.last_scanned_height() < retained_floor {
                    // The floor moved past the resumed cursor while a journal
                    // recovery was pending: the undone range can no longer be
                    // re-derived, so withdraw the exemption and let the history
                    // guard (case 2/3) decide rather than scanning past it.
                    if wallet.abandon_journal_recovery() {
                        tracing::warn!(
                            "BTC journal recovery abandoned: retained floor {} is above the resumed scan cursor; the history guard now decides",
                            retained_floor
                        );
                        update_wallet_metrics(&health, wallet);
                    } else {
                        tracing::warn!(
                            "BTC wallet history before retained floor {} is unavailable; restart with sufficient header retention before scanning",
                            retained_floor
                        );
                    }
                }
                if wallet.last_scanned_height() > wallet.best_height() {
                    tracing::warn!(
                        "BTC wallet birthday {} is above the current header tip {}; no funds will be scanned",
                        wallet.last_scanned_height(),
                        wallet.best_height()
                    );
                }
                health.scan_in_progress.store(true, Ordering::Relaxed);
                let started = Instant::now();
                let result = wallet.scan_utxos_bip158_multi(&mut peers, start).await;
                remember_connected_peers(&peer_cache, peer_cache_path.as_deref(), &peers).await;
                health
                    .scan_duration_ms
                    .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                health.scan_in_progress.store(false, Ordering::Relaxed);
                match result {
                    Ok(0) => {
                        // No blocks to scan finishes a journal recovery too.
                        wallet.finish_recovered_scan();
                        update_wallet_metrics(&health, wallet);
                        health
                            .last_success_unix
                            .store(unix_now(), Ordering::Relaxed);
                        break;
                    }
                    Ok(n) => {
                        health
                            .last_success_unix
                            .store(unix_now(), Ordering::Relaxed);
                        let done = start.saturating_add(n as u32);
                        wallet.set_scan_progress(done, tip_hash);
                        update_wallet_metrics(&health, wallet);
                        tracing::info!(
                            "BTC UTXO scan (BIP-158): {} blocks (through height {})",
                            n,
                            done.saturating_sub(1)
                        );
                        if done > tip {
                            break;
                        }
                    }
                    Err(e) => {
                        update_wallet_metrics(&health, wallet);
                        health.scan_errors.fetch_add(1, Ordering::Relaxed);
                        if e.to_string().contains("agreement from") {
                            health.auth_failures.fetch_add(1, Ordering::Relaxed);
                        }
                        tracing::warn!("BTC UTXO scan error: {}", e);
                        if scan_retries >= 2 {
                            break;
                        }
                        scan_retries += 1;
                        peers.clear();
                        connect_filter_peers(&candidates, &mut cursor, &mut peers, network).await;
                        update_peer_group_metric(&health, &peers);
                        if peers.len() < required {
                            break;
                        }
                        continue;
                    }
                }
            }
        }
        let released = crate::btc_reconciliation::release_expired_reservations(&state).await;
        if released > 0 {
            tracing::info!("Released {} expired BTC input reservation(s)", released);
        }
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    /// A regtest header chain on top of genesis, so the wallet has a non-zero
    /// retained floor.
    fn add_headers(wallet: &BtcWallet, count: u32) {
        use bitcoin::blockdata::block::{Header, Version};
        use bitcoin::hashes::Hash;
        use bitcoin::{BlockHash, CompactTarget, TxMerkleNode};

        let bits = CompactTarget::from_consensus(0x207fffff);
        let mut prev = bitcoin::constants::genesis_block(bitcoin::Network::Regtest)
            .block_hash()
            .to_byte_array();
        for height in 1..=count {
            let mut nonce = height;
            let header = loop {
                let header = Header {
                    version: Version::ONE,
                    prev_blockhash: BlockHash::from_byte_array(prev),
                    merkle_root: TxMerkleNode::all_zeros(),
                    time: 1_700_000_000 + height,
                    bits,
                    nonce,
                };
                if header.validate_pow(bits.into()).is_ok() {
                    break header;
                }
                nonce += 1;
            };
            prev = header.block_hash().to_byte_array();
            wallet
                .add_header(&bitcoin::consensus::encode::serialize(&header), height)
                .unwrap();
        }
    }

    #[tokio::test]
    async fn wallet_metrics_publish_history_blocked_with_an_empty_peer_set() {
        let path = std::env::temp_dir().join(format!(
            "vtr-btc-blocked-wallet-{}-{}.json",
            std::process::id(),
            unix_now()
        ));
        std::fs::write(
            &path,
            r#"{"utxos":[],"birthday":0,"seen_funds":true,"rescan_required":true}"#,
        )
        .unwrap();
        let wallet =
            BtcWallet::with_persistence([1u8; 64], bitcoin::Network::Regtest, path.clone())
                .unwrap();
        add_headers(&wallet, 7);
        wallet.set_header_retention(2);
        assert!(
            wallet.history_unavailable(),
            "precondition: the restored wallet is blocked"
        );

        let state = AppState::new();
        *state.btc_wallet.write().await = Some(wallet);
        assert_eq!(
            state.btc_health.peer_groups.load(Ordering::Relaxed),
            0,
            "no peer was connected"
        );

        publish_wallet_metrics(&state).await;

        assert!(
            state
                .btc_health
                .wallet_history_blocked
                .load(Ordering::Relaxed),
            "the blocked state must be published before peers are resolved"
        );
        tokio::fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn peer_cache_round_trips() {
        let path = std::env::temp_dir().join(format!(
            "vtr-btc-peers-{}-{}.json",
            std::process::id(),
            unix_now()
        ));
        let peers: Vec<SocketAddr> = vec![
            "203.0.113.1:8333".parse().unwrap(),
            "[2001:db8::1]:8333".parse().unwrap(),
        ];
        persist_peer_cache(&path, &peers).await.unwrap();
        assert_eq!(load_peer_cache(Some(&path)).await, peers);
        tokio::fs::remove_file(path).await.unwrap();
    }
}
