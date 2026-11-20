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

mod config;
mod incentive_journal;
mod incentive_ledger;
mod sync_status;

#[cfg(feature = "test-util")]
async fn pause_reorg_write(requested: Option<usize>, completed: usize) {
    if requested == Some(completed) {
        tracing::info!("Test reorg persistence paused after {completed} writes");
        std::future::pending::<()>().await;
    }
}

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

/// Node-event broadcast buffer. A store that falls this far behind is
/// reconciled by replaying the chain tail, which must therefore be resident in
/// memory — so `--block-body-cache` must be >= this value.
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Heap profiler, active only under the `heap-profile` feature.
///
/// Writes `dhat-heap.json` on exit. Never enabled in release builds; used to
/// locate the sustained RSS growth (see `docs/memory-observability-design.md`
/// §7.3). The global allocator must be installed for the whole process, so the
/// guard is held for the lifetime of `main`.
#[cfg(feature = "heap-profile")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Current heap profiler. Held in a global so a signal handler can drop it
/// (which writes `dhat-heap.json`) and start a fresh interval.
#[cfg(feature = "heap-profile")]
static HEAP_PROFILER: std::sync::Mutex<Option<dhat::Profiler>> = std::sync::Mutex::new(None);

/// Count of heap-dump intervals written.
#[cfg(feature = "heap-profile")]
static HEAP_DUMP_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "heap-profile")]
fn init_heap_profiler() {
    *HEAP_PROFILER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(dhat::Profiler::new_heap());
}

/// End the current profiling interval (writes `dhat-heap.json`), rename it to
/// `dhat-heap-<n>.json`, and begin a new interval.
///
/// Lets a long-running soak be profiled in windows: the first interval covers
/// startup + warmup; later intervals are steady state. Triggered by `SIGUSR1`
/// (`docker kill -s USR1`). Feature-gated and never compiled into release
/// builds.
#[cfg(feature = "heap-profile")]
fn dump_heap_interval() {
    let mut guard = HEAP_PROFILER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = None; // Drop writes ./dhat-heap.json
    let n = HEAP_DUMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dst = format!("dhat-heap-{n}.json");
    match std::fs::rename("dhat-heap.json", &dst) {
        Ok(()) => tracing::info!("heap interval written to {dst}; starting next interval"),
        Err(e) => tracing::warn!("heap interval rename failed: {e}"),
    }
    *guard = Some(dhat::Profiler::new_heap());
}

/// Load the persisted chain from the block store, choosing the network mode
/// from the CLI flags. Shared by the normal path and the test-util governance
/// override path.
fn load_chain_from_store(
    block_store: &BlockStore,
    regtest: bool,
    fast_stake: bool,
    cache: usize,
) -> Result<vtorrent_node::chain::Chain, vtorrent_store::StoreError> {
    if regtest && fast_stake {
        block_store.load_into_fast_regtest_chain_with_cache(cache)
    } else if regtest {
        block_store.load_into_regtest_chain_with_cache(cache)
    } else {
        block_store.load_into_chain_with_cache(cache)
    }
}

