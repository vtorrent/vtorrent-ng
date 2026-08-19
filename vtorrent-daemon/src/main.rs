/// vTorrent Daemon — full production node binary.
///
/// Starts three concurrent services:
///   1. P2P node (peer discovery, block sync, PoS staking)
///   2. HTTP JSON-RPC server (localhost:22525 by default)
///   3. Graceful shutdown on Ctrl+C / SIGTERM
///
/// Chain persistence is handled via `vtorrent-store` (redb): every new
/// main-chain block is persisted atomically.  On startup the daemon loads
/// the persisted chain state from disk before starting the node.
///
/// Usage:
///   vtorrent-daemon [OPTIONS]
///
/// Options:
///   --listen <ADDR>           P2P listen address [default: 0.0.0.0:22526]
///   --rpc-addr <ADDR>         RPC server bind address [default: 127.0.0.1:22525]
///   --data-dir <PATH>         Node data directory [default: ~/.vtorrent]
///   --staking-address <ADDR>  Enable PoS staking with this address
///   --no-dht                  Disable DHT bootstrap (use DNS seeds only)
///   --seed <ADDR>             Additional seed node (repeatable)
///   --tor-proxy <ADDR>        Tor SOCKS5 proxy address [default: 127.0.0.1:9050]
///   --tor-only                Prefer Tor for clearnet outbound peers
///   --i2p-sam <ADDR>          Enable I2P through this SAM bridge address
///   --log-level <LEVEL>       Log level: error|warn|info|debug|trace [default: info]
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vtorrent_node::events as node_events;
use vtorrent_node::node::{Node, NodeConfig};
use vtorrent_onion::TransportConfig;
use vtorrent_rpc::ws::NodeEvent as RpcNodeEvent;
use vtorrent_rpc::{server::start_server, state::AppState};
use vtorrent_spv::SpvHeader;
use vtorrent_store::store::BlockStore;

// ─── CLI Arguments ────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "vtorrent-daemon",
    version = "2.0.0",
    about = "vTorrent Daemon — Decentralized • Incentivized • Exchange-Free",
    long_about = None
)]
struct Cli {
    /// P2P listen address.
    #[arg(long, default_value = "0.0.0.0:22526")]
    listen: String,

    /// RPC server bind address (localhost only by default for security).
    #[arg(long, default_value = "127.0.0.1:22525")]
    rpc_addr: String,

    /// Node data directory (peer cache, chain data, wallet).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Enable PoS staking with this wallet address.
    #[arg(long)]
    staking_address: Option<String>,

    /// WIF-encoded private key used to sign coinstake inputs.
    ///
    /// Required for staking; without it the node can find stake kernels but
    /// the resulting blocks would be rejected by the chain's script
    /// verification.
    #[arg(long, env = "VTORRENT_STAKING_WIF")]
    staking_wif: Option<String>,

    /// Disable DHT bootstrap (use DNS seeds only).
    #[arg(long, default_value_t = false)]
    no_dht: bool,

    /// Additional seed nodes to connect to (can be repeated).
    #[arg(long = "seed", value_name = "ADDR")]
    seeds: Vec<String>,

    /// Tor SOCKS5 proxy address. Tor remains optional unless an onion peer is dialed.
    #[arg(long, value_name = "ADDR")]
    tor_proxy: Option<String>,

    /// Prefer Tor for outbound clearnet peers when the proxy is available.
    #[arg(long, default_value_t = false)]
    tor_only: bool,

    /// Enable I2P using this SAM bridge address (for example, 127.0.0.1:7656).
    #[arg(long, value_name = "ADDR")]
    i2p_sam: Option<String>,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Run in testnet mode.
    ///
    /// Enables private/RFC1918 address acceptance in PEX, allowing multi-node
    /// testing on a single machine or LAN without public IP addresses.
    #[arg(long, default_value_t = false)]
    testnet: bool,

    /// Run in regtest mode (local development).
    ///
    /// Enables a faucet RPC endpoint that mints coins to arbitrary addresses,
    /// so the full wallet/DEX/swap flow can be exercised locally without real
    /// coins or a legacy claim.
    #[arg(long, default_value_t = false)]
    regtest: bool,

