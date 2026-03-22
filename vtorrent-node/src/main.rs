/// vTorrent Node — main entry point.

use tracing_subscriber::EnvFilter;
use vtorrent_node::chain::Chain;
use vtorrent_node::consensus::{COIN, MAX_SUPPLY};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info"))
        )
        .init();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║              vTorrent Node v2.0.0                        ║");
    println!("║  Decentralized • Incentivized • Exchange-Free            ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    // Initialize the chain
    let chain = Chain::new()?;
    tracing::info!(
        "Chain started at height {} | Max supply: {} VTR",
        chain.best_height(),
        MAX_SUPPLY / COIN
    );

    // TODO: Start P2P networking
    // TODO: Start RPC server
    // TODO: Start staking if wallet is unlocked

    println!("Node initialized. P2P networking coming in next build.");
    println!("Genesis block hash: {}", hex::encode(chain.best_hash().unwrap_or([0u8; 32])));

    Ok(())
}
