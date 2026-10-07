use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use serde_json::Value;
/// vtorrent-cli — Command-line wallet and node control tool for vTorrent.
///
/// Communicates with a running `vtorrent-daemon` via the JSON-RPC API.
///
/// ## Usage
///
/// ```text
/// vtorrent-cli [OPTIONS] <COMMAND>
///
/// Options:
///   --rpc-url <URL>   RPC server URL [default: http://127.0.0.1:22525]
///
/// Commands:
///   info                     Show node info and chain status
///   height                   Show current block height
///   block <hash>             Show block details
///   mempool                  Show mempool contents
///   balance                  Show wallet balance
///   addresses                List wallet addresses
///   send <to> <amount>       Send VTR to an address
///   unlock <passphrase>      Unlock wallet for 5 minutes
///   lock                     Lock wallet immediately
///   staking status           Show staking status
///   staking start <address>  Start staking
///   staking stop             Stop staking
///   torrent list             List active torrent sessions
///   torrent add <magnet>     Add a torrent by magnet link
///   torrent remove <id>      Remove a torrent session
///   dex orders               Show DEX order book
///   dex buy <pair> <amount>  Place a buy order
///   dex sell <pair> <amount> Place a sell order
///   dex cancel <id>          Cancel a DEX order
///   claim check <address>    Check if a legacy address has a claimable balance
///   claim submit <addr> <sig> Submit a legacy balance claim
///   metrics                  Show Prometheus metrics summary
///   peers                    List connected P2P peers
/// ```
use std::process;
use zeroize::Zeroize;

mod client;
mod format;

use client::RpcClient;

/// vTorrent command-line wallet and node control tool.
#[derive(Parser, Debug)]
#[command(
    name = "vtorrent-cli",
    about = "Command-line wallet and node control tool for vTorrent",
    version,
    author
)]
struct Cli {
    /// RPC server URL.
    #[arg(
        long,
        env = "VTORRENT_RPC_URL",
        default_value = "http://127.0.0.1:22525"
    )]
    rpc_url: String,

    /// Output raw JSON instead of formatted output.
    #[arg(long, short = 'j')]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show node info and chain status.
    Info,
    /// Show current block height.
    Height,
    /// Show block details by hash.
    Block {
        /// Block hash (64 hex characters).
        hash: String,
    },
    /// Show mempool contents.
    Mempool,
    /// Show wallet balance.
    Balance,
    /// List wallet addresses.
    Addresses,
    /// Send VTR to an address.
    Send {
        /// Destination address.
        to: String,
        /// Amount in VTR (e.g., 1.5 for 1.5 VTR).
        amount: f64,
        /// Wallet passphrase.
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Unlock wallet.
    Unlock {
        /// Wallet passphrase.
        passphrase: String,
        /// Unlock duration in seconds [default: 300].
        #[arg(long, default_value = "300")]
        timeout: u64,
    },
    /// Lock wallet immediately.
    Lock,
    /// Staking commands.
    Staking {
        #[command(subcommand)]
        action: StakingCommands,
    },
    /// Torrent commands.
    Torrent {
        #[command(subcommand)]
        action: TorrentCommands,
    },
    /// DEX (decentralized exchange) commands.
    Dex {
        #[command(subcommand)]
        action: DexCommands,
    },
    /// Legacy balance claim commands.
    Claim {
        #[command(subcommand)]
        action: ClaimCommands,
    },
    /// Bitcoin SPV wallet commands (built-in BTC side of atomic swaps).
    Btc {
        #[command(subcommand)]
        action: BtcCommands,
    },
    /// Show Prometheus metrics summary.
    Metrics,
    /// List connected P2P peers.
    Peers,
    /// Rebuild the store's derived state from its blocks (offline; stop the
    /// daemon first). Repairs a corrupt tx index / UTXO set / commitment.
    Reindex {
        /// Data directory containing `chain.db` [default: ~/.vtorrent].
        #[arg(long)]
        data_dir: Option<String>,
        /// Regtest chain (uses the regtest genesis).
        #[arg(long)]
        regtest: bool,
        /// Regtest with fast-stake parameters.
        #[arg(long)]
        regtest_fast_stake: bool,
    },
    /// Scan the chain for outputs the consensus batch would treat differently
    /// (OP_RETURN, P2CS). Zero findings ⇒ the batch is a no-op on this chain.
    CheckConsensus {
        /// Data directory containing `chain.db` [default: ~/.vtorrent].
        #[arg(long)]
        data_dir: Option<String>,
        /// Regtest chain (uses the regtest genesis).
        #[arg(long)]
        regtest: bool,
        /// Regtest with fast-stake parameters.
        #[arg(long)]
        regtest_fast_stake: bool,
    },
}

