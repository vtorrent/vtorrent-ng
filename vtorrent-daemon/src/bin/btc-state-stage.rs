use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Stage or archive an isolated, independently verified legacy BTC wallet copy")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Copy exact legacy bytes to the verified seed-scoped path.
    Stage {
        /// The daemon data directory that contains both wallet paths.
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// 0600 file containing the 128-hex-character BTC seed.
        #[arg(long)]
        seed_file: PathBuf,
        /// Required when the source is the unlabelled btc_utxos.json.
        #[arg(long)]
        network: Option<String>,
        /// Assert all wallet writers are stopped and both directories are isolated.
        #[arg(long, required = true)]
        confirm_isolated: bool,
        /// Assert seed ownership, network, and outstanding swaps were verified independently.
        #[arg(long, required = true)]
        confirm_owner: bool,
    },
    /// Copy the legacy file to a durable archive, then retire its active filename.
    Archive {
        /// The daemon data directory that contains both wallet paths.
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        archive: PathBuf,
        /// The seed-scoped destination that must already hold identical bytes.
        #[arg(long)]
        output: PathBuf,
        /// 0600 file containing the 128-hex-character BTC seed.
        #[arg(long)]
        seed_file: PathBuf,
        /// Required when the source is the unlabelled btc_utxos.json.
        #[arg(long)]
        network: Option<String>,
        /// Assert all wallet writers are stopped and both directories are isolated.
        #[arg(long, required = true)]
        confirm_isolated: bool,
        /// Assert seed ownership, network, and outstanding swaps were verified independently.
        #[arg(long, required = true)]
        confirm_owner: bool,
    },
}

fn source_label(source: &Path, network: Option<&str>) -> anyhow::Result<&'static str> {
    let label = match source.file_name().and_then(|s| s.to_str()).unwrap_or("") {
        "btc_utxos-mainnet.json" => "mainnet",
        "btc_utxos-regtest.json" => "regtest",
        "btc_utxos.json" => match network {
            Some("mainnet") => "mainnet",
            Some("regtest") => "regtest",
            _ => anyhow::bail!(
                "btc_utxos.json is unlabelled; pass --network mainnet or --network regtest"
            ),
        },
        _ => anyhow::bail!("source must be a legacy BTC wallet filename"),
    };
    if let Some(requested) = network {
        if requested != label {
            anyhow::bail!(
                "--network {} does not match the source label {}",
                requested,
                label
            );
        }
    }
    Ok(label)
}

fn read_seed(path: &Path) -> anyhow::Result<[u8; 64]> {
    require_private_file(path)?;
    let raw = zeroize::Zeroizing::new(fs::read(path)?);
    let text = trim_ascii(&raw);
    if text.len() != 128 {
        anyhow::bail!("seed file must contain exactly 128 hex characters");
    }
    let mut seed = [0u8; 64];
    hex::decode_to_slice(text, &mut seed)
        .map_err(|_| anyhow::anyhow!("seed file must contain exactly 128 hex characters"))?;
    Ok(seed)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

fn require_private_file(path: &Path) -> anyhow::Result<()> {
    require_regular_file(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::symlink_metadata(path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "{} must not be accessible by group or others (expected 0600, found {:04o})",
                path.display(),
                mode
            );
        }
    }
    Ok(())
}

fn wallet_id(seed: &[u8; 64]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"vtorrent-btc-wallet-id-v1");
    hasher.update(seed);
    hex::encode(&hasher.finalize()[..8])
}

fn require_regular_file(path: &Path) -> anyhow::Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        anyhow::bail!("{} must be a regular file, not a symlink", path.display());
    }
    Ok(())
}

fn validate_in_data_dir(data_dir: &Path, path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if fs::canonicalize(parent)? != fs::canonicalize(data_dir)? {
        anyhow::bail!(
            "{} must be directly inside --data-dir {}",
            path.display(),
            data_dir.display()
        );
    }
    Ok(())
}

