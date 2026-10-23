//! Durable registry of cold-stake script hashes.
//!
//! Only the script hash is persisted, never the spending key. The registry
//! lets `fund_cold_stake` accept a script generated before a node restart
//! without keeping the cold key on the node.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static REGISTRY_LOCK: Mutex<()> = Mutex::new(());
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Registry file next to the encrypted wallet.
pub fn registry_path(wallet_path: &Path) -> PathBuf {
    wallet_path.with_extension("cold_stake.json")
}

fn load_strict(wallet_path: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::read(registry_path(wallet_path)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Load the registered script hashes.
///
/// A malformed registry is treated as empty for read-only checks; `add`
/// refuses to overwrite it so a corrupt file cannot silently discard hashes.
pub fn load(wallet_path: &Path) -> Vec<String> {
    load_strict(wallet_path).unwrap_or_default()
}

/// Register a script hash (idempotent, atomic, 0600 on Unix).
pub fn add(wallet_path: &Path, script_hash: &str) -> std::io::Result<()> {
    let _guard = REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut hashes = load_strict(wallet_path)?;
    if hashes.iter().any(|hash| hash == script_hash) {
        return Ok(());
    }
    hashes.push(script_hash.to_string());
    let path = registry_path(wallet_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&hashes).map_err(std::io::Error::other)?;
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Whether a script hash is registered.
pub fn contains(wallet_path: &Path, script_hash: &str) -> bool {
    load(wallet_path).iter().any(|hash| hash == script_hash)
}

#[cfg(test)]
mod tests {
    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vtr-cold-stake-registry-{}-{}",
            label,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn registry_round_trips() {
        let dir = temp_dir("round-trip");
        let wallet = dir.join("wallet.json");
        let hash = "ab".repeat(20);
        assert!(!super::contains(&wallet, &hash));
        super::add(&wallet, &hash).unwrap();
        assert!(super::contains(&wallet, &hash));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_refuses_to_overwrite_a_corrupt_file() {
        let dir = temp_dir("corrupt");
        let wallet = dir.join("wallet.json");
        let registry = super::registry_path(&wallet);
        std::fs::write(&registry, b"not json").unwrap();
        let error = super::add(&wallet, &"ab".repeat(20)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&registry).unwrap(), b"not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_serializes_concurrent_adds() {
        let dir = temp_dir("concurrent");
        let wallet = dir.join("wallet.json");
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let wallet = wallet.clone();
                std::thread::spawn(move || {
                    super::add(&wallet, &format!("{index:040x}")).unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let hashes = super::load(&wallet);
        assert_eq!(hashes.len(), 8);
        for index in 0..8 {
            assert!(hashes.contains(&format!("{index:040x}")));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