    /// Optional API key required for sensitive RPC endpoints.
    ///
    /// When set, wallet, staking, torrent, DEX, claim and broadcast endpoints
    /// reject requests that do not include the matching `X-API-Key` header.
    /// Read-only info endpoints remain open.
    #[arg(long, env = "VTORRENT_RPC_API_KEY")]
    rpc_api_key: Option<String>,

    /// 64-byte hex-encoded BIP39 seed for the Bitcoin SPV wallet.
    ///
    /// Required for cross-chain atomic swaps: the BTC side of an HTLC is
    /// funded, claimed, and refunded with keys derived from this seed. Without
    /// it the BTC wallet stays uninitialized and swap settlement is disabled.
    #[arg(long, env = "VTORRENT_BTC_SEED")]
    btc_seed: Option<String>,
}

// ─── Entry Point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialise structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .init();

    print_banner();

    // ── Resolve data directory ────────────────────────────────────────────────
    let data_dir = cli.data_dir.unwrap_or_else(|| {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".vtorrent")
    });

    std::fs::create_dir_all(&data_dir)?;

    // ── Open (or create) the persistent block store ───────────────────────────
    let chain_db_path = data_dir.join("chain.db");
    let block_store = Arc::new(BlockStore::open(&chain_db_path).map_err(|e| {
        anyhow::anyhow!("Failed to open block store at {:?}: {}", chain_db_path, e)
    })?);
    tracing::info!("Block store opened at {:?}", chain_db_path);

    // ── Build NodeConfig ──────────────────────────────────────────────────────
    let staking_enabled = cli.staking_address.is_some();
    let mut transport = TransportConfig::default();
    if let Some(proxy) = &cli.tor_proxy {
        transport.tor_socks_addr = proxy.clone();
    }
    transport.prefer_onion = cli.tor_only;
    if let Some(sam_addr) = &cli.i2p_sam {
        transport.i2p_enabled = true;
        transport.i2p_sam_addr = sam_addr.clone();
    }
    let config = NodeConfig {
        listen_addr: cli.listen.clone(),
        staking_enabled,
        staking_address: cli.staking_address.clone(),
        staking_wif: cli.staking_wif.clone(),
        max_mempool: 10_000,
        extra_seeds: cli.seeds.clone(),
        use_dht: !cli.no_dht,
        data_dir: data_dir.clone(),
        use_overlay: true,
        testnet: cli.testnet,
        regtest: cli.regtest,
        transport,
    };

    // ── Build the P2P Node ────────────────────────────────────────────────────
    //
    // If the block store has persisted blocks, load them into the chain first
    // so the node resumes from the last known tip rather than re-syncing from
    // genesis on every restart.
    let mut node = {
        let store_height = block_store
            .best_height()
            .map_err(|e| anyhow::anyhow!("BlockStore::best_height failed: {}", e))?;

        if store_height > 0 {
            tracing::info!("Resuming from persisted chain at height {}", store_height);
            // Load persisted chain into memory, then build the node with it.
            let chain = block_store
                .load_into_chain()
                .map_err(|e| anyhow::anyhow!("Failed to load chain from store: {}", e))?;
            Node::new_with_chain(config.clone(), chain)
                .map_err(|e| anyhow::anyhow!("Node::new_with_chain failed: {}", e))?
        } else {
            tracing::info!("No persisted chain found — starting from genesis");
            Node::new(config.clone()).map_err(|e| anyhow::anyhow!("Node::new failed: {}", e))?
        }
    };

    // ── Build RPC AppState ────────────────────────────────────────────────────
    // Share the live chain and mempool Arcs from the node so that RPC
    // responses always reflect the current chain state.
    let chain_arc = node.chain_arc();
    let mempool_arc = node.mempool_arc();
    let tx_submit_sender = node.tx_submit_sender();
    let mut rpc_state = AppState::new_with_shared(chain_arc, mempool_arc);
    // Wire the tx broadcast channel so RPC wallet can push txs into the P2P loop.
    rpc_state.tx_submit = Some(tx_submit_sender);
    rpc_state.rpc_api_key = cli.rpc_api_key.clone();
    rpc_state.regtest = cli.regtest;
    let rpc_addr = cli.rpc_addr.clone();

    // Set the torrent download directory under the data dir.
    *rpc_state.download_dir.write().await = data_dir.join("downloads");

    // Initialize the Bitcoin SPV wallet from the seed, if provided.
    if let Some(seed_hex) = &cli.btc_seed {
        let seed_bytes = hex::decode(seed_hex)
            .map_err(|e| anyhow::anyhow!("Invalid --btc-seed (expected 64-byte hex): {}", e))?;
        let seed: [u8; 64] = seed_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("--btc-seed must be exactly 64 bytes (128 hex chars)"))?;
        *rpc_state.btc_wallet.write().await = Some(vtorrent_btc::wallet::BtcWallet::new(seed));
        tracing::info!("Bitcoin SPV wallet initialized from --btc-seed");
    }

    // Share the DEX order book between the node (for gossip) and RPC (for the
    // order-book API), so received orders are visible to both.
    node.set_order_book(Arc::clone(&rpc_state.order_book));

    // ── Wire node events → RPC WebSocket broadcaster + BlockStore ────────────
    //
    // The node emits `vtorrent_node::events::NodeEvent` values.
    // The RPC WebSocket layer consumes `vtorrent_rpc::ws::NodeEvent` values.
    // We bridge them here in the daemon — the only place that knows about both
    // crates — to avoid a circular dependency.
    //
    // Additionally, every `NewBlock` event triggers an atomic `BlockStore::append_block`
    // call to persist the block to disk.
    {
        let (node_tx, mut node_rx) = node_events::channel(1024);
        node.set_event_sender(node_tx);

        let rpc_broadcaster = rpc_state.events.clone();
        let store_for_bridge = Arc::clone(&block_store);
        let best_peer_height_ref = Arc::clone(&rpc_state.best_peer_height);
        let peer_count_ref = Arc::clone(&rpc_state.peer_count);
        let syncing_ref = Arc::clone(&rpc_state.syncing);
        let spv_chain_ref = Arc::clone(&rpc_state.spv_chain);
        let peer_list_ref = Arc::clone(&rpc_state.peer_list);

        tokio::spawn(async move {
            loop {
                match node_rx.recv().await {
                    Ok(event) => {
                        // ── Persist new main-chain blocks ─────────────────────
                        if let node_events::NodeEvent::NewBlock {
                            height,
                            block,
                            utxos_added,
                            utxos_removed,
                            claimed_addresses,
                            ..
                        } = &*event
                        {
                            if let Err(e) = store_for_bridge.append_block(
                                block,
                                *height,
                                utxos_added,
                                utxos_removed,
                                claimed_addresses,
                            ) {
                                tracing::error!(
                                    "BlockStore::append_block failed at height {}: {}",
                                    height,
                                    e
                                );
                            } else {
                                tracing::debug!("Persisted block at height {}", height);
                            }
                            // ── Auto-feed SPV chain ───────────────────────────
                            let spv_header = SpvHeader {
                                version: block.header.version,
                                prev_hash: block.header.prev_block_hash,
                                merkle_root: block.header.merkle_root,
                                timestamp: block.header.timestamp,
                                bits: block.header.bits,
                                nonce: block.header.nonce,
                                height: *height,
                            };
                            {
                                let mut spv = spv_chain_ref.write().await;
                                if let Err(e) = spv.add_header(spv_header) {
                                    tracing::debug!(
                                        "SPV chain: could not add header at {}: {}",
                                        height,
                                        e
                                    );
                                }
                            }
                        }

                        // ── Bridge to RPC WebSocket broadcaster ───────────────
                        let rpc_event: Option<RpcNodeEvent> = match &*event {
                            node_events::NodeEvent::NewBlock {
                                height,
                                hash,
                                tx_count,
                                timestamp,
                                size_bytes,
                                ..
                            } => Some(RpcNodeEvent::NewBlock {
                                height: *height,
                                hash: hex::encode(hash),
                                tx_count: *tx_count,
                                timestamp: *timestamp,
                                size_bytes: *size_bytes,
                            }),
                            node_events::NodeEvent::TxConfirmed {
                                txid,
                                block_height,
                                block_hash,
                            } => Some(RpcNodeEvent::TxConfirmed {
                                txid: hex::encode(txid),
                                block_height: *block_height,
                                block_hash: hex::encode(block_hash),
                            }),
                            node_events::NodeEvent::TxUnconfirmed { txid, fee_sats } => {
                                Some(RpcNodeEvent::TxUnconfirmed {
                                    txid: hex::encode(txid),
                                    fee_sats: *fee_sats,
                                    fee_rate: 0.0,
                                    size_bytes: 0,
                                })
                            }
                            node_events::NodeEvent::PeerConnected {
                                addr,
                                user_agent,
                                version,
                                height,
                            } => {
                                // Update peer count and best known peer height for sync % calculation.
                                {
                                    let mut count = peer_count_ref.write().await;
                                    *count += 1;
                                }
                                {
                                    let mut best = best_peer_height_ref.write().await;
                                    let h = u64::from(*height);
                                    if h > *best {
                                        *best = h;
                                    }
                                }
                                // Update live peer list.
                                {
                                    use vtorrent_rpc::state::PeerInfo;
                                    let mut peers = peer_list_ref.write().await;
                                    if !peers.iter().any(|p| p.addr == addr.to_string()) {
                                        peers.push(PeerInfo {
                                            addr: addr.to_string(),
                                            user_agent: user_agent.clone(),
                                            services: 0,
                                            best_height: *height,
                                        });
                                    }
                                }
                                Some(RpcNodeEvent::PeerConnected {
                                    addr: addr.to_string(),
                                    version: *version,
                                    user_agent: user_agent.clone(),
                                    height: *height,
                                })
                            }
                            node_events::NodeEvent::PeerDisconnected { addr } => {
                                // Decrement peer count.
                                {
                                    let mut count = peer_count_ref.write().await;
                                    *count = count.saturating_sub(1);
                                    // If no peers remain, mark as syncing until reconnected.
                                    if *count == 0 {
                                        let mut syncing = syncing_ref.write().await;
                                        *syncing = true;
                                    }
                                }
                                // Remove from live peer list.
                                {
                                    let mut peers = peer_list_ref.write().await;
                                    peers.retain(|p| p.addr != addr.to_string());
                                }
                                Some(RpcNodeEvent::PeerDisconnected {
                                    addr: addr.to_string(),
                                    reason: "disconnected".to_string(),
                                })
                            }
                            node_events::NodeEvent::Reorg {
                                old_tip,
                                new_tip,
                                depth,
                            } => Some(RpcNodeEvent::Reorg {
                                old_tip: hex::encode(old_tip),
                                new_tip: hex::encode(new_tip),
                                depth: *depth,
                            }),
                            node_events::NodeEvent::StakingReward {
                                block_height,
                                reward_sats,
                                address,
                            } => Some(RpcNodeEvent::StakingReward {
                                block_height: *block_height,
                                reward_sats: *reward_sats,
                                address: address.clone(),
                            }),
                        };

                        if let Some(ev) = rpc_event {
                            rpc_broadcaster.broadcast(ev);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Event bridge lagged, skipped {} events", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("Node event channel closed — event bridge stopping");
                        break;
                    }
                }
            }
        });
    }

    tracing::info!("vTorrent daemon starting:");
    tracing::info!("  P2P listen:      {}", config.listen_addr);
    tracing::info!("  RPC server:      {}", rpc_addr);
    tracing::info!("  Data dir:        {}", data_dir.display());
    tracing::info!(
        "  DHT bootstrap:   {}",
        if config.use_dht {
            "enabled"
        } else {
            "disabled"
        }
    );
    tracing::info!(
        "  Network:         {}",
        if config.testnet { "TESTNET" } else { "mainnet" }
    );
    tracing::info!(
        "  Staking:         {}",
        if staking_enabled {
            cli.staking_address.as_deref().unwrap_or("enabled")
        } else {
            "disabled"
        }
    );

    // ── Start services concurrently ───────────────────────────────────────────

    // Periodic DEX order expiry maintenance — runs every 60 seconds.
    let order_book_for_maintenance = Arc::clone(&rpc_state.order_book);
    let swaps_for_maintenance = Arc::clone(&rpc_state.swaps);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let expired = order_book_for_maintenance.write().await.expire_orders();
            if expired > 0 {
                tracing::info!("DEX maintenance: expired {} stale orders", expired);
            }
            // Sweep expired swaps to Refunded.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u32;
            let mut swaps = swaps_for_maintenance.write().await;
            let mut swept = 0;
            for (id, swap) in swaps.iter_mut() {
                if swap.status == vtorrent_node::atomic_swap::SwapStatus::BtcFunded {
                    if let Some(order) = order_book_for_maintenance.read().await.get_order(id) {
                        if now >= order.expiry {
                            swap.status = vtorrent_node::atomic_swap::SwapStatus::Refunded;
                            swept += 1;
                        }
                    }
                }
            }
            if swept > 0 {
                tracing::info!("Swap maintenance: refunded {} expired swaps", swept);
            }
        }
    });

    // Payment channel: the torrent engine emits PaymentDue events; this task
    // builds and broadcasts the actual VTR transactions.
    let (payment_sender, mut payment_receiver) =
        vtorrent_torrent::payment::PaymentSender::channel();
    let payment_wif = Arc::clone(&rpc_state.wallet_wif);
    let payment_change = Arc::clone(&rpc_state.wallet_change_address);
    let payment_chain = Arc::clone(&rpc_state.chain);
    let payment_mempool = Arc::clone(&rpc_state.mempool);
    let payment_tx_submit = rpc_state.tx_submit.clone();
    tokio::spawn(async move {
        while let Some(payment) = payment_receiver.recv().await {
            let result = build_incentive_payment(
                &payment_wif,
                &payment_change,
                &payment_chain,
                &payment_mempool,
                &payment_tx_submit,
                &payment,
            )
            .await;
            match result {
                Ok(txid) => tracing::info!(
                    "Incentive payment {} sent to {}",
                    txid,
                    payment.peer_address
                ),
                Err(e) => tracing::warn!("Incentive payment failed: {}", e),
            }
        }
    });

    // Periodic torrent incentive settlement — runs every 5 minutes.
    let torrent_sessions_for_settlement = Arc::clone(&rpc_state.torrent_sessions);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
            vtorrent_torrent::incentive::PAYMENT_INTERVAL_SECS,
        ));
        loop {
            interval.tick().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut guard = torrent_sessions_for_settlement.write().await;
            let mut settled = 0;
            for session in guard.sessions_mut() {
                for account in session.incentive_accounts.values_mut() {
                    if account.needs_settlement(now) {
                        let (_earned, owed) = account.settle(now);
                        if owed > 0 && !account.peer_address.is_empty() {
                            payment_sender.emit(vtorrent_torrent::payment::PaymentDue {
                                peer_address: account.peer_address.clone(),
                                amount_satoshis: owed,
                            });
                        }
                        settled += 1;
                    }
                }
            }
            if settled > 0 {
                tracing::info!("Torrent incentive: settled {} peer accounts", settled);
            }
        }
    });

    // Bitcoin SPV sync — resolves DNS seeds and syncs headers in a loop.
    let btc_wallet = Arc::clone(&rpc_state.btc_wallet);
    tokio::spawn(async move {
        tracing::info!("Bitcoin SPV sync task started");
        loop {
            let has_wallet = btc_wallet.read().await.is_some();
            if !has_wallet {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                continue;
            }
            match vtorrent_btc::sync::resolve_seeds().await {
                Ok(addrs) => {
                    for addr in addrs {
                        match vtorrent_btc::p2p::BtcPeer::connect(addr).await {
                            Ok(mut peer) => {
                                if let Some(w) = btc_wallet.write().await.as_mut() {
                                    match w.sync(&mut peer).await {
                                        Ok(n) => tracing::info!("BTC sync: {} headers", n),
                                        Err(e) => tracing::warn!("BTC sync error: {}", e),
                                    }
                                    // After header sync, scan for wallet UTXOs
                                    // from the last checkpoint to the tip.
                                    let start = w.last_scanned_height();
                                    match w.scan_utxos(&mut peer, start).await {
                                        Ok(n) => {
                                            tracing::info!("BTC UTXO scan: {} blocks", n);
                                            let tip = w.best_height();
                                            w.set_last_scanned_height(tip);
                                        }
                                        Err(e) => tracing::warn!("BTC UTXO scan error: {}", e),
                                    }
                                }
                            }
                            Err(e) => tracing::warn!("BTC peer {} failed: {}", addr, e),
                        }
                    }
                }
                Err(e) => tracing::warn!("BTC seed resolution failed: {}", e),
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });

    let rpc_handle = tokio::spawn(async move {
        tracing::info!("RPC server starting on {}", rpc_addr);
        if let Err(e) = start_server(&rpc_addr, rpc_state).await {
            tracing::error!("RPC server error: {}", e);
        }
    });

    let node_handle = tokio::spawn(async move {
        tracing::info!("P2P node starting...");
        if let Err(e) = node.start().await {
            tracing::error!("P2P node error: {}", e);
        }
    });

    // Wait for shutdown signal or unexpected service exit
    tokio::select! {
        _ = rpc_handle => {
            tracing::error!("RPC server exited unexpectedly");
        }
        _ = node_handle => {
            tracing::error!("P2P node exited unexpectedly");
        }
        _ = shutdown_signal() => {
            tracing::info!("Shutdown signal received — stopping daemon");
        }
    }

    tracing::info!("vTorrent daemon stopped.");
    Ok(())
}

/// Wait for Ctrl+C or SIGTERM.
/// Build and broadcast a VTR payment for an incentive settlement.
async fn build_incentive_payment(
    wallet_wif: &Arc<tokio::sync::RwLock<Option<String>>>,
    wallet_change_address: &Arc<tokio::sync::RwLock<Option<String>>>,
    chain: &Arc<tokio::sync::Mutex<vtorrent_node::chain::Chain>>,
    mempool: &Arc<tokio::sync::Mutex<vtorrent_node::mempool::Mempool>>,
    tx_submit: &Option<tokio::sync::mpsc::Sender<vtorrent_node::block::Transaction>>,
    payment: &vtorrent_torrent::payment::PaymentDue,
) -> anyhow::Result<String> {
    use vtorrent_wallet::tx_builder::TxBuilder;

    let wif = wallet_wif
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("wallet not unlocked"))?;
    let change_address = wallet_change_address
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("change address not set"))?;

    let utxos: Vec<vtorrent_node::chain::Utxo> = {
        let chain = chain.lock().await;
        chain.get_utxos_for_address(&change_address)
    };
    if utxos.is_empty() {
        return Err(anyhow::anyhow!("no UTXOs available"));
    }

    let tx = TxBuilder::new()
        .recipient(&payment.peer_address, payment.amount_satoshis)
        .change_address(&change_address)
        .fee_rate(10)
        .sign_with_wif(&wif)
        .build(&utxos)
        .map_err(|e| anyhow::anyhow!("tx build failed: {}", e))?;

    let txid = hex::encode(tx.txid());
    {
        let mut mempool = mempool.lock().await;
        mempool
            .add_transaction(tx.clone())
            .map_err(|e| anyhow::anyhow!("mempool rejected: {}", e))?;
    }
    if let Some(ref sender) = tx_submit {
        let _ = sender.try_send(tx);
    }
    Ok(txid)
}

async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn print_banner() {
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║              vTorrent Daemon v2.0.0                      ║");
    println!("║  Decentralized • Incentivized • Exchange-Free            ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();
}
