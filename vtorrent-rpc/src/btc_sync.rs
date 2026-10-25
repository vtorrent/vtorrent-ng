//! Shared Bitcoin SPV sync loop for the daemon and the desktop node.

use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::state::{AppState, BtcSyncHealth};
use vtorrent_btc::{p2p::BtcPeer, sync::network_group, wallet::BtcWallet};

fn prioritize_peers(
    configured: Option<SocketAddr>,
    mut discovered: Vec<SocketAddr>,
) -> Vec<SocketAddr> {
    discovered.sort_by_key(|addr| (addr.is_ipv6(), *addr));
    discovered.dedup();
    if let Some(configured) = configured {
        discovered.retain(|addr| *addr != configured);
        discovered.insert(0, configured);
    }
    discovered
}

async fn connect_filter_peers(
    candidates: &[SocketAddr],
    cursor: &mut usize,
    peers: &mut Vec<BtcPeer>,
    network: bitcoin::Network,
) {
    let target = if network == bitcoin::Network::Regtest {
        1
    } else {
        5
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
    health.synced.store(wallet.synced(), Ordering::Relaxed);
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn resolve_addr(peer: &str) -> std::io::Result<SocketAddr> {
    if let Ok(addr) = peer.parse() {
        return Ok(addr);
    }
    tokio::net::lookup_host(peer)
        .await?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses"))
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
        let network = *state.btc_network.read().await;
        let health = state.btc_health.clone();
        let configured = match state.btc_peer.read().await.clone() {
            Some(host) => match resolve_addr(&host).await {
                Ok(addr) => Some(addr),
                Err(e) => {
                    tracing::warn!("BTC peer {} unresolved: {}", host, e);
                    None
                }
            },
            None => None,
        };
        let discovered = if network == bitcoin::Network::Bitcoin {
            match vtorrent_btc::sync::resolve_seeds().await {
                Ok(seeds) => seeds,
                Err(error) => {
                    tracing::warn!("BTC seed resolution failed: {}", error);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let mut candidates = prioritize_peers(configured, discovered);
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
                if start > tip {
                    update_wallet_metrics(&health, wallet);
                    health
                        .last_success_unix
                        .store(unix_now(), Ordering::Relaxed);
                    break;
                }
                if wallet.last_scanned_height() < retained_floor {
                    tracing::warn!(
                        "BTC wallet scan starts at retained floor {}; older history is unavailable. Set --btc-birthday to the wallet creation height to scan older funds.",
                        retained_floor
                    );
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
                health
                    .scan_duration_ms
                    .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                health.scan_in_progress.store(false, Ordering::Relaxed);
                match result {
                    Ok(0) => {
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
                        wallet.set_last_scanned_height(done);
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

    #[test]
    fn configured_peer_precedes_dns_candidates_and_attempt_limit() {
        let configured = "198.51.100.250:8333".parse().unwrap();
        let mut seeds: Vec<SocketAddr> = (1..=65)
            .map(|last| SocketAddr::from(([10, 0, 0, last], 8333)))
            .collect();
        seeds.extend([configured, configured]);
        let ordered = prioritize_peers(Some(configured), seeds);
        assert_eq!(ordered[0], configured);
        assert_eq!(
            ordered.iter().filter(|addr| **addr == configured).count(),
            1
        );
        assert!(ordered.into_iter().take(64).any(|addr| addr == configured));
    }

    #[test]
    fn dns_candidates_prefer_ipv4_and_remove_duplicates() {
        let v4 = "203.0.113.1:8333".parse().unwrap();
        let v6 = "[2001:db8::1]:8333".parse().unwrap();
        assert_eq!(prioritize_peers(None, vec![v6, v4, v4]), vec![v4, v6]);
        assert_eq!(prioritize_peers(Some(v6), vec![v6, v4]), vec![v6, v4]);
    }
}