#[derive(Subcommand, Debug)]
enum BtcCommands {
    /// Show BTC SPV wallet status (sync state, balance).
    Status,
    /// Show the BTC receive address.
    Address,
    /// Send BTC to an address.
    Send {
        /// Destination BTC address.
        address: String,
        /// Amount in BTC (e.g. 0.001).
        amount: f64,
    },
}

#[derive(Subcommand, Debug)]
enum StakingCommands {
    /// Show staking status.
    Status,
    /// Start staking.
    Start {
        /// Address to receive staking rewards.
        address: String,
    },
    /// Stop staking.
    Stop,
}

#[derive(Subcommand, Debug)]
enum TorrentCommands {
    /// List active torrent sessions.
    List,
    /// Add a torrent by magnet link or .torrent URL.
    Add {
        /// Magnet link or .torrent URL.
        magnet: String,
        /// Wallet address for incentive payments.
        #[arg(long)]
        wallet: Option<String>,
    },
    /// Remove a torrent session.
    Remove {
        /// Session ID.
        id: String,
    },
}

#[derive(Subcommand, Debug)]
enum DexCommands {
    /// Show open DEX orders.
    Orders,
    /// Place a buy order (buy the quote asset with the base asset).
    Buy {
        /// Trading pair (e.g., VTR/BTC).
        pair: String,
        /// Amount of the quote asset to buy.
        amount: f64,
        /// Price in base units per quote unit.
        price: f64,
        /// Maker's base-asset address (e.g. VTR address).
        #[arg(long)]
        maker_address: String,
        /// Wallet passphrase.
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Place a sell order (sell the base asset for the quote asset).
    Sell {
        /// Trading pair (e.g., VTR/BTC).
        pair: String,
        /// Amount of the base asset to sell.
        amount: f64,
        /// Price in base units per quote unit.
        price: f64,
        /// Maker's base-asset address (e.g. VTR address).
        #[arg(long)]
        maker_address: String,
        /// Wallet passphrase.
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Cancel a DEX order.
    Cancel {
        /// Order ID.
        id: String,
    },
}

#[derive(Subcommand, Debug)]
enum ClaimCommands {
    /// Check if a legacy address has a claimable balance.
    Check {
        /// Legacy vTorrent 1.x address.
        address: String,
    },
    /// Submit a legacy balance claim.
    Submit {
        /// Legacy vTorrent 1.x WIF-encoded private key, or "-" to read it
        /// from stdin (recommended: keeps the key out of `ps` and history).
        wif: String,
        /// New vTorrent 2.0 destination address.
        destination: String,
    },
}

fn main() {
    let cli = Cli::parse();
    // The daemon's --rpc-api-key gates wallet/staking/DEX/claim/broadcast
    // endpoints; without the header those calls fail with 401.
    let api_key = std::env::var("VTORRENT_RPC_API_KEY").ok();
    let client = RpcClient::new(cli.rpc_url.clone(), api_key);

    let result = run_command(&cli, &client);
    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            process::exit(1);
        }
    }
}

/// Convert a decimal asset amount to satoshis (8 decimal places).
fn to_sats(units: f64) -> u64 {
    (units * 100_000_000.0).round() as u64
}

/// Parse a "BASE/QUOTE" trading pair into its two assets.
fn parse_pair(pair: &str) -> Result<(&str, &str)> {
    let mut parts = pair.splitn(2, '/');
    let base = parts.next().unwrap_or("").trim();
    let quote = parts.next().unwrap_or("").trim();
    if base.is_empty() || quote.is_empty() {
        return Err(anyhow::anyhow!(
            "Invalid trading pair '{}' (expected BASE/QUOTE, e.g. VTR/BTC)",
            pair
        ));
    }
    Ok((base, quote))
}

