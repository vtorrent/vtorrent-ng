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
///   --tor-proxy <ADDR>        Tor SOCKS5 proxy address [default: 127.0.0.1:9050]
///   --tor-only                 Prefer Tor for clearnet outbound peers
///   --i2p-sam <ADDR>           Enable I2P through this SAM bridge address
///   --log-level <LEVEL>       Log level: error|warn|info|debug|trace [default: info]
use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vtorrent_node::node::{Node, NodeConfig};
use vtorrent_onion::TransportConfig;

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

    /// Skip all internet bootstrap; only connect to explicit --seed peers.
    #[arg(long, default_value_t = false)]
    isolated: bool,

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

    /// Run in testnet mode (private/RFC1918 addresses accepted in PEX).
    #[arg(long, default_value_t = false)]
    testnet: bool,
}

// ─── Entry Point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
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
        staking_wif: None,
        max_mempool: 10_000,
        extra_seeds: cli.seeds.clone(),
        use_dht: !cli.no_dht,
        isolated: cli.isolated,
        data_dir: data_dir.clone(),
        use_overlay: true,
        testnet: cli.testnet,
        regtest: false,
        transport,
    };

    let mut node = Node::new(config.clone())?;

    tracing::info!("P2P node starting on {}", config.listen_addr);
    tracing::info!("Data dir: {}", data_dir.display());
    tracing::info!(
        "DHT: {} | Staking: {}",
        if config.use_dht { "on" } else { "off" },
        if staking_enabled {
            cli.staking_address.as_deref().unwrap_or("on")
        } else {
            "off"
        }
    );
    tracing::info!(
        tor_proxy = %config.transport.tor_socks_addr,
        tor_preferred = config.transport.prefer_onion,
        i2p_enabled = config.transport.i2p_enabled,
        i2p_sam = %config.transport.i2p_sam_addr,
        "Outbound anonymous transport configured"
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
