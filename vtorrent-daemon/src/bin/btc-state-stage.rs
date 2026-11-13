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
        /// Assert all wallet writers are stopped and both directories are isolated.
        #[arg(long, required = true)]
        confirm_isolated: bool,
    },
}

fn source_label(source: &std::path::Path) -> anyhow::Result<&'static str> {
    match source.file_name().and_then(|s| s.to_str()).unwrap_or("") {
        "btc_utxos-mainnet.json" => Ok("mainnet"),
        "btc_utxos-regtest.json" => Ok("regtest"),
        _ => anyhow::bail!("source must be a network-only BTC wallet filename"),
    }
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

fn validate_scoped_output(path: &std::path::Path, label: &str) -> anyhow::Result<()> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let prefix = format!("btc_utxos-{label}-");
    let id = name
        .strip_prefix(&prefix)
        .and_then(|s| s.strip_suffix(".json"));
    if !id.is_some_and(|id| {
        id.len() == 16
            && id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }) {
        anyhow::bail!("output must be the verified seed-scoped filename for the same network");
    }
    Ok(())
}

fn stage(
    data_dir: PathBuf,
    source: PathBuf,
    output: PathBuf,
    _isolated: bool,
    _owner: bool,
) -> anyhow::Result<()> {
    let label = source_label(&source)?;
    validate_in_data_dir(&data_dir, &source)?;
    validate_in_data_dir(&data_dir, &output)?;
    validate_scoped_output(&output, label)?;
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

fn archive_legacy(
    data_dir: PathBuf,
    source: PathBuf,
    archive: PathBuf,
    output: PathBuf,
    _isolated: bool,
) -> anyhow::Result<()> {
    let label = source_label(&source)?;
    validate_in_data_dir(&data_dir, &source)?;
    validate_in_data_dir(&data_dir, &output)?;
    require_regular_file(&source)?;
    let original = fs::read(&source)?;
    let _: vtorrent_btc::utxo::UtxoSet = serde_json::from_slice(&original)?;
    validate_scoped_output(&output, label)?;
    require_regular_file(&output)?;
    if fs::read(&output)? != original {
        anyhow::bail!("staged output does not match the legacy bytes; re-stage before archiving");
    }
    fs::hard_link(&source, &archive)?;
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
    eprintln!("Archived exact legacy bytes. The active legacy filename is gone; verify the archive before starting the wallet.");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Stage {
            data_dir,
            source,
            output,
            confirm_isolated,
            confirm_owner,
        } => stage(data_dir, source, output, confirm_isolated, confirm_owner),
        Command::Archive {
            data_dir,
            source,
            archive,
            output,
            confirm_isolated,
        } => archive_legacy(data_dir, source, archive, output, confirm_isolated),
    }
}