/// UTXO file for this seed. Two wallets on one data directory must not share
/// state (cursor, reservations, contracts, birthday), so the filename carries a
/// seed-derived id, matching the desktop path's scheme.
fn btc_utxo_path(
    data_dir: &std::path::Path,
    network_label: &str,
    seed: &[u8; 64],
) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"vtorrent-btc-wallet-id-v1");
    hasher.update(seed);
    let wallet_id = hex::encode(&hasher.finalize()[..8]);
    data_dir.join(format!("btc_utxos-{network_label}-{wallet_id}.json"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(feature = "heap-profile")]
    init_heap_profiler();

    let mut cli = Cli::parse();
    config::load_secret_files(&mut cli)?;

    // Initialise structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .init();

    #[cfg(feature = "heap-profile")]
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::user_defined1()) {
            Ok(mut sig) => {
                while sig.recv().await.is_some() {
                    dump_heap_interval();
                }
            }
            Err(e) => tracing::warn!("could not install SIGUSR1 heap-dump handler: {e}"),
        }
    });

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

    if cli.block_body_cache < EVENT_CHANNEL_CAPACITY {
        tracing::warn!(
            block_body_cache = cli.block_body_cache,
            event_channel_capacity = EVENT_CHANNEL_CAPACITY,
            "block-body cache is smaller than the event-bridge buffer; a max-lag \
             store reconciliation may not find the tail resident and will abort. \
             Raise --block-body-cache."
        );
    }

    // ── Resolve data directory ────────────────────────────────────────────────
    let data_dir = cli.data_dir.unwrap_or_else(|| {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".vtorrent")
    });

    std::fs::create_dir_all(&data_dir)?;

    if cli.btc_seed.is_some() {
        let label = if cli.btc_regtest {
            "regtest"
        } else {
            "mainnet"
        };
        for name in [
            format!("btc_utxos-{label}.json"),
            "btc_utxos.json".to_string(),
        ] {
            let legacy_path = data_dir.join(&name);
            match std::fs::symlink_metadata(&legacy_path) {
                Ok(_) => anyhow::bail!(
                    "legacy BTC wallet state exists at {}; preserve it and resolve its ownership before starting the BTC wallet",
                    legacy_path.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => anyhow::bail!(
                    "cannot inspect legacy BTC wallet state at {}: {}",
                    legacy_path.display(),
                    error
                ),
            }
        }
    }

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
    let isolated = cli.isolated || cli.regtest;
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
        isolated,
        public_addr: cli.public_addr.as_deref().and_then(|a| a.parse().ok()),
        data_dir: data_dir.clone(),
        use_overlay: !isolated,
        testnet: cli.testnet,
        regtest: cli.regtest,
        regtest_fast_stake: cli.regtest_fast_stake,
        transport,
        load_persisted_checkpoint: true,
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
        } else {
            tracing::info!("No persisted chain found — starting from genesis");
        }
        // Test-only governance activation for regtest load testing. Read before
        // the chain load so replay rebuilds governance state, matching production
        // replay under a scheduled deployment. The env var is ignored entirely
        // without the test-util feature (never enabled in release builds); with
        // it, regtest is still mandatory.
        #[cfg(feature = "test-util")]
        let governance_activation = match std::env::var("VTR_GOVERNANCE_TEST_ACTIVATION") {
            Ok(raw) => {
                if !cli.regtest {
                    anyhow::bail!("VTR_GOVERNANCE_TEST_ACTIVATION requires --regtest");
                }
                let height: u32 = raw.parse().map_err(|e| {
                    anyhow::anyhow!("VTR_GOVERNANCE_TEST_ACTIVATION: invalid height {raw:?}: {e}")
                })?;
                tracing::warn!(
                    height,
                    "TEST-ONLY governance activation override — never use outside regtest load testing"
                );
                Some(height)
            }
            Err(_) => None,
        };
        let mut chain = {
            #[cfg(feature = "test-util")]
            {
                if let Some(height) = governance_activation {
                    if cli.regtest_fast_stake {
                        block_store.load_into_fast_regtest_chain_with_cache_and_governance(
                            cli.block_body_cache,
                            height,
                        )
                    } else {
                        block_store.load_into_regtest_chain_with_cache_and_governance(
                            cli.block_body_cache,
                            height,
                        )
                    }
                } else {
                    load_chain_from_store(
                        &block_store,
                        cli.regtest,
                        cli.regtest_fast_stake,
                        cli.block_body_cache,
                    )
                }
            }
            #[cfg(not(feature = "test-util"))]
            {
                load_chain_from_store(
                    &block_store,
                    cli.regtest,
                    cli.regtest_fast_stake,
                    cli.block_body_cache,
                )
            }
        }
        .map_err(|e| anyhow::anyhow!("Failed to load chain from store: {}", e))?;
        if let Some(value) = &cli.checkpoint {
            let (height, hash) = config::parse_checkpoint(value)
                .map_err(|e| anyhow::anyhow!("Invalid --checkpoint: {}", e))?;
            if let Some(persisted) = vtorrent_node::node::read_checkpoint(&data_dir)? {
                if persisted.genesis == chain.genesis_block().hash()
                    && persisted.height > height
                    && persisted.hash != hash
                {
                    anyhow::bail!(
                        "refusing to lower checkpoint from height {}; delete {}/checkpoint.json to reset",
                        persisted.height,
                        data_dir.display()
                    );
                }
            }
            chain
                .set_checkpoint(height, hash)
                .map_err(|e| anyhow::anyhow!("Could not install checkpoint: {}", e))?;
            vtorrent_node::node::persist_checkpoint(
                &data_dir,
                height,
                hash,
                chain.genesis_block().hash(),
            )
            .map_err(|e| anyhow::anyhow!("Could not persist checkpoint: {}", e))?;
            tracing::info!(
                height,
                hash = %hex::encode(hash),
                "Enforcing operator checkpoint"
            );
        }
        // Serve bodies pruned from memory during replay from the store.
        let body_source: std::sync::Arc<dyn vtorrent_node::chain::BlockBodySource> =
            block_store.clone();
        chain.set_body_source(body_source);
        Node::new_with_chain(config.clone(), chain)
            .map_err(|e| anyhow::anyhow!("Node::new_with_chain failed: {}", e))?
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
            // Lock order is chain → mempool (see vtorrent-node/src/node/mod.rs).
            // Acquiring them the other way here would invert the order the
            // node loop uses and risk a deadlock.
            let chain = chain_arc_clone.lock().await;
            let mut mp = mempool_arc.lock().await;
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
    let stake_proofs = node.stake_proofs_arc();
    let mut rpc_state = AppState::new_with_shared(chain_arc, mempool_arc);
    rpc_state.stake_proofs = stake_proofs;
    // Wire the tx broadcast channel so RPC wallet can push txs into the P2P loop.
    rpc_state.tx_submit = Some(tx_submit_sender);
    // Wire the block broadcast channel so the regtest faucet can announce blocks.
    rpc_state.block_submit = Some(block_submit_sender);
    // Wire the staking control channel so RPC can enable/disable staking.
    rpc_state.staking_control = Some(staking_control_sender);
    // Share the ban manager so the authed unban endpoint works.
    rpc_state.ban_manager = Some(node.ban_manager_handle());
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
    rpc_state.swap_recovery_dir = Some(data_dir.join("swaps"));
    rpc_state.staking_state_path = Some(data_dir.join("staking.json"));
    let cold_stake_attestor_path = data_dir.join("cold-stake-attestor.key");
    rpc_state.cold_stake_attestor_key_path = Some(cold_stake_attestor_path.clone());
    match vtorrent_rpc::cold_stake_attestor::load_or_create_attestor_key(&cold_stake_attestor_path)
    {
        Ok(key) => tracing::info!(
            "Cold-stake attestor public key: {}",
            vtorrent_rpc::cold_stake_attestor::attestor_pubkey_hex(&key)
        ),
        Err(error) => tracing::warn!(
            "Cold-stake attestor key unavailable at {}: {}",
            cold_stake_attestor_path.display(),
            error
        ),
    }
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
    // Anchor the SPV chain to the pinned VTR genesis before any header can be
    // accepted. This runs unconditionally: an unpinned chain would let an
    // authenticated caller install an arbitrary root and replace the
    // inclusion view.
    {
        let genesis = vtorrent_node::genesis::create_genesis_block();
        let header = SpvHeader {
            version: genesis.header.version,
            prev_hash: genesis.header.prev_block_hash,
            merkle_root: genesis.header.merkle_root,
            utxo_root: genesis.header.utxo_root,
            timestamp: genesis.header.timestamp,
            bits: genesis.header.bits,
            nonce: genesis.header.nonce,
            stake_modifier: genesis.header.stake_modifier,
            height: 0,
        };
        let mut spv = rpc_state.spv_chain.write().await;
        if let Err(error) = spv.set_genesis(
            vtorrent_node::genesis::GENESIS_HASH,
            vtorrent_node::genesis::GENESIS_BITS,
        ) {
            anyhow::bail!("Could not pin SPV genesis: {error}");
        }
        if let Err(error) = spv.add_trusted_header(header) {
            anyhow::bail!("Could not seed the SPV genesis header: {error}");
        }
    }
    vtorrent_rpc::spv_sync::sync_spv_from_chain(&rpc_state)
        .await
        .map_err(|error| anyhow::anyhow!("could not seed SPV headers: {error}"))?;

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
        let network_label = if cli.btc_regtest {
            "regtest"
        } else {
            "mainnet"
        };
        rpc_state.btc_peer_cache_path =
            Some(data_dir.join(format!("btc_peers-{network_label}.json")));
        let mut btc_wallet = vtorrent_btc::wallet::BtcWallet::with_persistence_attested_seed(
            seed,
            network,
            btc_utxo_path(&data_dir, network_label, &seed),
            cli.btc_attest_new_seed,
        )
        .map_err(|e| anyhow::anyhow!("Could not load persistent BTC wallet state: {}", e))?;
        // Bound the in-memory SPV header chain (mainnet ~870k headers).
        btc_wallet.set_header_retention(cli.btc_header_retention);
        if let Some(birthday) = cli.btc_birthday {
            if !btc_wallet.set_initial_birthday(birthday) {
                anyhow::bail!("Could not persist BTC wallet birthday and scan cursor");
            }
        }
        // Zeroize the local seed material; the wallet holds its own copy
        // (zeroized on drop) and the CLI string is process-lifetime anyway.
        seed.zeroize();
        *rpc_state.btc_wallet.write().await = Some(btc_wallet);
        *rpc_state.btc_network.write().await = network;
        if let Some(peer) = &cli.btc_peer {
            // Store the host:port as given; it is resolved on every connection
            // attempt so peer IPs can change across container restarts.
            *rpc_state.btc_peer.write().await = Some(peer.clone());
        }
        tracing::info!(
            "Bitcoin SPV wallet initialized from configured seed (network: {:?})",
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
        let (node_tx, mut node_rx) = node_events::channel(EVENT_CHANNEL_CAPACITY);
        node.set_event_sender(node_tx);

        let rpc_broadcaster = rpc_state.events.clone();
        let store_for_bridge = Arc::clone(&block_store);
        let block_body_cache = cli.block_body_cache;
        #[cfg(feature = "test-util")]
        let reorg_pause = match std::env::var("VTR_TEST_REORG_PAUSE_AFTER") {
            Ok(raw) => {
                anyhow::ensure!(cli.regtest, "VTR_TEST_REORG_PAUSE_AFTER requires --regtest");
                Some(raw.parse::<usize>().map_err(|e| {
                    anyhow::anyhow!("VTR_TEST_REORG_PAUSE_AFTER: invalid boundary {raw:?}: {e}")
                })?)
            }
            Err(_) => None,
        };
        let sync_status = sync_status::SyncStatus::new(&rpc_state);
        let spv_chain_ref = Arc::clone(&rpc_state.spv_chain);
        let chain_ref = Arc::clone(&rpc_state.chain);
        let state_for_bridge = rpc_state.clone();
        let blocks_staked_ref = Arc::clone(&rpc_state.blocks_staked);
        let last_stake_time_ref = Arc::clone(&rpc_state.last_stake_time);
        let rewards_earned_ref = Arc::clone(&rpc_state.rewards_earned_sats);

        tokio::spawn(async move {
            loop {
                match node_rx.recv().await {
                    Ok(event) => {
                        if matches!(
                            &*event,
                            node_events::NodeEvent::PeerConnected { .. }
                                | node_events::NodeEvent::PeerDisconnected { .. }
                                | node_events::NodeEvent::NewBlock { .. }
                                | node_events::NodeEvent::Reorg { .. }
                        ) {
                            let local_height = chain_ref.lock().await.best_height();
                            sync_status.apply(&event, local_height).await;
                        }
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
                                utxo_root: block.header.utxo_root,
                                timestamp: block.header.timestamp,
                                bits: block.header.bits,
                                nonce: block.header.nonce,
                                stake_modifier: block.header.stake_modifier,
                                height: *height,
                            };
                            let spv_failed = {
                                let mut spv = spv_chain_ref.write().await;
                                if let Err(e) = spv.add_trusted_header(spv_header) {
                                    tracing::debug!(
                                        "SPV chain: could not add header at {}: {}",
                                        height,
                                        e
                                    );
                                    true
                                } else {
                                    false
                                }
                            };
                            if spv_failed {
                                if let Err(e) =
                                    vtorrent_rpc::spv_sync::sync_spv_from_chain(&state_for_bridge)
                                        .await
                                {
                                    tracing::warn!(
                                        "SPV chain: full resync after a failed header failed: {}",
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
                            } => Some(RpcNodeEvent::PeerConnected {
                                addr: addr.to_string(),
                                version: *version,
                                user_agent: user_agent.clone(),
                                height: *height,
                            }),
                            node_events::NodeEvent::PeerDisconnected { addr } => {
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
                                #[cfg(feature = "test-util")]
                                pause_reorg_write(reorg_pause, 0).await;
                                #[cfg(feature = "test-util")]
                                let mut completed = 0;
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
                                    #[cfg(feature = "test-util")]
                                    {
                                        completed += 1;
                                        pause_reorg_write(reorg_pause, completed).await;
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
                                    #[cfg(feature = "test-util")]
                                    {
                                        completed += 1;
                                        pause_reorg_write(reorg_pause, completed).await;
                                    }
                                }
                                // The SPV chain must follow the reorg too:
                                // without these headers, the next NewBlock's
                                // parent is unknown and every later header is
                                // rejected, leaving inclusion proofs anchored
                                // to the abandoned tip.
                                let spv_failed = {
                                    let mut spv = spv_chain_ref.write().await;
                                    let mut failed = false;
                                    for fb in applied_fork_blocks.iter() {
                                        let header = SpvHeader {
                                            version: fb.block.header.version,
                                            prev_hash: fb.block.header.prev_block_hash,
                                            merkle_root: fb.block.header.merkle_root,
                                            utxo_root: fb.block.header.utxo_root,
                                            timestamp: fb.block.header.timestamp,
                                            bits: fb.block.header.bits,
                                            nonce: fb.block.header.nonce,
                                            stake_modifier: fb.block.header.stake_modifier,
                                            height: fb.height,
                                        };
                                        if let Err(e) = spv.add_trusted_header(header) {
                                            tracing::warn!(
                                                "SPV chain: could not add reorg header at {}: {}",
                                                fb.height,
                                                e
                                            );
                                            failed = true;
                                        }
                                    }
                                    failed
                                };
                                if spv_failed {
                                    if let Err(e) = vtorrent_rpc::spv_sync::sync_spv_from_chain(
                                        &state_for_bridge,
                                    )
                                    .await
                                    {
                                        tracing::warn!(
                                            "SPV chain: full resync after a failed reorg header failed: {}",
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
                        // Decide how to reconcile, then act outside the chain
                        // lock. Normal case: the store is a prefix of the chain
                        // and only its tail is missing → append the tail. If the
                        // store is *not* a prefix (e.g. a dropped reorg rollback),
                        // heal by rebuilding from the chain (pruned bodies are
                        // fetched back from the store itself).
                        enum Plan {
                            Tail(u32, Vec<vtorrent_node::block::Block>),
                            Full(Vec<vtorrent_node::block::Block>),
                            Unavailable(String),
                        }
                        let plan = {
                            let chain = chain_ref.lock().await;
                            let tip = chain.best_height();
                            let store_height = store_for_bridge.best_height().unwrap_or(0);
                            let store_tip = store_for_bridge.best_hash().ok().flatten();
                            let diverged = store_tip.is_some()
                                && store_tip != chain.block_hash_at_height(store_height);
                            if diverged {
                                // Gathering may read pruned bodies from the
                                // store under the lock, but this path is rare and
                                // needs a consistent snapshot.
                                match (0..=tip)
                                    .map(|h| chain.get_block_at_height_owned(h))
                                    .collect::<Option<Vec<_>>>()
                                {
                                    Some(blocks) => Plan::Full(blocks),
                                    None => Plan::Unavailable(format!(
                                        "store diverged at height {store_height} and a chain body \
                                         needed for a full rebuild is unavailable"
                                    )),
                                }
                            } else {
                                let start = store_height + 1;
                                match (start..=tip)
                                    .map(|h| chain.get_block_at_height(h).cloned())
                                    .collect::<Option<Vec<_>>>()
                                {
                                    Some(blocks) => Plan::Tail(start, blocks),
                                    None => Plan::Unavailable(format!(
                                        "tail body at height >= {start} was pruned and the store \
                                         lags; raise --block-body-cache"
                                    )),
                                }
                            }
                        };
                        let result: Result<String, String> = match plan {
                            Plan::Tail(start, blocks) => store_for_bridge
                                .reconcile_tail(
                                    config.regtest,
                                    config.regtest_fast_stake,
                                    block_body_cache,
                                    start,
                                    &blocks,
                                )
                                .map(|()| {
                                    format!("appended {} tail block(s) from {start}", blocks.len())
                                })
                                .map_err(|e| format!("tail append failed: {e}")),
                            Plan::Full(blocks) => {
                                let r = if config.regtest && config.regtest_fast_stake {
                                    store_for_bridge.rebuild_from_fast_regtest_blocks(&blocks)
                                } else if config.regtest {
                                    store_for_bridge.rebuild_from_regtest_blocks(&blocks)
                                } else {
                                    store_for_bridge.rebuild_from_blocks(&blocks)
                                };
                                r.map(|()| format!("full rebuild of {} block(s)", blocks.len()))
                                    .map_err(|e| format!("full rebuild failed: {e}"))
                            }
                            Plan::Unavailable(reason) => Err(reason),
                        };
                        match result {
                            Ok(what) => tracing::info!(
                                "Block store reconciled after event-bridge lag ({what})"
                            ),
                            Err(e) => {
                                tracing::error!("Block store reconciliation failed: {e}");
                            }
                        }
                        // Lost events also leave the in-memory SPV view stale.
                        // Rebuild it from the same canonical chain the store was
                        // reconciled against.
                        if let Err(e) =
                            vtorrent_rpc::spv_sync::sync_spv_from_chain(&state_for_bridge).await
                        {
                            tracing::error!(
                                "SPV chain reconciliation failed after event-bridge lag: {e}"
                            );
                        }
                        let local_height = chain_ref.lock().await.best_height();
                        sync_status.refresh(local_height).await;
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
        if config.use_dht && !config.isolated {
            "enabled"
        } else {
            "disabled"
        }
    );
    tracing::info!(
        "  Network:         {}",
        if config.regtest {
            "regtest"
        } else if config.testnet {
            "testnet"
        } else {
            "mainnet"
        }
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
    // Fail closed on an unreadable passphrase file: silently starting locked
    // would recreate the exact stall this option exists to prevent.
    // Fail closed on an unusable passphrase file: silently starting locked
    // would recreate the exact stall this option exists to prevent.
    if let Some(path) = &cli.wallet_passphrase_file {
        validate_passphrase_file(path)?;
    }
    let rpc_state_for_unlock = rpc_state.clone();
    // VTR + BTC swap monitoring. Both are required: the BTC reconciler observes
    // the maker's claim (revealing the preimage) and drives the taker's claim.
    let _swap_services = vtorrent_rpc::services::spawn_swap_services(rpc_state.clone());
    // Belt and braces alongside the event-driven SPV updates: recovers from a
    // silently dropped event or a failed header insertion.
    let _spv_sync = vtorrent_rpc::services::spawn_spv_sync(rpc_state.clone());

    // Periodic DEX order expiry maintenance — runs every 60 seconds.
    let order_book_for_maintenance = Arc::clone(&rpc_state.order_book);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let expired = order_book_for_maintenance.write().await.expire_orders();
            if expired > 0 {
                tracing::info!("DEX maintenance: expired {} stale orders", expired);
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
    let payment_check_chain = Arc::clone(&rpc_state.chain);
    let payment_check_mempool = Arc::clone(&rpc_state.mempool);
    let payment_tx_submit = rpc_state.tx_submit.clone();
    let (payment_result_tx, mut payment_result_rx) =
        tokio::sync::mpsc::unbounded_channel::<vtorrent_torrent::payment::PaymentResult>();
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
                Ok(txid) => {
                    tracing::info!(
                        "Incentive payment {} sent to {}",
                        txid,
                        payment.peer_address
                    );
                    let parsed = hex::decode(&txid)
                        .ok()
                        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
                    let _ = payment_result_tx.send(vtorrent_torrent::payment::PaymentResult {
                        account_key: payment.account_key,
                        amount_satoshis: payment.amount_satoshis,
                        success: parsed.is_some(),
                        txid: parsed,
                    });
                }
                Err(e) => {
                    tracing::warn!("Incentive payment failed: {}", e);
                    let _ = payment_result_tx.send(vtorrent_torrent::payment::PaymentResult {
                        account_key: payment.account_key,
                        amount_satoshis: payment.amount_satoshis,
                        success: false,
                        txid: None,
                    });
                }
            }
        }
    });

    let incentive_ledger_path = data_dir.join("incentive-ledger.json");
    let incentive_live_path = data_dir.join("incentive-live.json");
    let mut incentive_shutdown_tx: Option<
        tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<()>>,
    > = None;
    if cli.torrent_incentive_payments {
        let incentive_budget_sats = cli.torrent_incentive_budget_sats;
        let incentive_payment_cap_sats = cli.torrent_incentive_payment_cap_sats;
        let (ledger_tx, mut ledger_shutdown_rx) = tokio::sync::mpsc::channel(1);
        incentive_shutdown_tx = Some(ledger_tx);
        let mut ledger = match incentive_ledger::IncentiveLedger::load(&incentive_ledger_path) {
            Ok(ledger) => ledger,
            Err(error) => {
                tracing::error!("Could not load the incentive ledger: {error:#}");
                return Err(error);
            }
        };
        let merged_live = incentive_live_path.exists();
        if merged_live {
            let mut live = match incentive_ledger::IncentiveLedger::load(&incentive_live_path) {
                Ok(live) => live,
                Err(error) => {
                    tracing::error!("Could not load the live incentive snapshot: {error:#}");
                    return Err(error);
                }
            };
            ledger.emitted_total = ledger.emitted_total.max(live.emitted_total);
            for mut debt in live.debts.drain(..) {
                debt.id = ledger.next_id;
                ledger.next_id = ledger.next_id.saturating_add(1);
                ledger.debts.push(debt);
            }
            if let Err(error) = std::fs::remove_file(&incentive_live_path) {
                tracing::warn!("Could not remove the merged live incentive snapshot: {error}");
            }
            if let Err(error) = ledger.save(&incentive_ledger_path) {
                tracing::warn!("Could not save the merged incentive ledger: {error:#}");
            }
        }
        let journal_path = data_dir.join("incentive-journal.jsonl");
        let concrete_journal = match incentive_journal::JsonlJournal::open(&journal_path) {
            Ok(journal) => journal,
            Err(error) => {
                tracing::error!("Could not open the incentive journal: {error:#}");
                return Err(error);
            }
        };
        let journal_budget = concrete_journal.emitted_total();
        let journal_accounts = concrete_journal.accounts();
        let journal: std::sync::Arc<dyn vtorrent_torrent::incentive::IncentiveJournal> =
            std::sync::Arc::new(concrete_journal);
        rpc_state
            .torrent_sessions
            .write()
            .await
            .set_journal(journal.clone());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // The journal is authoritative for keys it contains. Replace any
        // ledger debt from a previous recovery run so repeated restarts do not
        // duplicate the same obligation.
        let journal_keys: std::collections::HashSet<String> = journal_accounts
            .iter()
            .map(|record| {
                format!(
                    "{}:{}:{}",
                    hex::encode(record.info_hash),
                    record.peer_address,
                    record.receipt_epoch
                )
            })
            .collect();
        ledger
            .debts
            .retain(|debt| !journal_keys.contains(&debt.key));
        for record in journal_accounts {
            let mut account = record.account;
            if let Some(agreement) = account.agreed_owed() {
                account.settle_agreed(now, agreement.satoshis, agreement.bytes);
            }
            ledger.add_account(&record.info_hash, &record.peer_address, &account);
        }
        ledger.emitted_total = ledger.emitted_total.max(journal_budget);
        if ledger.emitted_total > journal_budget {
            journal_set_budget(&journal, ledger.emitted_total).await;
        }
        if let Err(error) = ledger.save(&incentive_ledger_path) {
            tracing::warn!("Could not save the recovered incentive ledger: {error:#}");
        }
        let mut emitted_total = ledger.emitted_total;
        let ledger_path = incentive_ledger_path.clone();
        // Periodic torrent incentive settlement — runs every 5 minutes.
        let torrent_sessions_for_settlement = Arc::clone(&rpc_state.torrent_sessions);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
                vtorrent_torrent::incentive::PAYMENT_INTERVAL_SECS,
            ));
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    Some(ack) = ledger_shutdown_rx.recv() => {
                        let flush_journal = journal.clone();
                        let _ = tokio::task::spawn_blocking(move || flush_journal.flush()).await;
                        journal_set_budget(&journal, emitted_total).await;
                        ledger.emitted_total = emitted_total;
                        if let Err(error) = ledger.save(&ledger_path) {
                            tracing::error!(
                                "Could not save the incentive ledger at shutdown: {error:#}"
                            );
                        }
                        let _ = ack.send(());
                        break;
                    }
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let mut results = Vec::new();
                while let Ok(result) = payment_result_rx.try_recv() {
                    results.push(result);
                }

                // Ledger results are keyed by a stable debt id, not a session.
                let mut ledger_changed = false;
                for result in &results {
                    let Some(id) = result
                        .account_key
                        .strip_prefix("ledger\u{0}")
                        .and_then(|value| value.parse::<u64>().ok())
                    else {
                        continue;
                    };
                    let Some(debt) = ledger.debt_mut(id) else {
                        continue;
                    };
                    if result.success {
                        if let Some(txid) = result.txid {
                            debt.pending_txid = Some(txid);
                            debt.pending_satoshis = result.amount_satoshis;
                        }
                    } else {
                        emitted_total = emitted_total.saturating_sub(result.amount_satoshis);
                    }
                    debt.payment_in_flight = false;
                    ledger_changed = true;
                }

                // Keep a ledger payment pending until six confirmations; a
                // transaction missing from the local view may still be mined,
                // so it is never re-paid.
                let mut settled_debts = Vec::new();
                for debt in &ledger.debts {
                    let Some(txid) = debt.pending_txid else {
                        continue;
                    };
                    let (confirmed, in_chain) = {
                        let chain = payment_check_chain.lock().await;
                        match chain.get_transaction_owned(&txid) {
                            Some((_, _, height)) => (
                                vtorrent_torrent::payment::payment_is_final(
                                    chain.best_height(),
                                    height,
                                ),
                                true,
                            ),
                            None => (false, false),
                        }
                    };
                    let in_mempool = if in_chain {
                        true
                    } else {
                        payment_check_mempool
                            .lock()
                            .await
                            .get_transaction(&txid)
                            .is_some()
                    };
                    if confirmed {
                        settled_debts.push((debt.id, debt.key.clone(), debt.pending_satoshis));
                    } else if !in_chain && !in_mempool {
                        tracing::warn!(
                            peer = %debt.peer_address,
                            txid = %hex::encode(txid),
                            "Ledger incentive payment is not in the chain or mempool; keeping it pending"
                        );
                    }
                }
                if !settled_debts.is_empty() {
                    for debt in &mut ledger.debts {
                        if let Some((_, _, paid)) =
                            settled_debts.iter().find(|(id, _, _)| *id == debt.id)
                        {
                            debt.confirm_payment(*paid);
                        }
                    }
                    ledger.debts.retain(|debt| debt.has_outstanding());
                    for (_, key, _) in &settled_debts {
                        let mut parts = key.splitn(3, ':');
                        let (Some(info_hex), Some(peer_address), Some(epoch)) =
                            (parts.next(), parts.next(), parts.next())
                        else {
                            continue;
                        };
                        let Ok(epoch) = epoch.parse::<u64>() else {
                            continue;
                        };
                        let Ok(info_hash) = hex::decode(info_hex)
                            .ok()
                            .and_then(|bytes| <[u8; 20]>::try_from(bytes).ok())
                            .ok_or(())
                        else {
                            continue;
                        };
                        let live = torrent_sessions_for_settlement
                            .read()
                            .await
                            .find_account(&info_hash, peer_address);
                        if let Some(account) = live {
                            journal_upsert(&journal, info_hash, peer_address, epoch, &account)
                                .await;
                        } else {
                            journal_remove(&journal, info_hash, peer_address, epoch).await;
                        }
                    }
                    ledger_changed = true;
                }

                // Pay surviving ledger debts from the previous process.
                let mut due_ledger_payments = Vec::new();
                for debt in &mut ledger.debts {
                    if debt.pending_txid.is_some()
                        || debt.payment_in_flight
                        || debt.unpaid_satoshis == 0
                    {
                        continue;
                    }
                    let amount = vtorrent_torrent::payment::capped_payment_amount(
                        debt.unpaid_satoshis,
                        incentive_payment_cap_sats,
                        incentive_budget_sats.saturating_sub(emitted_total),
                    );
                    if amount < vtorrent_torrent::incentive::MIN_PAYMENT_SATS {
                        continue;
                    }
                    debt.payment_in_flight = true;
                    emitted_total = emitted_total.saturating_add(amount);
                    due_ledger_payments.push(vtorrent_torrent::payment::PaymentDue {
                        peer_address: debt.peer_address.clone(),
                        amount_satoshis: amount,
                        account_key: format!("ledger\u{0}{}", debt.id),
                    });
                    ledger_changed = true;
                }
                let mut ledger_durable = true;
                if ledger_changed {
                    ledger.emitted_total = emitted_total;
                    if let Err(error) = ledger.save(&ledger_path) {
                        tracing::warn!("Could not save the incentive ledger: {error:#}");
                        ledger_durable = false;
                    }
                    journal_set_budget(&journal, emitted_total).await;
                }
                if ledger_durable {
                    for payment in due_ledger_payments {
                        payment_sender.emit(payment);
                    }
                } else {
                    tracing::warn!(
                        "Skipping recovered incentive payments because the ledger write failed"
                    );
                }

                let mut journal_updates = Vec::new();
                let mut due_payments = Vec::new();
                let mut pending = Vec::new();
                {
                    let mut guard = torrent_sessions_for_settlement.write().await;
                    for result in results
                        .iter()
                        .filter(|result| !result.account_key.starts_with("ledger\u{0}"))
                    {
                        let Some((session_id, peer_key)) = result.account_key.split_once('\u{0}')
                        else {
                            continue;
                        };
                        let Ok(session) = guard.get_session_mut(session_id) else {
                            continue;
                        };
                        let info_hash = session.metainfo.info_hash;
                        let Some(account) = session.incentive_accounts.get_mut(peer_key) else {
                            continue;
                        };
                        match (result.success, result.txid) {
                            (true, Some(txid)) => {
                                account.record_payment_broadcast(txid, result.amount_satoshis);
                            }
                            _ => {
                                emitted_total =
                                    emitted_total.saturating_sub(result.amount_satoshis);
                                account.payment_failed();
                            }
                        }
                        journal_updates.push((info_hash, peer_key.to_string(), account.clone()));
                    }
                    for session in guard.sessions_mut() {
                        let session_id = session.id.clone();
                        for (peer_key, account) in session.incentive_accounts.iter() {
                            if let Some(payment) = account.pending_payment {
                                pending.push((session_id.clone(), peer_key.clone(), payment.txid));
                            }
                        }
                    }
                }
                // Check pending payments without holding the torrent-session
                // lock, so a slow chain or mempool read cannot block the
                // torrent engine.
                let mut resolved = Vec::new();
                for (session_id, peer_key, txid) in pending {
                    let (confirmed, in_chain) = {
                        let chain = payment_check_chain.lock().await;
                        match chain.get_transaction_owned(&txid) {
                            Some((_, _, height)) => (
                                vtorrent_torrent::payment::payment_is_final(
                                    chain.best_height(),
                                    height,
                                ),
                                true,
                            ),
                            None => (false, false),
                        }
                    };
                    let in_mempool = if in_chain {
                        true
                    } else {
                        payment_check_mempool
                            .lock()
                            .await
                            .get_transaction(&txid)
                            .is_some()
                    };
                    if confirmed || (!in_chain && !in_mempool) {
                        resolved.push((session_id, peer_key, confirmed, txid));
                    }
                }
                let mut guard = torrent_sessions_for_settlement.write().await;
                for (session_id, peer_key, confirmed, txid) in resolved {
                    let Ok(session) = guard.get_session_mut(&session_id) else {
                        continue;
                    };
                    let info_hash = session.metainfo.info_hash;
                    let Some(account) = session.incentive_accounts.get_mut(&peer_key) else {
                        continue;
                    };
                    if confirmed {
                        account.payment_confirmed();
                    } else {
                        tracing::warn!(
                            peer = %account.peer_address,
                            txid = %hex::encode(txid),
                            "Incentive payment is not in the chain or mempool; keeping it pending to avoid a double payment"
                        );
                        account.payment_missing();
                    }
                    journal_updates.push((info_hash, peer_key.clone(), account.clone()));
                }
                let mut settled = 0;
                for session in guard.sessions_mut() {
                    let session_id = session.id.clone();
                    let info_hash = session.metainfo.info_hash;
                    for (peer_key, account) in session.incentive_accounts.iter_mut() {
                        if account.needs_settlement(now) {
                            // Never destroy accounting without a verified
                            // receipt: an unpayable account keeps accumulating
                            // until the peer starts sending receipts.
                            if account.peer_address.is_empty() {
                                continue;
                            }
                            if let Some(agreement) = account.agreed_owed() {
                                account.settle_agreed(now, agreement.satoshis, agreement.bytes);
                                journal_updates.push((
                                    info_hash,
                                    peer_key.clone(),
                                    account.clone(),
                                ));
                                settled += 1;
                            } else {
                                tracing::debug!(
                                    peer = %account.peer_address,
                                    "Withholding incentive settlement: no matching signed receipt"
                                );
                            }
                        }
                        if account.unpaid_satoshis > 0
                            && !account.payment_in_flight
                            && account.pending_payment.is_none()
                            && !account.peer_address.is_empty()
                        {
                            let amount = vtorrent_torrent::payment::capped_payment_amount(
                                account.unpaid_satoshis,
                                incentive_payment_cap_sats,
                                incentive_budget_sats.saturating_sub(emitted_total),
                            );
                            if amount < vtorrent_torrent::incentive::MIN_PAYMENT_SATS {
                                continue;
                            }
                            account.payment_in_flight = true;
                            emitted_total = emitted_total.saturating_add(amount);
                            journal_updates.push((info_hash, peer_key.clone(), account.clone()));
                            due_payments.push(vtorrent_torrent::payment::PaymentDue {
                                peer_address: account.peer_address.clone(),
                                amount_satoshis: amount,
                                account_key: format!("{}\u{0}{}", session_id, peer_key),
                            });
                        }
                    }
                }
                drop(guard);
                let mut durable = true;
                for (info_hash, peer_address, account) in journal_updates {
                    durable &= journal_upsert(
                        &journal,
                        info_hash,
                        &peer_address,
                        account.receipt_epoch,
                        &account,
                    )
                    .await;
                }
                if durable {
                    for payment in due_payments {
                        payment_sender.emit(payment);
                    }
                } else {
                    tracing::warn!(
                        "Skipping incentive payments because an account journal write failed"
                    );
                }
                journal_set_budget(&journal, emitted_total).await;
                if settled > 0 {
                    tracing::info!("Torrent incentive: settled {} peer accounts", settled);
                }
            }
        });
    } else {
        tracing::info!(
            "Torrent incentive payments are disabled; peer addresses are self-reported and receipts are not enforced"
        );
    }

    // Bitcoin SPV sync — resolves DNS seeds and syncs headers in a loop.
    tokio::spawn(vtorrent_rpc::btc_sync::run_sync_loop(std::sync::Arc::new(
        rpc_state.clone(),
    )));

    let rpc_handle = tokio::spawn(async move {
        tracing::info!("RPC server starting on {}", rpc_addr);
        if let Err(e) = start_server(&rpc_addr, rpc_state).await {
            tracing::error!("RPC server failed: {}", e);
            // A bind failure must not look like a clean shutdown: the process
            // manager should restart the node, not leave it down.
            std::process::exit(1);
        }
    });

    // Boot-time wallet auto-unlock so staking resumes without operator action.
    //
    // The wallet is restored from disk locked, so without this the staking
    // loop stays disabled after any restart and the chain stalls until someone
    // calls /wallet/unlock. The passphrase is read from a 0600 file (never
    // from argv or the environment) and is not logged.
    if let Some(passphrase_path) = cli.wallet_passphrase_file.clone() {
        let unlock_state = rpc_state_for_unlock.clone();
        tokio::spawn(async move {
            // Give the node loop a moment to start so the staking control
            // channel is being polled before we send Start.
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            let passphrase = match tokio::task::spawn_blocking({
                let path = passphrase_path.clone();
                move || std::fs::read_to_string(path)
            })
            .await
            {
                Ok(Ok(text)) => {
                    let text = zeroize::Zeroizing::new(text);
                    zeroize::Zeroizing::new(text.trim_end().to_string())
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        "Wallet auto-unlock failed: cannot read {}: {}",
                        passphrase_path.display(),
                        e
                    );
                    return;
                }
                Err(e) => {
                    tracing::error!("Wallet auto-unlock task failed: {}", e);
                    return;
                }
            };
            if passphrase.is_empty() {
                tracing::error!(
                    "Wallet auto-unlock failed: {} is empty",
                    passphrase_path.display()
                );
                return;
            }
            match vtorrent_rpc::handlers::unlock_with_passphrase(
                &unlock_state,
                &passphrase,
                None,
                0,
            )
            .await
            {
                Ok(_) => tracing::info!(
                    "Wallet auto-unlocked from {}; staking will resume if requested",
                    passphrase_path.display()
                ),
                Err(e) => tracing::error!(
                    "Wallet auto-unlock failed (wallet stays locked, staking will not resume): {}",
                    e
                ),
            }
        });
    }

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
            if let Some(sender) = incentive_shutdown_tx.take() {
                let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                if sender.send(ack_tx).await.is_ok() {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        ack_rx,
                    )
                    .await;
                }
            }
            // Flush mempool to disk so unconfirmed transactions survive restart.
            if let Err(e) = mempool_arc_for_saver.lock().await.save_to(&mempool_path) {
                tracing::warn!("Final mempool save failed: {}", e);
            }
        }
    }

    tracing::info!("vTorrent daemon stopped.");
    Ok(())
}

