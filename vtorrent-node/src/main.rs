/// vTorrent Node — standalone P2P node binary.
///
/// Starts the P2P networking layer (peer discovery, block sync, PoS staking).
/// For the full daemon with RPC server, use `vtorrent-daemon`.
///
/// Usage:
///   vtorrent-node [OPTIONS]
///
/// Options:
///   --listen <ADDR>           P2P listen address [default: 0.0.0.0:22526]
///   --data-dir <PATH>         Node data directory [default: ~/.vtorrent]
///   --staking-address <ADDR>  Enable PoS staking with this address
///   --no-dht                  Disable DHT bootstrap (use DNS seeds only)
///   --seed <ADDR>             Additional seed node (repeatable)
///   --log-level <LEVEL>       Log level: error|warn|info|debug|trace [default: info]

use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vtorrent_node::node::{Node, NodeConfig};

// ─── CLI Arguments ────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "vtorrent-node",
    version = "2.0.0",
    about = "vTorrent Node — P2P networking layer",
    long_about = None
)]
struct Cli {
    /// P2P listen address.
    #[arg(long, default_value = "0.0.0.0:22526")]
    listen: String,

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

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&cli.log_level))
        )
        .init();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║              vTorrent Node v2.0.0                        ║");
    println!("║  Decentralized • Incentivized • Exchange-Free            ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    let data_dir = cli.data_dir.unwrap_or_else(|| {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".vtorrent")
    });

    std::fs::create_dir_all(&data_dir)?;

    let staking_enabled = cli.staking_address.is_some();
    let config = NodeConfig {
        listen_addr: cli.listen.clone(),
        staking_enabled,
        staking_address: cli.staking_address.clone(),
        max_mempool: 10_000,
        extra_seeds: cli.seeds.clone(),
        use_dht: !cli.no_dht,
        data_dir: data_dir.clone(),
    };

    let mut node = Node::new(config.clone())?;

    tracing::info!("P2P node starting on {}", config.listen_addr);
    tracing::info!("Data dir: {}", data_dir.display());
    tracing::info!("DHT: {} | Staking: {}",
        if config.use_dht { "on" } else { "off" },
        if staking_enabled { cli.staking_address.as_deref().unwrap_or("on") } else { "off" }
    );

    tokio::select! {
        result = node.start() => {
            if let Err(e) = result {
                tracing::error!("Node error: {}", e);
            }
        }
        _ = shutdown_signal() => {
            tracing::info!("Shutdown signal received");
        }
    }

    tracing::info!("vTorrent node stopped.");
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
