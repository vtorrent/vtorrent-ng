//! CLI argument parsing and startup configuration validation.

use std::path::PathBuf;

use clap::Parser;

// ─── CLI Arguments ────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "vtorrent-daemon",
    version = "2.0.0",
    about = "vTorrent Daemon — Decentralized • Incentivized • Exchange-Free",
    long_about = None
)]
pub struct Cli {
    /// P2P listen address.
    #[arg(long, default_value = "0.0.0.0:22526")]
    pub listen: String,

    /// RPC server bind address (localhost only by default for security).
    #[arg(long, default_value = "127.0.0.1:22525")]
    pub rpc_addr: String,

    /// Node data directory (peer cache, chain data, wallet).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Enable PoS staking with this wallet address.
    #[arg(long)]
    pub staking_address: Option<String>,

    /// WIF-encoded private key used to sign coinstake inputs.
    ///
    /// Required for staking; without it the node can find stake kernels but
    /// the resulting blocks would be rejected by the chain's script
    /// verification.
    #[arg(long, env = "VTORRENT_STAKING_WIF")]
    pub staking_wif: Option<String>,

    /// Disable DHT bootstrap (use DNS seeds only).
    #[arg(long, default_value_t = false)]
    pub no_dht: bool,

    /// Run fully isolated: skip all internet bootstrap (DHT, DoH, DNS seeds,
    /// GitHub peer list) and only connect to explicit --seed peers. For local
    /// testnets that must never reach production seed nodes.
    #[arg(long, default_value_t = false)]
    pub isolated: bool,

    /// This node's public `ip:port` as seen by peers. Prevents self-dials
    /// when listening on 0.0.0.0 (PEX-learned addresses matching it are
    /// filtered). Recommended for public seed nodes.
    #[arg(long, value_name = "IP:PORT")]
    pub public_addr: Option<String>,

    /// Additional seed nodes to connect to (can be repeated).
    #[arg(long = "seed", value_name = "ADDR")]
    pub seeds: Vec<String>,

    /// Tor SOCKS5 proxy address. Tor remains optional unless an onion peer is dialed.
    #[arg(long, value_name = "ADDR")]
    pub tor_proxy: Option<String>,

    /// Prefer Tor for outbound clearnet peers when the proxy is available.
    #[arg(long, default_value_t = false)]
    pub tor_only: bool,

    /// Enable I2P using this SAM bridge address (for example, 127.0.0.1:7656).
    #[arg(long, value_name = "ADDR")]
    pub i2p_sam: Option<String>,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Run in testnet mode.
    ///
    /// Enables private/RFC1918 address acceptance in PEX, allowing multi-node
    /// testing on a single machine or LAN without public IP addresses.
    #[arg(long, default_value_t = false)]
    pub testnet: bool,

    /// Run in regtest mode (local development).
    ///
    /// Enables a faucet RPC endpoint that mints coins to arbitrary addresses,
    /// so the full wallet/DEX/swap flow can be exercised locally without real
    /// coins or a legacy claim.
    #[arg(long, default_value_t = false)]
    pub regtest: bool,

    /// Lower stake age for regtest soak testing (60s min, 1h max).
    #[arg(long, default_value_t = false)]
    pub regtest_fast_stake: bool,

    /// Optional API key required for sensitive RPC endpoints.
    ///
    /// When set, wallet, staking, torrent, DEX, claim and broadcast endpoints
    /// reject requests that do not include the matching `X-API-Key` header.
    /// Read-only info endpoints remain open.
    #[arg(long, env = "VTORRENT_RPC_API_KEY")]
    pub rpc_api_key: Option<String>,

    /// 64-byte hex-encoded BIP39 seed for the Bitcoin SPV wallet.
    ///
    /// Required for cross-chain atomic swaps: the BTC side of an HTLC is
    /// funded, claimed, and refunded with keys derived from this seed. Without
    /// it the BTC wallet stays uninitialized and swap settlement is disabled.
    #[arg(long, env = "VTORRENT_BTC_SEED")]
    pub btc_seed: Option<String>,

    /// Run the Bitcoin SPV wallet in regtest mode (local development).
    ///
    /// Uses regtest network magic and addresses (bcrt1...), and connects to
    /// the peer given by --btc-peer instead of mainnet DNS seeds.
    #[arg(long, default_value_t = false)]
    pub btc_regtest: bool,

    /// Explicit Bitcoin peer address for regtest (e.g. 127.0.0.1:18444).
    #[arg(long)]
    pub btc_peer: Option<String>,
}

// ─── Startup Validation ──────────────────────────────────────────────────────

