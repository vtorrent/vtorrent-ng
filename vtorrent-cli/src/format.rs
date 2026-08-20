/// Pretty-print formatters for vtorrent-cli terminal output.
use colored::Colorize;
use serde_json::Value;

/// Print a key-value pair in a consistent style.
fn kv(key: &str, value: &str) {
    println!("  {:.<30} {}", format!("{} ", key).cyan(), value.white());
}

/// Format satoshis as VTR with 8 decimal places.
fn sats_to_vtr(sats: u64) -> String {
    format!("{:.8} VTR", sats as f64 / 100_000_000.0)
}

/// Print node info.
pub fn print_node_info(data: &Value) {
    println!("\n{}", "=== vTorrent Node Info ===".cyan().bold());
    kv("Network", data["network"].as_str().unwrap_or("unknown"));
    kv("Version", data["version"].as_str().unwrap_or("unknown"));
    kv(
        "Block height",
        &data["block_height"].as_u64().unwrap_or(0).to_string(),
    );
    kv(
        "Best hash",
        data["best_block_hash"].as_str().unwrap_or("none"),
    );
    kv(
        "Peers",
        &data["connections"].as_u64().unwrap_or(0).to_string(),
    );
    kv(
        "Syncing",
        if data["syncing"].as_bool().unwrap_or(false) {
            "yes"
        } else {
            "no"
        },
    );
    kv(
        "Uptime",
        &format_uptime(data["uptime_secs"].as_u64().unwrap_or(0)),
    );
    println!();
}

/// Print block details.
pub fn print_block(data: &Value) {
    println!("\n{}", "=== Block ===".cyan().bold());
    kv("Hash", data["hash"].as_str().unwrap_or("unknown"));
    kv("Height", &data["height"].as_u64().unwrap_or(0).to_string());
    kv("Timestamp", data["timestamp"].as_str().unwrap_or("unknown"));
    kv(
        "Tx count",
        &data["tx_count"].as_u64().unwrap_or(0).to_string(),
    );
    kv(
        "Size",
        &format!("{} bytes", data["size_bytes"].as_u64().unwrap_or(0)),
    );
    kv("Prev hash", data["prev_hash"].as_str().unwrap_or("none"));
    kv(
        "Merkle root",
        data["merkle_root"].as_str().unwrap_or("none"),
    );
    println!();
}

/// Print mempool summary.
pub fn print_mempool(data: &Value) {
    let count = data["count"].as_u64().unwrap_or(0);
    println!("\n{}", "=== Mempool ===".cyan().bold());
    kv("Transactions", &count.to_string());
    kv(
        "Size",
        &format!("{} bytes", data["size_bytes"].as_u64().unwrap_or(0)),
    );

    if let Some(txids) = data["txids"].as_array() {
        if !txids.is_empty() {
            println!("\n  {}", "Recent transaction ids:".dimmed());
            for txid in txids.iter().take(10) {
                let txid = txid.as_str().unwrap_or("unknown");
                let short = &txid[..txid.len().min(16)];
                println!("    {} {}", "•".dimmed(), short.white());
            }
            if txids.len() > 10 {
                println!("    {} {} more...", "•".dimmed(), txids.len() - 10);
            }
        }
    }
    println!();
}

/// Print wallet balance.
pub fn print_balance(data: &Value) {
    println!("\n{}", "=== Wallet Balance ===".cyan().bold());
    kv(
        "Confirmed",
        &sats_to_vtr(data["confirmed"].as_u64().unwrap_or(0)),
    );
    kv(
        "Unconfirmed",
        &sats_to_vtr(data["unconfirmed"].as_u64().unwrap_or(0)),
    );
    kv(
        "Staking",
        &sats_to_vtr(data["staking"].as_u64().unwrap_or(0)),
    );
    println!();
}

/// Print wallet addresses.
pub fn print_addresses(data: &Value) {
    println!("\n{}", "=== Wallet Addresses ===".cyan().bold());
    if let Some(addrs) = data.as_array() {
        for addr in addrs {
            let address = addr["address"].as_str().unwrap_or("unknown");
            let balance = addr["balance"].as_u64().unwrap_or(0);
            let label = addr["label"].as_str().unwrap_or("");
            println!(
                "  {} {} {} {}",
                "•".dimmed(),
                address.white().bold(),
                sats_to_vtr(balance).cyan(),
                if label.is_empty() {
                    String::new()
                } else {
                    format!("({})", label).dimmed().to_string()
                }
            );
        }
    }
    println!();
}

/// Print staking status.
pub fn print_staking_status(data: &Value) {
    println!("\n{}", "=== Staking Status ===".cyan().bold());
    let enabled = data["enabled"].as_bool().unwrap_or(false);
    kv("Status", if enabled { "Active" } else { "Inactive" });
    kv(
        "Address",
        data["staking_address"].as_str().unwrap_or("none"),
    );
    kv(
        "Eligible UTXOs",
        &data["eligible_utxos"].as_u64().unwrap_or(0).to_string(),
    );
    kv(
        "Staked",
        &sats_to_vtr(data["total_staking_satoshis"].as_u64().unwrap_or(0)),
    );
    kv(
        "Blocks staked",
        &data["blocks_staked"].as_u64().unwrap_or(0).to_string(),
    );
    kv(
        "Expected/day",
        &sats_to_vtr(data["expected_reward_per_day"].as_f64().unwrap_or(0.0) as u64),
    );
    println!();
}