fn run_command(cli: &Cli, client: &RpcClient) -> Result<()> {
    match &cli.command {
        Commands::Info => {
            let data = client.get("/api/v1/info")?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                format::print_node_info(&data);
            }
        }

        Commands::Height => {
            let data = client.get("/api/v1/blockchain/height")?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                let height = data["height"].as_u64().unwrap_or(0);
                println!(
                    "{} {}",
                    "Block height:".cyan().bold(),
                    height.to_string().white().bold()
                );
            }
        }

        Commands::Block { hash } => {
            let data = client.get(&format!("/api/v1/blockchain/block/{}", hash))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                format::print_block(&data);
            }
        }

        Commands::Mempool => {
            let data = client.get("/api/v1/mempool")?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                format::print_mempool(&data);
            }
        }

        Commands::Balance => {
            let data = client.get("/api/v1/wallet/balance")?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                format::print_balance(&data);
            }
        }

        Commands::Addresses => {
            let data = client.get("/api/v1/wallet/addresses")?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                format::print_addresses(&data);
            }
        }

        Commands::Send {
            to,
            amount,
            passphrase,
        } => {
            let amount_sats = (amount * 100_000_000.0) as u64;
            let passphrase = passphrase.clone().unwrap_or_else(|| {
                rpassword::prompt_password("Wallet passphrase: ").unwrap_or_default()
            });
            let payload = serde_json::json!({
                "to_address": to,
                "amount_satoshis": amount_sats,
                "passphrase": passphrase,
            });
            let data = client.post("/api/v1/wallet/send", &payload);
            let mut passphrase = passphrase;
            passphrase.zeroize();
            let data = data?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                let txid = data["txid"].as_str().unwrap_or("unknown");
                let fee = data["fee_satoshis"].as_u64().unwrap_or(0);
                println!("{} {}", "Sent! TXID:".green().bold(), txid.white());
                if fee > 0 {
                    println!("  Fee: {} sats", fee.to_string().dimmed());
                }
            }
        }

        Commands::Unlock {
            passphrase,
            timeout,
        } => {
            let payload = serde_json::json!({
                "passphrase": passphrase,
                "timeout_secs": timeout,
            });
            let data = client.post("/api/v1/wallet/unlock", &payload);
            let mut owned = passphrase.clone();
            owned.zeroize();
            let data = data?;
            if data["success"].as_bool().unwrap_or(false) {
                println!("{}", "Wallet unlocked.".green().bold());
            } else {
                return Err(anyhow::anyhow!("Failed to unlock wallet"));
            }
        }

        Commands::Lock => {
            let data = client.post("/api/v1/wallet/lock", &serde_json::json!({}))?;
            if data["success"].as_bool().unwrap_or(false) {
                println!("{}", "Wallet locked.".yellow().bold());
            }
        }

        Commands::Staking { action } => match action {
            StakingCommands::Status => {
                let data = client.get("/api/v1/staking/status")?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    format::print_staking_status(&data);
                }
            }
            StakingCommands::Start { address } => {
                // Staking requires an unlocked wallet (the coinstake is signed
                // with the hot-wallet key). Prompt for the passphrase, unlock,
                // then start staking.
                let passphrase =
                    rpassword::prompt_password("Wallet passphrase: ").unwrap_or_default();
                let unlock = client.post(
                    "/api/v1/wallet/unlock",
                    &serde_json::json!({
                        "passphrase": passphrase,
                        "timeout_secs": 300,
                    }),
                );
                let mut passphrase = passphrase;
                passphrase.zeroize();
                let unlock = unlock?;
                if !unlock["success"].as_bool().unwrap_or(false) {
                    return Err(anyhow::anyhow!("Failed to unlock wallet"));
                }
                let payload = serde_json::json!({ "address": address });
                let data = client.post("/api/v1/staking/start", &payload)?;
                if data["success"].as_bool().unwrap_or(false) {
                    println!(
                        "{} {}",
                        "Staking started on address:".green().bold(),
                        address.white()
                    );
                } else {
                    return Err(anyhow::anyhow!("Failed to start staking"));
                }
            }
            StakingCommands::Stop => {
                let data = client.post("/api/v1/staking/stop", &serde_json::json!({}))?;
                if data["success"].as_bool().unwrap_or(false) {
                    println!("{}", "Staking stopped.".yellow().bold());
                }
            }
        },

        Commands::Torrent { action } => match action {
            TorrentCommands::List => {
                let data = client.get("/api/v1/torrent/sessions")?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    format::print_torrent_sessions(&data);
                }
            }
            TorrentCommands::Add { magnet, wallet } => {
                let payload = serde_json::json!({
                    "source": magnet,
                    "source_type": "magnet",
                    "wallet_address": wallet.clone().unwrap_or_default(),
                });
                let data = client.post("/api/v1/torrent/add", &payload)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    let id = data["session_id"].as_str().unwrap_or("unknown");
                    println!(
                        "{} {}",
                        "Torrent added. Session ID:".green().bold(),
                        id.white()
                    );
                }
            }
            TorrentCommands::Remove { id } => {
                client.delete(&format!("/api/v1/torrent/{}", id))?;
                println!(
                    "{} {}",
                    "Removed torrent session:".yellow().bold(),
                    id.white()
                );
            }
        },

        Commands::Dex { action } => match action {
            DexCommands::Orders => {
                let data = client.get("/api/v1/dex/orders")?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    format::print_dex_orders(&data);
                }
            }
            DexCommands::Buy {
                pair,
                amount,
                price,
                maker_address,
                passphrase,
            } => {
                let (base, quote) = parse_pair(pair)?;
                let passphrase = passphrase.clone().unwrap_or_else(|| {
                    rpassword::prompt_password("Wallet passphrase: ").unwrap_or_default()
                });
                // Buying `amount` of the quote asset at `price` base/quote:
                // offer `amount * price` of the base asset, request `amount` quote.
                let payload = serde_json::json!({
                    "maker_address": maker_address,
                    "offer_amount_satoshis": to_sats(amount * price),
                    "offer_asset": base,
                    "request_amount_satoshis": to_sats(*amount),
                    "request_asset": quote,
                    "expiry_secs": 0,
                    "passphrase": passphrase,
                });
                let data = client.post("/api/v1/dex/order", &payload);
                let mut passphrase = passphrase;
                passphrase.zeroize();
                let data = data?;
                let id = data["order_id"].as_str().unwrap_or("unknown");
                println!(
                    "{} {} (ID: {})",
                    "Buy order placed:".green().bold(),
                    format!("{} {} @ {}", amount, pair, price).white(),
                    id.dimmed()
                );
            }
            DexCommands::Sell {
                pair,
                amount,
                price,
                maker_address,
                passphrase,
            } => {
                let (base, quote) = parse_pair(pair)?;
                let passphrase = passphrase.clone().unwrap_or_else(|| {
                    rpassword::prompt_password("Wallet passphrase: ").unwrap_or_default()
                });
                // Selling `amount` of the base asset at `price` base/quote:
                // offer `amount` of the base asset, request `amount / price` quote.
                let payload = serde_json::json!({
                    "maker_address": maker_address,
                    "offer_amount_satoshis": to_sats(*amount),
                    "offer_asset": base,
                    "request_amount_satoshis": to_sats(amount / price),
                    "request_asset": quote,
                    "expiry_secs": 0,
                    "passphrase": passphrase,
                });
                let data = client.post("/api/v1/dex/order", &payload);
                let mut passphrase = passphrase;
                passphrase.zeroize();
                let data = data?;
                let id = data["order_id"].as_str().unwrap_or("unknown");
                println!(
                    "{} {} (ID: {})",
                    "Sell order placed:".green().bold(),
                    format!("{} {} @ {}", amount, pair, price).white(),
                    id.dimmed()
                );
            }
            DexCommands::Cancel { id } => {
                client.delete(&format!("/api/v1/dex/order/{}", id))?;
                println!("{} {}", "Order cancelled:".yellow().bold(), id.white());
            }
        },

        Commands::Btc { action } => match action {
            BtcCommands::Status => {
                let data = client.get("/api/v1/btc/status")?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    format::print_btc_status(&data);
                }
            }
            BtcCommands::Address => {
                let data = client.get("/api/v1/btc/address")?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    println!("BTC address: {}", data["address"].as_str().unwrap_or("?"));
                }
            }
            BtcCommands::Send { address, amount } => {
                let amount_satoshis = (amount * 100_000_000.0).round() as u64;
                let payload = serde_json::json!({
                    "to_address": address,
                    "amount_satoshis": amount_satoshis,
                });
                let data = client.post("/api/v1/btc/send", &payload)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    println!("BTC txid: {}", data["txid"].as_str().unwrap_or("?"));
                }
            }
        },
        Commands::Claim { action } => match action {
            ClaimCommands::Check { address } => {
                let payload = serde_json::json!({ "legacy_address": address });
                let data = client.post("/api/v1/claim/check", &payload)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    format::print_claim_check(&data);
                }
            }
            ClaimCommands::Submit { wif, destination } => {
                // Never leave key material in argv-visible memory longer than
                // needed: "-" reads the WIF from stdin (pipe/redirection keeps
                // it out of `ps` output and shell history).
                let mut wif = if wif == "-" {
                    rpassword::prompt_password("Legacy WIF: ").unwrap_or_default()
                } else {
                    wif.clone()
                };
                let payload = serde_json::json!({
                    "wif_private_key": wif,
                    "recipient_address": destination,
                });
                wif.zeroize();
                let data = client.post("/api/v1/claim/submit", &payload)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    let txid = data["txid"].as_str().unwrap_or("unknown");
                    let claimed = data["claimed_satoshis"].as_u64().unwrap_or(0);
                    println!(
                        "{} {} ({} sats)",
                        "Claim submitted! TXID:".green().bold(),
                        txid.white(),
                        claimed.to_string().dimmed()
                    );
                }
            }
        },

        Commands::Metrics => {
            let text = client.get_text("/metrics")?;
            if cli.json {
                // Parse metrics into JSON for --json mode
                let mut map = serde_json::Map::new();
                for line in text.lines() {
                    if !line.starts_with('#') && !line.is_empty() {
                        let parts: Vec<&str> = line.splitn(2, ' ').collect();
                        if parts.len() == 2 {
                            if let Ok(v) = parts[1].parse::<u64>() {
                                map.insert(parts[0].to_string(), Value::Number(v.into()));
                            }
                        }
                    }
                }
                println!("{}", serde_json::to_string_pretty(&Value::Object(map))?);
            } else {
                format::print_metrics(&text);
            }
        }

        Commands::Peers => {
            let data = client.get("/api/v1/peers")?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                format::print_peers(&data);
            }
        }
        Commands::Reindex {
            data_dir,
            regtest,
            regtest_fast_stake,
        } => {
            run_reindex(data_dir.as_deref(), *regtest, *regtest_fast_stake)?;
        }
        Commands::CheckConsensus {
            data_dir,
            regtest,
            regtest_fast_stake,
        } => {
            run_check_consensus(data_dir.as_deref(), *regtest, *regtest_fast_stake)?;
        }
    }
    Ok(())
}

