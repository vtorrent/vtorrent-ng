//! Shared Bitcoin SPV sync loop for the daemon and the desktop node.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use crate::state::AppState;

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
        let addrs: Vec<SocketAddr> = match state.btc_peer.read().await.clone() {
            Some(host) => match resolve_addr(&host).await {
                Ok(addr) => vec![addr],
                Err(e) => {
                    tracing::warn!("BTC peer {} unresolved: {}", host, e);
                    tokio::time::sleep(Duration::from_secs(300)).await;
                    continue;
                }
            },
            None => match vtorrent_btc::sync::resolve_seeds().await {
                Ok(addrs) => addrs,
                Err(e) => {
                    tracing::warn!("BTC seed resolution failed: {}", e);
                    tokio::time::sleep(Duration::from_secs(300)).await;
                    continue;
                }
            },
        };
        for addr in addrs {
            tracing::debug!("BTC sync: connecting to {} (network {:?})", addr, network);
            match vtorrent_btc::p2p::BtcPeer::connect_with_network(addr, network).await {
                Ok(mut peer) => {
                    if let Some(wallet) = state.btc_wallet.write().await.as_mut() {
                        match wallet.sync(&mut peer).await {
                            Ok(n) => tracing::info!("BTC sync: {} headers", n),
                            Err(e) => tracing::warn!("BTC sync error: {}", e),
                        }
                        loop {
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
                            match wallet.scan_utxos_bip158(&mut peer, start).await {
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
                }
                Err(e) => tracing::warn!("BTC peer {} failed: {}", addr, e),
            }
        }
        let released = crate::btc_reconciliation::release_expired_reservations(&state).await;
        if released > 0 {
            tracing::info!("Released {} expired BTC input reservation(s)", released);
        }
        tokio::time::sleep(Duration::from_secs(300)).await;
    }
}