/// Print torrent sessions.
pub fn print_torrent_sessions(data: &Value) {
    println!("\n{}", "=== Torrent Sessions ===".cyan().bold());
    if let Some(sessions) = data.as_array() {
        if sessions.is_empty() {
            println!("  {}", "No active sessions.".dimmed());
        } else {
            for session in sessions {
                let id = session["id"].as_str().unwrap_or("unknown");
                let name = session["name"].as_str().unwrap_or("unknown");
                let progress = session["progress"].as_f64().unwrap_or(0.0);
                let state = session["state"].as_str().unwrap_or("unknown");
                let short_id = &id[..id.len().min(8)];
                println!(
                    "  {} [{}] {} — {:.1}% ({})",
                    "•".dimmed(),
                    short_id.dimmed(),
                    name.white().bold(),
                    progress * 100.0,
                    state.cyan()
                );
            }
        }
    }
    println!();
}

/// Print DEX orders.
pub fn print_dex_orders(data: &Value) {
    println!("\n{}", "=== DEX Order Book ===".cyan().bold());
    if let Some(orders) = data.as_array() {
        if orders.is_empty() {
            println!("  {}", "No open orders.".dimmed());
        } else {
            println!(
                "  {:<12} {:<8} {:<12} {:<12} {:<20}",
                "ID".dimmed(),
                "Side".dimmed(),
                "Pair".dimmed(),
                "Amount".dimmed(),
                "Price".dimmed()
            );
            println!("  {}", "-".repeat(64).dimmed());
            for order in orders {
                let id = order["id"].as_str().unwrap_or("?");
                let side = order["side"].as_str().unwrap_or("?");
                let pair = order["pair"].as_str().unwrap_or("?");
                let amount = order["amount"].as_f64().unwrap_or(0.0);
                let price = order["price"].as_f64().unwrap_or(0.0);
                let side_colored = if side == "buy" {
                    side.green().to_string()
                } else {
                    side.red().to_string()
                };
                println!(
                    "  {:<12} {:<8} {:<12} {:<12.4} {:<20.8}",
                    id[..8.min(id.len())].dimmed(),
                    side_colored,
                    pair,
                    amount,
                    price
                );
            }
        }
    }
    println!();
}

/// Print claim check result.
pub fn print_claim_check(data: &Value) {
    println!("\n{}", "=== Legacy Claim Check ===".cyan().bold());
    let claimable = data["claimable"].as_bool().unwrap_or(false);
    kv("Claimable", if claimable { "Yes" } else { "No" });
    if claimable {
        kv(
            "Balance",
            &sats_to_vtr(data["balance_satoshis"].as_u64().unwrap_or(0)),
        );
        kv("Address", data["address"].as_str().unwrap_or("unknown"));
        println!("\n  {}", "To claim, run:".dimmed());
        println!(
            "  {}",
            "vtorrent-cli claim submit <address> <signature> <destination>".white()
        );
    }
    println!();
}

/// Print Prometheus metrics in a human-readable summary.
pub fn print_metrics(text: &str) {
    println!("\n{}", "=== Node Metrics ===".cyan().bold());
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() == 2 {
            let name = parts[0].replace("vtorrent_", "");
            let value = parts[1];
            kv(&name, value);
        }
    }
    println!();
}

/// Format seconds as human-readable uptime string.
fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

/// Print connected peers in a human-readable table.
pub fn print_peers(data: &Value) {
    let count = data["count"].as_u64().unwrap_or(0);
    let peers = data["peers"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    println!("{}", "Connected Peers".cyan().bold());
    println!("{}", "─".repeat(72).dimmed());

    if peers.is_empty() {
        println!("{}", "  No peers connected.".dimmed());
    } else {
        println!(
            "{:<36} {:<20} {:>8}  {}",
            "Address".bold(),
            "User Agent".bold(),
            "Height".bold(),
            "Services".bold()
        );
        println!("{}", "─".repeat(72).dimmed());
        for peer in peers {
            let addr = peer["addr"].as_str().unwrap_or("unknown");
            let ua = peer["user_agent"].as_str().unwrap_or("unknown");
            let height = peer["best_height"].as_u64().unwrap_or(0);
            let services = peer["services"].as_u64().unwrap_or(0);
            println!("{:<36} {:<20} {:>8}  {:#018x}", addr, ua, height, services);
        }
    }

    println!("{}", "─".repeat(72).dimmed());
    println!(
        "{} {}",
        "Total:".cyan().bold(),
        count.to_string().white().bold()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sats_to_vtr() {
        assert_eq!(sats_to_vtr(100_000_000), "1.00000000 VTR");
        assert_eq!(sats_to_vtr(50_000_000), "0.50000000 VTR");
        assert_eq!(sats_to_vtr(0), "0.00000000 VTR");
        assert_eq!(sats_to_vtr(1), "0.00000001 VTR");
    }

    #[test]
    fn test_format_uptime_seconds() {
        assert_eq!(format_uptime(45), "45s");
    }

    #[test]
    fn test_format_uptime_minutes() {
        assert_eq!(format_uptime(125), "2m 5s");
    }

    #[test]
    fn test_format_uptime_hours() {
        assert_eq!(format_uptime(3661), "1h 1m");
    }

    #[test]
    fn test_format_uptime_days() {
        assert_eq!(format_uptime(90000), "1d 1h");
    }

    #[test]
    fn test_print_metrics_parses_prometheus() {
        let text = "# HELP vtorrent_block_height Current height\n# TYPE vtorrent_block_height gauge\nvtorrent_block_height 42\n\n";
        // Should not panic
        print_metrics(text);
    }

    #[test]
    fn test_print_node_info_no_panic() {
        let data = serde_json::json!({
            "network": "vtorrent-testnet",
            "version": "2.0.0",
            "block_height": 100,
            "best_hash": "abc123",
            "peer_count": 5,
            "syncing": false,
            "uptime_seconds": 3600
        });
        // Should not panic
        print_node_info(&data);
    }
}