/// Persist one live incentive account before an external payment effect.
async fn journal_upsert(
    journal: &std::sync::Arc<dyn vtorrent_torrent::incentive::IncentiveJournal>,
    info_hash: [u8; 20],
    peer_address: &str,
    receipt_epoch: u64,
    account: &vtorrent_torrent::incentive::PeerBandwidthAccount,
) -> bool {
    let journal = journal.clone();
    let peer_address = peer_address.to_string();
    let account = account.clone();
    match tokio::task::spawn_blocking(move || {
        journal.upsert(info_hash, &peer_address, receipt_epoch, &account)
    })
    .await
    {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            tracing::warn!("Incentive journal write failed: {}", error);
            false
        }
        Err(error) => {
            tracing::warn!("Incentive journal task failed: {}", error);
            false
        }
    }
}

async fn journal_remove(
    journal: &std::sync::Arc<dyn vtorrent_torrent::incentive::IncentiveJournal>,
    info_hash: [u8; 20],
    peer_address: &str,
    receipt_epoch: u64,
) {
    let journal = journal.clone();
    let peer_address = peer_address.to_string();
    match tokio::task::spawn_blocking(move || {
        journal.remove(info_hash, &peer_address, receipt_epoch)
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!("Incentive journal delete failed: {}", error),
        Err(error) => tracing::warn!("Incentive journal task failed: {}", error),
    }
}

async fn journal_set_budget(
    journal: &std::sync::Arc<dyn vtorrent_torrent::incentive::IncentiveJournal>,
    emitted_total: u64,
) {
    let journal = journal.clone();
    let _ = tokio::task::spawn_blocking(move || journal.set_emitted_total(emitted_total)).await;
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

/// Validate the `--wallet-passphrase-file` target before startup.
///
/// Docker silently creates a *directory* when a bind-mount source is missing,
/// so an existence check alone would let the daemon start and then fail at
/// read time — leaving the wallet locked and staking stalled, which is exactly
/// the failure this option exists to prevent. Reject anything that is not a
/// regular file.
fn validate_passphrase_file(path: &std::path::Path) -> anyhow::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o077 != 0 {
                    anyhow::bail!(
                        "--wallet-passphrase-file {} is group/world accessible; require mode 0600",
                        path.display()
                    );
                }
            }
            Ok(())
        }
        Ok(_) => anyhow::bail!(
            "--wallet-passphrase-file {} is not a regular file (a missing \
             bind-mount source becomes a directory; check that the host path \
             exists and VTORRENT_WALLET_PASSPHRASE_FILE is set)",
            path.display()
        ),
        Err(e) => anyhow::bail!(
            "--wallet-passphrase-file {} is not readable: {}",
            path.display(),
            e
        ),
    }
}

