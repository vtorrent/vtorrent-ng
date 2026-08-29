use super::Node;
use vtorrent_p2p::dht::{discover_peers_via_doh, discover_peers_via_github, DhtBootstrap};
use vtorrent_p2p::peer_manager::{DEFAULT_PORT, TARGET_OUTBOUND};

pub(crate) async fn bootstrap_via_dht(node: &mut Node) {
    tracing::info!("Starting parallel DHT + Cloudflare DoH bootstrap...");

    let port = node
        .config
        .listen_addr
        .split(':')
        .next_back()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    let dht_task = tokio::task::spawn_blocking(move || {
        let dht = DhtBootstrap::new();
        dht.discover_peers()
    });

    let doh_task = tokio::task::spawn_blocking(move || discover_peers_via_doh(port));

    let (dht_peers, doh_peers) = tokio::join!(dht_task, doh_task);

    let dht_peers = dht_peers.unwrap_or_default();
    let doh_peers = doh_peers.unwrap_or_default();

    tracing::info!(
        "Bootstrap complete: DHT={} candidates, DoH={} candidates",
        dht_peers.len(),
        doh_peers.len()
    );

    if !dht_peers.is_empty() {
        node.peer_manager.add_dht_peers(dht_peers);
    }
    if !doh_peers.is_empty() {
        node.peer_manager.add_dht_peers(doh_peers);
    }

    if node.peer_manager.addr_book.is_empty() {
        tracing::warn!("Both DHT and DoH bootstrap returned no peers");
        return;
    }

    let candidates = node.peer_manager.get_peer_candidates(TARGET_OUTBOUND);
    for addr in candidates {
        tracing::info!("Bootstrap: Connecting to peer candidate {}", addr);
        if let Err(e) = node.peer_manager.connect_addr(addr).await {
            tracing::debug!("Bootstrap: Could not connect to {}: {}", addr, e);
        }
    }
}

pub(crate) async fn dht_announce(node: &Node) {
    let port = node
        .config
        .listen_addr
        .split(':')
        .next_back()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    let dht = DhtBootstrap::new();
    tokio::task::spawn_blocking(move || {
        dht.announce(port);
    });
}

pub(crate) async fn connect_to_extra_seeds(node: &mut Node) {
    for seed in node.config.extra_seeds.clone() {
        tracing::info!("Connecting to extra seed: {}", seed);
        if let Err(e) = node.peer_manager.connect(&seed).await {
            tracing::debug!("Could not connect to {}: {}", seed, e);
        }
    }
}

pub(crate) async fn bootstrap_via_github(node: &mut Node) {
    let port = node
        .config
        .listen_addr
        .split(':')
        .next_back()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    let peers = tokio::task::spawn_blocking(move || discover_peers_via_github(port))
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("GitHub bootstrap task failed: {}", e);
            Vec::new()
        });

    if peers.is_empty() {
        tracing::debug!("GitHub bootstrap: no peers returned");
        return;
    }

    tracing::info!("GitHub bootstrap: {} peer candidates found", peers.len());
    node.peer_manager.add_dht_peers(peers);

    let candidates = node.peer_manager.get_peer_candidates(TARGET_OUTBOUND);
    for addr in candidates {
        tracing::info!("GitHub: Connecting to peer candidate {}", addr);
        if let Err(e) = node.peer_manager.connect_addr(addr).await {
            tracing::debug!("GitHub: Could not connect to {}: {}", addr, e);
        }
    }
}

pub(crate) async fn connect_to_dns_seeds(node: &mut Node) {
    use vtorrent_p2p::peer_manager::DNS_SEEDS;
    let seeds: Vec<String> = DNS_SEEDS
        .iter()
        .map(|s| format!("{}:{}", s, DEFAULT_PORT))
        .collect();

    for seed in seeds {
        tracing::info!("Connecting to DNS seed (fallback): {}", seed);
        if let Err(e) = node.peer_manager.connect(&seed).await {
            tracing::debug!("Could not connect to {}: {}", seed, e);
        }
    }
}