fn open_private_new(path: &std::path::Path) -> anyhow::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn fsync_parent(path: &std::path::Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn validate_scoped_output(
    path: &std::path::Path,
    label: &str,
    expected_id: &str,
) -> anyhow::Result<()> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let prefix = format!("btc_utxos-{label}-");
    let id = name
        .strip_prefix(&prefix)
        .and_then(|s| s.strip_suffix(".json"));
    if id != Some(expected_id) {
        anyhow::bail!(
            "output must be btc_utxos-{}-{}.json for this seed",
            label,
            expected_id
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn stage(
    data_dir: PathBuf,
    source: PathBuf,
    output: PathBuf,
    seed_file: PathBuf,
    network: Option<String>,
    _isolated: bool,
    _owner: bool,
) -> anyhow::Result<()> {
    let seed = zeroize::Zeroizing::new(read_seed(&seed_file)?);
    let expected_id = wallet_id(&seed);
    let label = source_label(&source, network.as_deref())?;
    validate_in_data_dir(&data_dir, &source)?;
    validate_in_data_dir(&data_dir, &output)?;
    validate_scoped_output(&output, label, &expected_id)?;
    require_regular_file(&source)?;
    let original = fs::read(&source)?;
    let _: vtorrent_btc::utxo::UtxoSet = serde_json::from_slice(&original)?;
    let stage_path = output.with_extension("json.recovery-stage");
    let mut file = open_private_new(&stage_path)?;
    file.write_all(&original)?;
    file.sync_all()?;
    if fs::read(&stage_path)? != original || fs::read(&source)? != original {
        anyhow::bail!("source or staged bytes changed; inspect the retained stage file");
    }
    fs::hard_link(&stage_path, &output)?;
    fsync_parent(&output)?;
    if fs::read(&source)? != original {
        anyhow::bail!("source changed during publication; do not use the staged output");
    }
    fs::remove_file(&stage_path)?;
    fsync_parent(&output)?;
    eprintln!("Staged bytes only. The original remains in place; the operator must verify ownership, isolation, backups, and recovery before use.");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn archive_legacy(
    data_dir: PathBuf,
    source: PathBuf,
    archive: PathBuf,
    output: PathBuf,
    seed_file: PathBuf,
    network: Option<String>,
    _isolated: bool,
    _owner: bool,
) -> anyhow::Result<()> {
    let seed = zeroize::Zeroizing::new(read_seed(&seed_file)?);
    let expected_id = wallet_id(&seed);
    let label = source_label(&source, network.as_deref())?;
    validate_in_data_dir(&data_dir, &source)?;
    validate_in_data_dir(&data_dir, &output)?;
    require_regular_file(&source)?;
    let original = fs::read(&source)?;
    let _: vtorrent_btc::utxo::UtxoSet = serde_json::from_slice(&original)?;
    validate_scoped_output(&output, label, &expected_id)?;
    require_regular_file(&output)?;
    if fs::read(&output)? != original {
        anyhow::bail!("staged output does not match the legacy bytes; re-stage before archiving");
    }
    let stage_path = archive.with_extension("json.archive-stage");
    let mut file = open_private_new(&stage_path)?;
    file.write_all(&original)?;
    file.sync_all()?;
    if fs::read(&stage_path)? != original {
        anyhow::bail!("archived copy changed before publication; inspect the stage file");
    }
    fs::hard_link(&stage_path, &archive)?;
    fsync_parent(&archive)?;
    if fs::read(&archive)? != original || fs::read(&output)? != original {
        anyhow::bail!(
            "verification failed after archive publication; the legacy filename was not removed"
        );
    }
    if fs::read(&source)? != original {
        anyhow::bail!(
            "source changed after archive publication; the legacy filename was not removed"
        );
    }
    fs::remove_file(&source)?;
    fsync_parent(&source)?;
    fs::remove_file(&stage_path)?;
    fsync_parent(&archive)?;
    eprintln!("Archived exact legacy bytes. The active legacy filename is gone; verify the archive before starting the wallet.");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Stage {
            data_dir,
            source,
            output,
            seed_file,
            network,
            confirm_isolated,
            confirm_owner,
        } => stage(
            data_dir,
            source,
            output,
            seed_file,
            network,
            confirm_isolated,
            confirm_owner,
        ),
        Command::Archive {
            data_dir,
            source,
            archive,
            output,
            seed_file,
            network,
            confirm_isolated,
            confirm_owner,
        } => archive_legacy(
            data_dir,
            source,
            archive,
            output,
            seed_file,
            network,
            confirm_isolated,
            confirm_owner,
        ),
    }
}