/// Scan the chain for outputs the consensus batch would treat differently
/// (OP_RETURN, P2CS). Zero findings ⇒ the batch is a no-op on this chain.
/// Offline (reads the store directly). See `docs/consensus-batch-activation-plan.md`.
fn run_check_consensus(
    data_dir: Option<&str>,
    regtest: bool,
    regtest_fast_stake: bool,
) -> Result<()> {
    use vtorrent_store::store::BlockStore;

    let dir = match data_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => dirs_data_dir()?,
    };
    let db_path = dir.join("chain.db");
    if !db_path.exists() {
        anyhow::bail!(
            "no store at {} — is the data dir correct?",
            db_path.display()
        );
    }
    let store =
        BlockStore::open(&db_path).map_err(|e| anyhow::anyhow!("failed to open store: {e}"))?;
    let tip = store.best_height().unwrap_or(0);
    let chain = if regtest_fast_stake {
        store.load_into_fast_regtest_chain()
    } else if regtest {
        store.load_into_regtest_chain()
    } else {
        store.load_into_chain()
    }
    .map_err(|e| anyhow::anyhow!("failed to load chain: {e}"))?;

    let mut op_return = 0u64;
    let mut p2cs = 0u64;
    for h in 0..=tip {
        let Some(block) = chain.get_block_at_height(h) else {
            continue;
        };
        for tx in &block.transactions {
            for out in &tx.outputs {
                let Ok(script) = vtorrent_script::Script::from_bytes(out.script_pubkey.clone())
                else {
                    continue;
                };
                match vtorrent_script::classify_script(&script) {
                    vtorrent_script::ScriptType::OpReturn => op_return += 1,
                    vtorrent_script::ScriptType::P2CS { .. } => p2cs += 1,
                    _ => {}
                }
            }
        }
    }

    println!("Consensus-batch no-op check (heights 0..={tip}):");
    println!("  OP_RETURN outputs: {op_return}");
    println!("  P2CS outputs:      {p2cs}");
    if op_return == 0 && p2cs == 0 {
        println!("  => NO-OP: the batch changes nothing on this chain (safe to adopt via a coordinated upgrade).");
    } else {
        println!("  => NOT a no-op: {op_return} OP_RETURN / {p2cs} P2CS outputs exist; a height activation or fresh genesis is required.");
    }
    Ok(())
}

