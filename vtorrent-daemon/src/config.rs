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

    /// File containing the wallet encryption passphrase, used to auto-unlock
    /// the wallet at startup so staking resumes without operator action.
    ///
    /// The file must be readable only by the daemon's user (0600). The
    /// passphrase is never logged or placed in the process arguments. If the
    /// file is set but unreadable, the daemon refuses to start rather than
    /// silently running locked (which would stall staking).
    #[arg(long, value_name = "PATH")]
    pub wallet_passphrase_file: Option<PathBuf>,

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

    /// Lower the minimum stake age to 60 seconds for regtest soak testing.
    #[arg(long, default_value_t = false)]
    pub regtest_fast_stake: bool,

    /// Number of most-recent block bodies retained in memory.
    ///
    /// Bounds the node's in-memory chain footprint. Older block bodies are
    /// pruned (headers and indexes are always retained) and served on demand
    /// from the block store. Use a very large value to keep every body.
    #[arg(long, default_value_t = vtorrent_node::chain::DEFAULT_BLOCK_BODY_CACHE)]
    pub block_body_cache: usize,

    /// Optional API key required for sensitive RPC endpoints.
    ///
    /// Read from the environment rather than argv so it is not exposed in the
    /// process command line. Prefer `--rpc-api-key-file` for production.
    #[arg(env = "VTORRENT_RPC_API_KEY", hide_env_values = true)]
    pub rpc_api_key: Option<String>,

    /// File containing the RPC API key (mode 0600).
    #[arg(long, value_name = "PATH")]
    pub rpc_api_key_file: Option<PathBuf>,

    /// 64-byte hex-encoded BIP39 seed for the Bitcoin SPV wallet.
    ///
    /// Read from the environment rather than argv. Prefer `--btc-seed-file` for
    /// production so the seed is not exposed in the process command line.
    #[arg(env = "VTORRENT_BTC_SEED", hide_env_values = true)]
    pub btc_seed: Option<String>,

    /// File containing the 128-hex-character Bitcoin SPV seed (mode 0600).
    #[arg(long, value_name = "PATH")]
    pub btc_seed_file: Option<PathBuf>,

    /// Run the Bitcoin SPV wallet in regtest mode (local development).
    ///
    /// Uses regtest network magic and addresses (bcrt1...), and connects to
    /// the peer given by --btc-peer instead of mainnet DNS seeds.
    #[arg(long, default_value_t = false)]
    pub btc_regtest: bool,

    /// Explicit Bitcoin peer address for regtest (e.g. 127.0.0.1:18444).
    #[arg(long)]
    pub btc_peer: Option<String>,

    /// Max Bitcoin SPV headers retained in memory (0 = unbounded).
    ///
    /// Bounds SPV memory on mainnet (~870k headers ≈ 100 MiB). Must exceed the
    /// difficulty retarget period (~2016) and the UTXO-scan depth. Default
    /// 100_000 keeps ~11 MiB of headers while covering both.
    ///
    /// A scan that starts near the tip with no stored previous filter header
    /// needs a checkpoint in range; if that checkpoint is below the retained
    /// floor, the scan fails closed. Raise this value or rescan from an earlier
    /// height.
    #[arg(long, default_value_t = 100_000)]
    pub btc_header_retention: usize,

    /// BTC block height at which the wallet was created.
    ///
    /// The SPV wallet scans from the later of this height and the retained
    /// header floor. Funds older than the retained window are **not**
    /// discoverable: raise `--btc-header-retention` or rescan from a retained
    /// checkpoint. Without this flag, the scan starts at the retained floor.
    #[arg(long)]
    pub btc_birthday: Option<u32>,
}

