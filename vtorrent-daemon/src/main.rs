/// vTorrent Daemon — full production node binary.
///
/// Starts three concurrent services:
///   1. P2P node (peer discovery, block sync, PoS staking)
///   2. HTTP JSON-RPC server (localhost:22525 by default)
///   3. Graceful shutdown on Ctrl+C / SIGTERM
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
///   --log-level <LEVEL>       Log level: error|warn|info|debug|trace [default: info]

use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vtorrent_node::node::{Node, NodeConfig};
use vtorrent_rpc::{server::start_server, state::AppState};

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

    /// Disable DHT bootstrap (use DNS seeds only).
    #[arg(long, default_value_t = false)]
    no_dht: bool,

    /// Additional seed nodes to connect to (can be repeated).
    #[arg(long = "seed", value_name = "ADDR")]
    seeds: Vec<String>,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, default_value = "info")]
    log_level: String,
}

// ─── Entry Point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialise structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&cli.log_level))
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

    // ── Build NodeConfig ──────────────────────────────────────────────────────
    let staking_enabled = cli.staking_address.is_some();
    let config = NodeConfig {
        listen_addr: cli.listen.clone(),
        staking_enabled,
        staking_address: cli.staking_address.clone(),
        max_mempool: 10_000,
        extra_seeds: cli.seeds.clone(),
        use_dht: !cli.no_dht,
        data_dir: data_dir.clone(),
        use_overlay: true,
    };

    // ── Build the P2P Node ────────────────────────────────────────────────────
    let mut node = Node::new(config.clone())?;

    // ── Build RPC AppState ────────────────────────────────────────────────────
    // The RPC server uses its own in-memory state for now.
    // A future refactor will wire the shared Arc<Mutex<Chain>> from the node.
    let rpc_state = AppState::new();
    let rpc_addr = cli.rpc_addr.clone();

    tracing::info!("vTorrent daemon starting:");
    tracing::info!("  P2P listen:      {}", config.listen_addr);
    tracing::info!("  RPC server:      {}", rpc_addr);
    tracing::info!("  Data dir:        {}", data_dir.display());
    tracing::info!("  DHT bootstrap:   {}", if config.use_dht { "enabled" } else { "disabled" });
    tracing::info!("  Staking:         {}", if staking_enabled {
        cli.staking_address.as_deref().unwrap_or("enabled")
    } else {
        "disabled"
    });

    // ── Start services concurrently ───────────────────────────────────────────
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
