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
    /// Show Prometheus metrics summary.
    Metrics,
    /// List connected P2P peers.
    Peers,
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
    /// Place a buy order.
    Buy {
        /// Trading pair (e.g., VTR/BTC).
        pair: String,
        /// Amount to buy.
        amount: f64,
        /// Price per unit.
        price: f64,
    },
    /// Place a sell order.
    Sell {
        /// Trading pair (e.g., VTR/BTC).
        pair: String,
        /// Amount to sell.
        amount: f64,
        /// Price per unit.
        price: f64,
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
        /// Legacy vTorrent 1.x address.
        address: String,
        /// Signature proving ownership (from signmessage in old wallet).
        signature: String,
        /// New vTorrent 2.0 destination address.
        destination: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let client = RpcClient::new(cli.rpc_url.clone());

    let result = run_command(&cli, &client);
    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            process::exit(1);
        }
    }
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
            let data = client.post("/api/v1/wallet/send", &payload)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else if data["success"].as_bool().unwrap_or(false) {
                let txid = data["txid"].as_str().unwrap_or("unknown");
                println!("{} {}", "Sent! TXID:".green().bold(), txid.white());
            } else {
                let err = data["error"].as_str().unwrap_or("unknown error");
                return Err(anyhow::anyhow!("Send failed: {}", err));
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
            let data = client.post("/api/v1/wallet/unlock", &payload)?;
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
                    "magnet": magnet,
                    "wallet_address": wallet,
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
            } => {
                let payload = serde_json::json!({
                    "pair": pair,
                    "side": "buy",
                    "amount": amount,
                    "price": price,
                });
                let data = client.post("/api/v1/dex/order", &payload)?;
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
            } => {
                let payload = serde_json::json!({
                    "pair": pair,
                    "side": "sell",
                    "amount": amount,
                    "price": price,
                });
                let data = client.post("/api/v1/dex/order", &payload)?;
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

        Commands::Claim { action } => match action {
            ClaimCommands::Check { address } => {
                let payload = serde_json::json!({ "address": address });
                let data = client.post("/api/v1/claim/check", &payload)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else {
                    format::print_claim_check(&data);
                }
            }
            ClaimCommands::Submit {
                address,
                signature,
                destination,
            } => {
                let payload = serde_json::json!({
                    "legacy_address": address,
                    "signature": signature,
                    "destination_address": destination,
                });
                let data = client.post("/api/v1/claim/submit", &payload)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&data)?);
                } else if data["success"].as_bool().unwrap_or(false) {
                    let txid = data["txid"].as_str().unwrap_or("unknown");
                    println!(
                        "{} {}",
                        "Claim submitted! TXID:".green().bold(),
                        txid.white()
                    );
                } else {
                    let err = data["error"].as_str().unwrap_or("unknown error");
                    return Err(anyhow::anyhow!("Claim failed: {}", err));
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
    }
    Ok(())
}