#[cfg(test)]
mod passphrase_file_tests {
    use super::validate_passphrase_file;

    #[test]
    fn accepts_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passphrase");
        std::fs::write(&path, "secret\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(validate_passphrase_file(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_group_or_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passphrase");
        std::fs::write(&path, "secret\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = validate_passphrase_file(&path).unwrap_err();
        assert!(err.to_string().contains("0600"), "got: {err}");
    }

    #[test]
    fn rejects_a_directory() {
        // The Docker missing-bind-mount case: the source becomes a directory.
        let dir = tempfile::tempdir().unwrap();
        let err = validate_passphrase_file(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "got: {err}");
    }

    #[test]
    fn rejects_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_passphrase_file(&dir.path().join("nope")).unwrap_err();
        assert!(err.to_string().contains("not readable"), "got: {err}");
    }
}

#[cfg(test)]
mod btc_utxo_path_tests {
    use super::btc_utxo_path;
    use std::path::Path;

    #[test]
    fn two_wallets_on_one_data_dir_do_not_share_state() {
        let dir = Path::new("/tmp/vtr");
        let first = btc_utxo_path(dir, "mainnet", &[1u8; 64]);
        let second = btc_utxo_path(dir, "mainnet", &[2u8; 64]);
        assert_ne!(
            first, second,
            "different seeds must not share the UTXO file"
        );
        assert_eq!(first, btc_utxo_path(dir, "mainnet", &[1u8; 64]));
        assert!(first.to_string_lossy().contains("btc_utxos-mainnet-"));
    }
}
