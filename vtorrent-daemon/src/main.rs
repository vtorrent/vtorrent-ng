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
use vtorrent_core::time::now_timestamp_u32;

mod config;

use clap::Parser;
use config::{validate_startup_config, Cli};
use tracing_subscriber::EnvFilter;
use zeroize::Zeroize;

use vtorrent_node::events as node_events;
use vtorrent_node::node::{Node, NodeConfig};
use vtorrent_onion::TransportConfig;
use vtorrent_rpc::ws::NodeEvent as RpcNodeEvent;
use vtorrent_rpc::{server::start_server, state::AppState};
use vtorrent_spv::SpvHeader;
use vtorrent_store::store::BlockStore;

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
    let data_dir = cli.data_dir.clone().unwrap_or_else(|| {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".vtorrent")
    });

    // ── Validate startup configuration (before any network connections) ───────
    validate_startup_config(&cli, &data_dir)?;

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
        // Regtest shares the mainnet magic and port, so an un-isolated regtest
        // node can reach production seeds, ingest their peer view, and gossip
        // locally-minted non-PoS blocks to the live network. Force isolation
        // in regtest mode (mirrors the faucet: local-only by design).
        isolated: cli.isolated || cli.regtest,
        public_addr: cli.public_addr.as_deref().and_then(|a| a.parse().ok()),
        data_dir: data_dir.clone(),
        use_overlay: true,
        testnet: cli.testnet,
        regtest: cli.regtest,
        regtest_fast_stake: cli.regtest_fast_stake,
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
            let chain = if cli.regtest {
                block_store.load_into_regtest_chain()
            } else {
                block_store.load_into_chain()
            }
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
    let chain_arc_clone = std::sync::Arc::clone(&chain_arc);
    let mempool_arc = node.mempool_arc();
    let mempool_arc_for_saver = std::sync::Arc::clone(&mempool_arc);

    // ── Mempool durability: reload persisted entries, then save periodically ──
    let mempool_path = data_dir.join("mempool.json");
    {
        let saved = vtorrent_node::mempool::Mempool::load_saved(&mempool_path);
        if !saved.is_empty() {
            let mut mp = mempool_arc.lock().await;
            let chain = chain_arc_clone.lock().await;
            let mut admitted = 0usize;
            for (tx, _old_fee) in saved {
                // Re-verify against the loaded UTXO set: entries may reference
                // inputs that were confirmed while we were down.
                if mp.admit_with_chain_fee(&chain, tx).is_ok() {
                    admitted += 1;
                }
            }
            drop(chain);
            drop(mp);
            if admitted > 0 {
                tracing::info!("Restored {} mempool transactions from disk", admitted);
            }
        }
    }
    let tx_submit_sender = node.tx_submit_sender();
    let block_submit_sender = node.block_submit_sender();
    let staking_control_sender = node.staking_control();
    let mut rpc_state = AppState::new_with_shared(chain_arc, mempool_arc);
    // Wire the tx broadcast channel so RPC wallet can push txs into the P2P loop.
    rpc_state.tx_submit = Some(tx_submit_sender);
    // Wire the block broadcast channel so the regtest faucet can announce blocks.
    rpc_state.block_submit = Some(block_submit_sender);
    // Wire the staking control channel so RPC can enable/disable staking.
    rpc_state.staking_control = Some(staking_control_sender);
    rpc_state.rpc_api_key = cli.rpc_api_key.clone();
    rpc_state.regtest = cli.regtest;
    rpc_state.network = if cli.regtest {
        "vtorrent-regtest".to_string()
    } else if config.testnet {
        "vtorrent-testnet".to_string()
    } else {
        "vtorrent-mainnet".to_string()
    };
    // Persist imported wallets under the data dir and restore any previous
    // one on startup (the wallet stays locked until /wallet/unlock).
    let wallet_path = data_dir.join("wallet.json");
    rpc_state.wallet_path = Some(wallet_path.clone());
    rpc_state.staking_state_path = Some(data_dir.join("staking.json"));
    if wallet_path.exists() {
        match std::fs::read(&wallet_path)
            .map_err(|e| anyhow::anyhow!("read failed: {}", e))
            .and_then(|b| {
                serde_json::from_slice::<serde_json::Value>(&b)
                    .map_err(|e| anyhow::anyhow!("parse failed: {}", e))
            }) {
            Ok(blob) => {
                match serde_json::from_value::<vtorrent_wallet::encryption::EncryptedWallet>(
                    blob["wallet"].clone(),
                ) {
                    Ok(encrypted) => {
                        *rpc_state.wallet_encrypted.write().await = Some(encrypted);
                        tracing::info!(
                            "Restored encrypted wallet from {} (locked)",
                            wallet_path.display()
                        );
                    }
                    Err(e) => tracing::warn!("Could not parse stored wallet: {}", e),
                }
            }
            Err(e) => tracing::warn!("Could not read stored wallet: {}", e),
        }
    }
    // Reflect startup-configured staking in the RPC status so it agrees with
    // the node's actual staking engine (rather than reporting "disabled").
    if staking_enabled {
        *rpc_state.staking_enabled.write().await = true;
        *rpc_state.staking_address.write().await = cli.staking_address.clone();
    }
    let rpc_addr = cli.rpc_addr.clone();

    // Set the torrent download directory under the data dir.
    *rpc_state.download_dir.write().await = data_dir.join("downloads");

    // Initialize the Bitcoin SPV wallet from the seed, if provided.
    if let Some(seed_hex) = &cli.btc_seed {
        let seed_bytes = hex::decode(seed_hex)
            .map_err(|e| anyhow::anyhow!("Invalid --btc-seed (expected 64-byte hex): {}", e))?;
        let mut seed: [u8; 64] = seed_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("--btc-seed must be exactly 64 bytes (128 hex chars)"))?;
        let network = if cli.btc_regtest {
            bitcoin::Network::Regtest
        } else {
            bitcoin::Network::Bitcoin
        };
        *rpc_state.btc_wallet.write().await =
            Some(vtorrent_btc::wallet::BtcWallet::with_network(seed, network));
        *rpc_state.btc_network.write().await = network;
        // Zeroize the local seed material; the wallet holds its own copy
        // (zeroized on drop) and the CLI string is process-lifetime anyway.
        seed.zeroize();
        if let Some(peer) = &cli.btc_peer {
            // Store the host:port as given; it is resolved on every connection
            // attempt so peer IPs can change across container restarts.
            *rpc_state.btc_peer.write().await = Some(peer.clone());
        }
        tracing::info!(
            "Bitcoin SPV wallet initialized from --btc-seed (network: {:?})",
            network
        );
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
        let chain_ref = Arc::clone(&rpc_state.chain);
        let peer_list_ref = Arc::clone(&rpc_state.peer_list);
        let blocks_staked_ref = Arc::clone(&rpc_state.blocks_staked);
        let last_stake_time_ref = Arc::clone(&rpc_state.last_stake_time);
        let rewards_earned_ref = Arc::clone(&rpc_state.rewards_earned_sats);

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
                                utxo_root: [0u8; 32],
                                timestamp: block.header.timestamp,
                                bits: block.header.bits,
                                nonce: block.header.nonce,
                                height: *height,
                            };
                            {
                                let mut spv = spv_chain_ref.write().await;
                                if let Err(e) = spv.add_trusted_header(spv_header) {
                                    tracing::debug!(
                                        "SPV chain: could not add header at {}: {}",
                                        height,
                                        e
                                    );
                                }
                            }
                        }

                        // ── Track staking counters ─────────────────────────
                        if let node_events::NodeEvent::StakingReward { reward_sats, .. } = &*event {
                            *blocks_staked_ref.write().await += 1;
                            *last_stake_time_ref.write().await = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                as u32;
                            let current = *rewards_earned_ref.read().await;
                            *rewards_earned_ref.write().await =
                                current.saturating_add(*reward_sats);
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
                            node_events::NodeEvent::TxUnconfirmed {
                                txid,
                                fee_sats,
                                size_bytes,
                            } => {
                                let fee_rate = if *size_bytes > 0 {
                                    *fee_sats as f64 / *size_bytes as f64
                                } else {
                                    0.0
                                };
                                Some(RpcNodeEvent::TxUnconfirmed {
                                    txid: hex::encode(txid),
                                    fee_sats: *fee_sats,
                                    fee_rate,
                                    size_bytes: *size_bytes,
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
                                rolled_back_blocks,
                                applied_fork_blocks,
                            } => {
                                // ── Persist the reorg: undo abandoned blocks, then
                                // record the fork blocks now canonical. Without
                                // this the on-disk state diverges from memory and
                                // the next restart fails replay.
                                for rb in rolled_back_blocks.iter() {
                                    if let Err(e) = store_for_bridge.rollback_tip(
                                        &rb.utxos_to_restore,
                                        &rb.utxos_to_remove,
                                        &rb.claimed_to_remove,
                                    ) {
                                        tracing::error!(
                                            "BlockStore::rollback_tip failed for block {} at height {}: {}",
                                            hex::encode(rb.hash),
                                            rb.height,
                                            e
                                        );
                                    }
                                }
                                for fb in applied_fork_blocks.iter() {
                                    if let Err(e) = store_for_bridge.append_block(
                                        &fb.block,
                                        fb.height,
                                        &fb.utxos_added,
                                        &fb.utxos_removed,
                                        &fb.claimed_addresses,
                                    ) {
                                        tracing::error!(
                                            "BlockStore::append_block (reorg) failed at height {}: {}",
                                            fb.height,
                                            e
                                        );
                                    }
                                }

                                Some(RpcNodeEvent::Reorg {
                                    old_tip: hex::encode(old_tip),
                                    new_tip: hex::encode(new_tip),
                                    depth: *depth,
                                })
                            }
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
                        // Dropped events may include NewBlock persistence — the
                        // store can now be behind the in-memory chain. Rebuild
                        // derived state from the chain's full block list.
                        tracing::error!(
                            "Event bridge lagged, {} events lost — reconciling block store",
                            n
                        );
                        let blocks: Vec<vtorrent_node::block::Block> = {
                            let chain = chain_ref.lock().await;
                            (0..=chain.best_height())
                                .filter_map(|h| chain.get_block_at_height(h))
                                .cloned()
                                .collect()
                        };
                        let rebuild = if config.regtest {
                            store_for_bridge.rebuild_from_regtest_blocks(&blocks)
                        } else {
                            store_for_bridge.rebuild_from_blocks(&blocks)
                        };
                        if let Err(e) = rebuild {
                            tracing::error!("Block store reconciliation failed: {}", e);
                        } else {
                            tracing::info!(
                                "Block store reconciled to height {} after lag",
                                blocks.len().saturating_sub(1)
                            );
                        }
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
            let now = now_timestamp_u32();
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
            let peer_address = payment.peer_address.clone();
            let result = vtorrent_wallet_service::build_incentive_payment(
                &payment_wif,
                &payment_change,
                &payment_chain,
                &payment_mempool,
                payment_tx_submit.as_ref(),
                &vtorrent_wallet_service::IncentivePayment {
                    peer_address,
                    amount_satoshis: payment.amount_satoshis,
                },
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
    let btc_network = Arc::clone(&rpc_state.btc_network);
    let btc_peer = Arc::clone(&rpc_state.btc_peer);
    tokio::spawn(async move {
        tracing::info!("Bitcoin SPV sync task started");
        loop {
            let has_wallet = btc_wallet.read().await.is_some();
            if !has_wallet {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                continue;
            }
            let network = *btc_network.read().await;
            // Resolve peers: explicit regtest peer, or mainnet DNS seeds.
            let addrs: Vec<std::net::SocketAddr> = match btc_peer.read().await.clone() {
                Some(host) => match resolve_addr(&host).await {
                    Ok(addr) => vec![addr],
                    Err(e) => {
                        tracing::warn!("BTC peer {} unresolved: {}", host, e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                        continue;
                    }
                },
                None => match vtorrent_btc::sync::resolve_seeds().await {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!("BTC seed resolution failed: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                        continue;
                    }
                },
            };
            for addr in addrs {
                tracing::debug!("BTC sync: connecting to {} (network {:?})", addr, network);
                match vtorrent_btc::p2p::BtcPeer::connect_with_network(addr, network).await {
                    Ok(mut peer) => {
                        if let Some(w) = btc_wallet.write().await.as_mut() {
                            match w.sync(&mut peer).await {
                                Ok(n) => tracing::info!("BTC sync: {} headers", n),
                                Err(e) => tracing::warn!("BTC sync error: {}", e),
                            }
                            // After header sync, scan for wallet UTXOs from the
                            // last checkpoint to the tip. Use BIP-158 compact
                            // block filters (BIP-37 is disabled by most
                            // mainnet nodes).
                            //
                            // Only checkpoint the range actually covered: a
                            // partial scan (peer filter-index lag, dropped
                            // connection) must be resumed on the next cycle,
                            // never skipped over.
                            loop {
                                let start = w.last_scanned_height();
                                let tip = w.best_height();
                                if start > tip {
                                    break;
                                }
                                match w.scan_utxos_bip158(&mut peer, start).await {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        let done = start.saturating_add(n as u32);
                                        w.set_last_scanned_height(done);
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

    // Periodic mempool persistence so restarts keep unconfirmed transactions.
    {
        let mp = std::sync::Arc::clone(&mempool_arc_for_saver);
        let path = mempool_path.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                let mut mempool = mp.lock().await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                mempool.evict_stale(now);
                if let Err(e) = mempool.save_to(&path) {
                    tracing::warn!("Mempool save failed: {}", e);
                }
            }
        });
    }

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
            // Flush mempool to disk so unconfirmed transactions survive restart.
            if let Err(e) = mempool_arc_for_saver.lock().await.save_to(&mempool_path) {
                tracing::warn!("Final mempool save failed: {}", e);
            }
        }
    }

    tracing::info!("vTorrent daemon stopped.");
    Ok(())
}

/// Wait for Ctrl+C or SIGTERM.
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

/// Resolve a `host:port` peer address, accepting hostnames as well as IPs.
async fn resolve_addr(peer: &str) -> anyhow::Result<std::net::SocketAddr> {
    if let Ok(addr) = peer.parse() {
        return Ok(addr);
    }
    let mut addrs = tokio::net::lookup_host(peer)
        .await
        .map_err(|e| anyhow::anyhow!("resolution failed: {}", e))?;
    addrs.next().ok_or_else(|| anyhow::anyhow!("no addresses"))
}