/// Validate consensus parameters and daemon configuration at startup.
///
/// This runs before any network connections are established, catching
/// configuration errors early with clear messages.
pub fn validate_startup_config(cli: &Cli, data_dir: &std::path::Path) -> anyhow::Result<()> {
    use vtorrent_core::network::{mainnet, testnet};
    use vtorrent_node::consensus::{
        BLOCK_REWARD, MAX_STAKE_AGE, MAX_SUPPLY, MIN_STAKE_AGE, MIN_STAKE_AMOUNT, TARGET_BLOCK_TIME,
    };

    // ── 1. Network magic consistency ──────────────────────────────────────────
    //
    // The compiled P2P magic (vtorrent_p2p::message::NETWORK_MAGIC) must match
    // the expected magic for the chosen network mode.
    let expected_magic = if cli.regtest {
        // Regtest uses mainnet magic (same chain, local faucet).
        mainnet::NETWORK_MAGIC
    } else if cli.testnet {
        testnet::NETWORK_MAGIC
    } else {
        mainnet::NETWORK_MAGIC
    };

    // The P2P crate compiles with a hardcoded magic — verify it matches.
    // (Comparing against the core mainnet constant instead was a tautology
    // for mainnet/regtest and an unconditional failure for testnet.)
    let compiled_magic = vtorrent_p2p::message::NETWORK_MAGIC;
    if compiled_magic != expected_magic {
        anyhow::bail!(
            "Network magic mismatch: compiled magic {:02x?} does not match expected {:02x?} for {} mode",
            compiled_magic,
            expected_magic,
            if cli.regtest { "regtest" } else if cli.testnet { "testnet" } else { "mainnet" },
        );
    }
    tracing::info!(
        "Network magic validated: {:02x?} ({})",
        expected_magic,
        if cli.regtest {
            "regtest"
        } else if cli.testnet {
            "testnet"
        } else {
            "mainnet"
        }
    );

    // ── 2. Port sanity ────────────────────────────────────────────────────────
    //
    // --listen and --rpc-addr must use different ports to avoid bind conflicts.
    let parse_port = |addr: &str| -> anyhow::Result<u16> {
        addr.rsplit(':')
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing port in '{}'", addr))?
            .parse::<u16>()
            .map_err(|e| anyhow::anyhow!("invalid port in '{}': {}", addr, e))
    };

    let listen_port = parse_port(&cli.listen)?;
    let rpc_port = parse_port(&cli.rpc_addr)?;

    if listen_port == rpc_port {
        anyhow::bail!(
            "Port conflict: --listen ({}) and --rpc-addr ({}) use the same port {}",
            cli.listen,
            cli.rpc_addr,
            listen_port,
        );
    }
    tracing::info!(
        "Port sanity check passed: P2P={}, RPC={}",
        listen_port,
        rpc_port
    );

    // ── 3. Consensus parameter sanity ─────────────────────────────────────────
    //
    // Static checks that critical constants have sensible values.
    if MIN_STAKE_AMOUNT == 0 {
        anyhow::bail!("Consensus error: MIN_STAKE_AMOUNT must be > 0");
    }
    if MIN_STAKE_AGE >= MAX_STAKE_AGE {
        anyhow::bail!(
            "Consensus error: MIN_STAKE_AGE ({}) must be < MAX_STAKE_AGE ({})",
            MIN_STAKE_AGE,
            MAX_STAKE_AGE,
        );
    }
    if TARGET_BLOCK_TIME == 0 {
        anyhow::bail!("Consensus error: TARGET_BLOCK_TIME must be > 0");
    }
    if MAX_SUPPLY == 0 {
        anyhow::bail!("Consensus error: MAX_SUPPLY must be > 0");
    }
    if BLOCK_REWARD == 0 {
        anyhow::bail!("Consensus error: BLOCK_REWARD must be > 0");
    }
    tracing::info!(
        "Consensus parameters validated: MIN_STAKE_AMOUNT={}, MIN_STAKE_AGE={}s, MAX_STAKE_AGE={}s, TARGET_BLOCK_TIME={}s, MAX_SUPPLY={}, BLOCK_REWARD={}",
        MIN_STAKE_AMOUNT,
        MIN_STAKE_AGE,
        MAX_STAKE_AGE,
        TARGET_BLOCK_TIME,
        MAX_SUPPLY,
        BLOCK_REWARD,
    );

    // ── 4. Data directory writability ──────────────────────────────────────────
    //
    // Ensure the data directory exists (or can be created) and is writable by
    // creating a temporary file and immediately removing it.
    std::fs::create_dir_all(data_dir)
        .map_err(|e| anyhow::anyhow!("Cannot create data directory {:?}: {}", data_dir, e))?;

    let test_file = data_dir.join(".vtorrent_write_test");
    std::fs::write(&test_file, b"ok")
        .map_err(|e| anyhow::anyhow!("Data directory {:?} is not writable: {}", data_dir, e))?;
    let _ = std::fs::remove_file(&test_file);
    tracing::info!("Data directory validated: {:?} (writable)", data_dir);

    Ok(())
}
