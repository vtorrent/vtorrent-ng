//! Shared Bitcoin SPV sync loop for the daemon and the desktop node.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use crate::state::AppState;
use vtorrent_btc::{p2p::BtcPeer, sync::network_group};

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
    candidates: &mut impl Iterator<Item = SocketAddr>,
    peers: &mut Vec<BtcPeer>,
    network: bitcoin::Network,
) {
    let target = if network == bitcoin::Network::Regtest {
        1
    } else {
        3
    };
    while peers.len() < target {
        let Some(addr) = candidates.next() else { break };
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
        let mut candidates = prioritize_peers(configured, discovered)
            .into_iter()
            .take(64);

        let mut peers = Vec::new();
        connect_filter_peers(&mut candidates, &mut peers, network).await;
        if peers.is_empty() {
            tracing::warn!("No usable BTC peers; retrying in 300s");
            tokio::time::sleep(Duration::from_secs(300)).await;
            continue;
        }
        if let Some(wallet) = state.btc_wallet.write().await.as_mut() {
            let synced = loop {
                match wallet.sync_with_peers(&mut peers).await {
                    Ok(n) => {
                        tracing::info!("BTC sync: {} headers", n);
                        connect_filter_peers(&mut candidates, &mut peers, network).await;
                        break true;
                    }
                    Err(error) => {
                        tracing::warn!("BTC sync error: {}", error);
                        connect_filter_peers(&mut candidates, &mut peers, network).await;
                        if peers.is_empty() {
                            break false;
                        }
                    }
                }
            };
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
            while synced && peers.len() >= required {
                let retained_floor = wallet.retained_floor();
                let start = wallet.last_scanned_height().max(retained_floor);
                let tip = wallet.best_height();
                if start > tip {
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
                match wallet.scan_utxos_bip158_multi(&mut peers, start).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let done = start.saturating_add(n as u32);
                        wallet.set_last_scanned_height(done);
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
                        tracing::warn!("BTC UTXO scan error: {}", e);
                        break;
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
