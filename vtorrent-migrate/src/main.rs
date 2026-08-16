use std::fs;
use std::path::PathBuf;
use vtorrent_migrate::extractor::extract_wallet;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    println!("╔══════════════════════════════════════════════════════╗");
    println!("║   vTorrent Legacy Wallet Migration Tool v0.1.0       ║");
    println!("║   Extracts keys from legacy wallet.dat files         ║");
    println!("╚══════════════════════════════════════════════════════╝");
    println!();

    if args.len() < 2 {
        eprintln!("Usage: vtorrent-migrate <wallet.dat> [--passphrase <passphrase>]");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --passphrase <pass>   Passphrase for encrypted wallets");
        eprintln!("  --json                Output results as JSON");
        std::process::exit(1);
    }

    let wallet_path = PathBuf::from(&args[1]);
    if !wallet_path.exists() {
        eprintln!("Error: File not found: {}", wallet_path.display());
        std::process::exit(1);
    }

    let mut passphrase: Option<String> = None;
    let mut json_output = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--passphrase" => {
                if i + 1 < args.len() {
                    passphrase = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --passphrase requires a value");
                    std::process::exit(1);
                }
            }
            "--json" => {
                json_output = true;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    println!("Reading wallet file: {}", wallet_path.display());
    let wallet_data = match fs::read(&wallet_path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Error reading wallet file: {}", e);
            std::process::exit(1);
        }
    };
    println!("File size: {} bytes", wallet_data.len());
    println!();

    println!("Parsing BerkeleyDB structure...");
    match extract_wallet(&wallet_data, passphrase.as_deref()) {
        Ok(extraction) => {
            if json_output {
                match serde_json::to_string_pretty(&extraction) {
                    Ok(json) => println!("{}", json),
                    Err(e) => eprintln!("JSON serialization error: {}", e),
                }
            } else {
                println!("╔══════════════════════════════════════════════════════╗");
                println!("║                  Extraction Results                  ║");
                println!("╚══════════════════════════════════════════════════════╝");
                println!();
                println!("  Wallet version:  {:?}", extraction.wallet_version);
                println!("  Was encrypted:   {}", extraction.was_encrypted);
                println!("  Had 2FA (OTP):   {}", extraction.had_2fa);
                println!("  Keys found:      {}", extraction.keys.len());
                println!("  Labels found:    {}", extraction.labels.len());
                println!();

                for (idx, key) in extraction.keys.iter().enumerate() {
                    println!("  Key #{}", idx + 1);
                    println!("    Legacy Address: {}", key.legacy_address);
                    println!("    Compressed:     {}", key.compressed);
                    println!("    Source:         {:?}", key.source);
                    if std::env::var("VTORRENT_SHOW_WIF").is_ok() {
                        println!("    WIF:            {}", key.wif);
                    } else {
                        println!(
                            "    WIF:            [hidden - set VTORRENT_SHOW_WIF=1 to reveal]"
                        );
                    }
                    println!();
                }

                if !extraction.labels.is_empty() {
                    println!("  Address Labels:");
                    for (addr, label) in &extraction.labels {
                        println!("    {} -> {}", addr, label);
                    }
                    println!();
                }

                println!("Migration data ready. Use the vTorrent 2.0 client to claim your VTR.");
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