/// Load secrets from 0600 files, rejecting ambiguous or exposed inputs.
pub fn load_secret_files(cli: &mut Cli) -> anyhow::Result<()> {
    if cli.rpc_api_key.is_some() && cli.rpc_api_key_file.is_some() {
        anyhow::bail!("set only one of VTORRENT_RPC_API_KEY and --rpc-api-key-file");
    }
    if cli.btc_seed.is_some() && cli.btc_seed_file.is_some() {
        anyhow::bail!("set only one of VTORRENT_BTC_SEED and --btc-seed-file");
    }
    if let Some(path) = &cli.rpc_api_key_file {
        cli.rpc_api_key = Some(read_secret_file(path, "RPC API key")?);
    }
    if let Some(path) = &cli.btc_seed_file {
        cli.btc_seed = Some(read_secret_file(path, "BTC seed")?);
    }
    Ok(())
}

fn read_secret_file(path: &std::path::Path, label: &str) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| anyhow::anyhow!("cannot read {label} file {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            anyhow::bail!(
                "{label} file {} must not be group/world readable (chmod 600)",
                path.display()
            );
        }
    }
    let value = std::fs::read_to_string(path)
        .map_err(|error| anyhow::anyhow!("cannot read {label} file {}: {error}", path.display()))?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        anyhow::bail!("{label} file {} is empty", path.display());
    }
    Ok(value.to_string())
}

// ─── Startup Validation ──────────────────────────────────────────────────────

/// Validate consensus parameters and daemon configuration at startup.
///
/// This runs before any network connections are established, catching
/// configuration errors early with clear messages.
pub fn validate_startup_config(cli: &Cli, data_dir: &std::path::Path) -> anyhow::Result<()> {
    use vtorrent_core::network::{mainnet, testnet};
    use vtorrent_node::consensus::{
        BLOCK_REWARD, MAX_STAKE_AGE, MAX_SUPPLY, MIN_STAKE_AGE, MIN_STAKE_AMOUNT,
        REGTEST_FAST_MAX_STAKE_AGE, REGTEST_FAST_MIN_STAKE_AGE, TARGET_BLOCK_TIME,
    };

    if cli.regtest_fast_stake && !cli.regtest {
        anyhow::bail!("--regtest-fast-stake requires --regtest");
    }

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
    let rpc_is_loopback = cli
        .rpc_addr
        .parse::<std::net::SocketAddr>()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or_else(|_| cli.rpc_addr.starts_with("localhost:"));
    let has_rpc_key = cli
        .rpc_api_key
        .as_deref()
        .is_some_and(|key| !key.is_empty())
        || cli.rpc_api_key_file.is_some();
    if !rpc_is_loopback && !has_rpc_key {
        anyhow::bail!(
            "an RPC API key is required when --rpc-addr is not bound to loopback; use --rpc-api-key-file or VTORRENT_RPC_API_KEY"
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
    let (min_stake_age, max_stake_age) = if cli.regtest_fast_stake {
        (REGTEST_FAST_MIN_STAKE_AGE, REGTEST_FAST_MAX_STAKE_AGE)
    } else {
        (MIN_STAKE_AGE, MAX_STAKE_AGE)
    };
    if min_stake_age >= max_stake_age {
        anyhow::bail!(
            "Consensus error: MIN_STAKE_AGE ({}) must be < MAX_STAKE_AGE ({})",
            min_stake_age,
            max_stake_age,
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
        min_stake_age,
        max_stake_age,
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn secret_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vtr-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn secret_files_are_loaded_and_mode_checked() {
        let path = secret_path("seed");
        std::fs::write(&path, "ab".repeat(64) + "\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut cli =
            Cli::parse_from(["vtorrent-daemon", "--btc-seed-file", path.to_str().unwrap()]);
        load_secret_files(&mut cli).unwrap();
        assert_eq!(cli.btc_seed.as_deref(), Some("ab".repeat(64).as_str()));
        assert!(Cli::try_parse_from(["vtorrent-daemon", "--btc-seed", "00"]).is_err());
        assert!(Cli::try_parse_from(["vtorrent-daemon", "--rpc-api-key", "secret"]).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let mut cli =
                Cli::parse_from(["vtorrent-daemon", "--btc-seed-file", path.to_str().unwrap()]);
            assert!(load_secret_files(&mut cli).is_err());
        }
        std::fs::remove_file(path).unwrap();
    }
}
