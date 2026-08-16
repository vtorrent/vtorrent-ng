/// vTorrent Snapshot Extractor CLI
///
/// Usage:
///   vtorrent-snapshot --chainstate <path> --output <path> [--height <n>] [--block-hash <hash>]
///
/// Example:
///   vtorrent-snapshot \
///     --chainstate ~/.vtorrent/chainstate \
///     --output ./snapshot \
///     --height 500000 \
///     --block-hash 000000abc123...
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use vtorrent_snapshot::{
    leveldb_reader::read_all_utxos,
    snapshot_reader::verify_integrity,
    snapshot_writer::{print_summary, write_binary, write_json},
    utxo_set::build_snapshot,
};

#[derive(Parser, Debug)]
#[command(
    name = "vtorrent-snapshot",
    about = "Extract the UTXO snapshot from the legacy vTorrent blockchain",
    long_about = "Reads the legacy vTorrent chainstate LevelDB database and produces a \
                  compact, cryptographically-signed UTXO snapshot for the new genesis block."
)]
struct Args {
    /// Path to the legacy vTorrent chainstate directory
    /// (usually ~/.vtorrent/chainstate or ~/.vtorrent/txleveldb)
    #[arg(short, long)]
    chainstate: PathBuf,

    /// Output directory for the snapshot files
    #[arg(short, long, default_value = "./snapshot")]
    output: PathBuf,

    /// Block height at which the snapshot was taken
    #[arg(long, default_value = "0")]
    height: u32,

    /// Best block hash at snapshot time (hex string)
    #[arg(long, default_value = "unknown")]
    block_hash: String,

    /// Skip integrity verification after writing
    #[arg(long, default_value = "false")]
    skip_verify: bool,
}

fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    println!("vTorrent Snapshot Extractor v2.0");
    println!("=================================");
    println!("Chainstate: {}", args.chainstate.display());
    println!("Output:     {}", args.output.display());
    println!();

    // Step 1: Read all UTXOs from the legacy LevelDB chainstate
    println!("Step 1/4: Reading chainstate database...");
    let raw_utxos = read_all_utxos(&args.chainstate)
        .map_err(|e| anyhow::anyhow!("Failed to read chainstate: {}", e))?;
    println!("  ✓ Read {} raw UTXO records", raw_utxos.len());

    // Step 2: Parse and aggregate UTXOs into the snapshot
    println!("Step 2/4: Parsing and aggregating UTXOs...");
    let snapshot = build_snapshot(raw_utxos, args.height, &args.block_hash)
        .map_err(|e| anyhow::anyhow!("Failed to build snapshot: {}", e))?;
    println!(
        "  ✓ Aggregated {} addresses ({:.2} VTR total supply)",
        snapshot.metadata.total_addresses,
        snapshot.metadata.total_supply as f64 / 1e8
    );

    // Step 3: Write snapshot files
    println!("Step 3/4: Writing snapshot files...");
    std::fs::create_dir_all(&args.output)?;

    let json_path = args.output.join("utxo_snapshot.json");
    let bin_path = args.output.join("utxo_snapshot.bin");

    write_json(&snapshot, &json_path)
        .map_err(|e| anyhow::anyhow!("Failed to write JSON: {}", e))?;
    println!("  ✓ JSON: {}", json_path.display());

    write_binary(&snapshot, &bin_path)
        .map_err(|e| anyhow::anyhow!("Failed to write binary: {}", e))?;
    println!("  ✓ Binary: {}", bin_path.display());

    // Step 4: Verify integrity
    if !args.skip_verify {
        println!("Step 4/4: Verifying snapshot integrity...");
        verify_integrity(&snapshot)
            .map_err(|e| anyhow::anyhow!("Integrity check failed: {}", e))?;
        println!(
            "  ✓ Integrity hash verified: {}",
            &snapshot.metadata.entries_hash[..16]
        );
    }

    println!();
    print_summary(&snapshot);

    println!();
    println!("✓ Snapshot complete!");
    println!();
    println!("Next steps:");
    println!("  1. Share utxo_snapshot.json publicly for community verification");
    println!("  2. Copy utxo_snapshot.bin to vtorrent-node/src/genesis/");
    println!("  3. Build the new chain with: cargo build -p vtorrent-node --release");

    Ok(())
}