/// Rebuild the store's derived state from its own blocks. Offline: the daemon
/// must be stopped so the store lock is free.
///
/// The store's `rebuild_from_blocks` replays a **contiguous genesis→tip** list
/// (height = index), so this is a full reindex — the correct, self-consistent
/// operation. A checkpoint-based partial reindex would need a store API that
/// rebuilds from a non-zero height; that is a documented follow-up.
fn run_reindex(data_dir: Option<&str>, regtest: bool, regtest_fast_stake: bool) -> Result<()> {
    use vtorrent_store::store::BlockStore;

    let dir = match data_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => dirs_data_dir()?,
    };
    let db_path = dir.join("chain.db");
    if !db_path.exists() {
        anyhow::bail!(
            "no store at {} — is the data dir correct?",
            db_path.display()
        );
    }
    println!("Reindexing {} (full, from genesis)…", db_path.display());

    let store =
        BlockStore::open(&db_path).map_err(|e| anyhow::anyhow!("failed to open store: {e}"))?;
    let tip = store.best_height().unwrap_or(0);

    // Load the chain (bodies from the store) and replay genesis→tip.
    let chain = if regtest_fast_stake {
        store.load_into_fast_regtest_chain()
    } else if regtest {
        store.load_into_regtest_chain()
    } else {
        store.load_into_chain()
    }
    .map_err(|e| anyhow::anyhow!("failed to load chain: {e}"))?;
    let blocks: Vec<vtorrent_node::block::Block> = (0..=tip)
        .filter_map(|h| chain.get_block_at_height(h).cloned())
        .collect();

    if blocks.len() as u32 != tip + 1 {
        anyhow::bail!(
            "chain is not contiguous: got {} blocks for tip {} — refusing to reindex",
            blocks.len(),
            tip
        );
    }

    let result = if regtest_fast_stake {
        store.rebuild_from_fast_regtest_blocks(&blocks)
    } else if regtest {
        store.rebuild_from_regtest_blocks(&blocks)
    } else {
        store.rebuild_from_blocks(&blocks)
    };
    result.map_err(|e| anyhow::anyhow!("reindex failed: {e}"))?;

    let new_tip = store.best_height().unwrap_or(0);
    if new_tip != tip {
        anyhow::bail!("reindex produced tip {new_tip}, expected {tip} — store may be corrupt");
    }
    println!(
        "Reindex complete: {} block(s) replayed, tip {tip}.",
        blocks.len()
    );
    Ok(())
}

/// Default data dir (`~/.vtorrent`), matching the daemon.
fn dirs_data_dir() -> Result<std::path::PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    Ok(std::path::PathBuf::from(home).join(".vtorrent"))
}
