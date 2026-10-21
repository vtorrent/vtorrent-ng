//! Durable registry of cold-stake script hashes.
//!
//! Only the script hash is persisted, never the spending key. The registry
//! lets `fund_cold_stake` accept a script generated before a node restart
//! without keeping the cold key on the node.

use std::path::{Path, PathBuf};

/// Registry file next to the encrypted wallet.
pub fn registry_path(wallet_path: &Path) -> PathBuf {
    wallet_path.with_extension("cold_stake.json")
}

/// Load the registered script hashes.
pub fn load(wallet_path: &Path) -> Vec<String> {
    std::fs::read(registry_path(wallet_path))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Register a script hash (idempotent, atomic, 0600 on Unix).
pub fn add(wallet_path: &Path, script_hash: &str) -> std::io::Result<()> {
    let mut hashes = load(wallet_path);
    if hashes.iter().any(|hash| hash == script_hash) {
        return Ok(());
    }
    hashes.push(script_hash.to_string());
    let path = registry_path(wallet_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&hashes).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Whether a script hash is registered.
pub fn contains(wallet_path: &Path, script_hash: &str) -> bool {
    load(wallet_path).iter().any(|hash| hash == script_hash)
}

#[cfg(test)]
mod tests {
    #[test]
    fn registry_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("vtr-cold-stake-registry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wallet = dir.join("wallet.json");
        let hash = "ab".repeat(20);
        assert!(!super::contains(&wallet, &hash));
        super::add(&wallet, &hash).unwrap();
        assert!(super::contains(&wallet, &hash));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
