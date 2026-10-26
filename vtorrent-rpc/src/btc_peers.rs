use crate::error::{RpcError, RpcResult};
use crate::state::AppState;
use std::net::SocketAddr;
use std::time::Duration;

const DNS_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn prioritize_peers(
    configured: Vec<SocketAddr>,
    cached: Vec<SocketAddr>,
    mut discovered: Vec<SocketAddr>,
) -> Vec<SocketAddr> {
    discovered.sort_by_key(|addr| (addr.is_ipv6(), *addr));
    discovered.dedup();
    let mut ordered = Vec::new();
    for addr in configured.into_iter().chain(cached).chain(discovered) {
        if !ordered.contains(&addr) {
            ordered.push(addr);
        }
    }
    ordered
}

pub(crate) async fn resolve_btc_peers(
    state: &AppState,
    network: bitcoin::Network,
) -> RpcResult<Vec<SocketAddr>> {
    resolve_with_seeds(state, network, async || {
        tokio::time::timeout(DNS_TIMEOUT, vtorrent_btc::sync::resolve_seeds())
            .await
            .map_err(|_| "BTC seed resolution timed out".to_string())?
            .map_err(|error| error.to_string())
    })
    .await
}

async fn resolve_with_seeds(
    state: &AppState,
    network: bitcoin::Network,
    seeds: impl AsyncFnOnce() -> Result<Vec<SocketAddr>, String>,
) -> RpcResult<Vec<SocketAddr>> {
    let host = state.btc_peer.read().await.clone();
    if network != bitcoin::Network::Bitcoin && host.is_none() {
        return Err(RpcError::BadRequest(
            "Configure a BTC peer for this network".into(),
        ));
    }
    let configured = if let Some(host) = host {
        if let Ok(addr) = host.parse() {
            vec![addr]
        } else {
            match tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host(&host)).await {
                Ok(Ok(addresses)) => addresses.collect(),
                Ok(Err(error)) => {
                    tracing::warn!("BTC peer {} unresolved: {}", host, error);
                    Vec::new()
                }
                Err(_) => {
                    tracing::warn!("BTC peer {} resolution timed out", host);
                    Vec::new()
                }
            }
        }
    } else {
        Vec::new()
    };
    if network != bitcoin::Network::Bitcoin {
        return if configured.is_empty() {
            Err(RpcError::BadRequest("BTC peer resolution failed".into()))
        } else {
            Ok(configured)
        };
    }
    let cached = state.btc_peer_cache.read().await.clone();
    let discovered = match seeds().await {
        Ok(addresses) => addresses,
        Err(error) => {
            tracing::warn!(
                "BTC seed resolution failed; using configured/cached peers: {}",
                error
            );
            Vec::new()
        }
    };
    let peers = prioritize_peers(configured, cached, discovered);
    if peers.is_empty() {
        return Err(RpcError::BadRequest("No BTC peers resolved".into()));
    }
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use vtorrent_btc::sync::network_group;

    #[tokio::test]
    async fn mainnet_configured_peer_is_supplemented_by_cache_and_seeds() {
        let state = AppState::new();
        *state.btc_peer.write().await = Some("203.0.113.1:8333".into());
        let cached = "198.51.100.2:8333".parse().unwrap();
        let seed = "192.0.2.3:8333".parse().unwrap();
        *state.btc_peer_cache.write().await = vec![cached];
        let peers = resolve_with_seeds(&state, bitcoin::Network::Bitcoin, async || Ok(vec![seed]))
            .await
            .unwrap();
        assert_eq!(
            peers,
            vec!["203.0.113.1:8333".parse().unwrap(), cached, seed]
        );
        assert_eq!(
            peers
                .iter()
                .map(|peer| network_group(peer.ip()))
                .collect::<HashSet<_>>()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn cached_mainnet_peers_survive_dns_failure() {
        let state = AppState::new();
        let cached = "198.51.100.2:8333".parse().unwrap();
        *state.btc_peer_cache.write().await = vec![cached];
        let peers = resolve_with_seeds(&state, bitcoin::Network::Bitcoin, async || {
            Err("offline".into())
        })
        .await
        .unwrap();
        assert_eq!(peers, vec![cached]);
    }

    #[tokio::test]
    async fn non_mainnet_requires_explicit_peer_and_never_uses_mainnet_seeds() {
        let state = AppState::new();
        assert!(
            resolve_with_seeds(&state, bitcoin::Network::Regtest, async || panic!("no DNS"))
                .await
                .is_err()
        );
        *state.btc_peer.write().await = Some("127.0.0.1:18444".into());
        let peers =
            resolve_with_seeds(&state, bitcoin::Network::Regtest, async || panic!("no DNS"))
                .await
                .unwrap();
        assert_eq!(peers, vec!["127.0.0.1:18444".parse().unwrap()]);
    }

    #[test]
    fn configured_and_cached_peers_precede_dns_and_are_deduplicated() {
        let configured = "[2001:db8::1]:8333".parse().unwrap();
        let cached = "203.0.113.1:8333".parse().unwrap();
        let seed = "198.51.100.1:8333".parse().unwrap();
        assert_eq!(
            prioritize_peers(vec![configured], vec![cached], vec![cached, seed]),
            vec![configured, cached, seed]
        );
        assert_eq!(
            prioritize_peers(vec![], vec![], vec![configured, cached, cached]),
            vec![cached, configured]
        );
    }

    #[test]
    fn configured_peer_survives_attempt_limit() {
        let configured = "198.51.100.250:8333".parse().unwrap();
        let seeds = (1..=65)
            .map(|last| SocketAddr::from(([10, 0, 0, last], 8333)))
            .collect();
        let ordered = prioritize_peers(vec![configured], vec![configured], seeds);
        assert_eq!(ordered[0], configured);
        assert_eq!(
            ordered.iter().filter(|peer| **peer == configured).count(),
            1
        );
        assert!(ordered.into_iter().take(64).any(|peer| peer == configured));
    }
}
