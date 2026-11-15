//! Attestor key for cold-stake funding attestations.

use secp256k1::SecretKey;
use std::io::Write;
use std::path::Path;

/// Serializes first-use key creation inside one process.
///
/// Two concurrent requests that both see a missing file would otherwise
/// generate different keys and race on the same temporary file, leaving a
/// response signed by a key that is not on disk.
static ATTESTOR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The public key, compressed hex, that operators pin on the offline machine.
pub fn attestor_pubkey_hex(key: &SecretKey) -> String {
    let secp = secp256k1::Secp256k1::new();
    hex::encode(secp256k1::PublicKey::from_secret_key(&secp, key).serialize())
}

/// Load the attestor key, creating it on first use.
///
/// The key is written atomically with mode 0600 and the parent directory is
/// synced, so a crash cannot leave a truncated key that later fails to parse.
pub fn load_or_create_attestor_key(path: &Path) -> std::io::Result<SecretKey> {
    let _guard = ATTESTOR_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match std::fs::read_to_string(path) {
        Ok(text) => return parse_key(text.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let key = generate_key();
    let temporary = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(hex::encode(key.secret_bytes()).as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    // Another process may have won the create. Return the key that is actually
    // on disk so every caller signs with the key the operator can pin.
    parse_key(std::fs::read_to_string(path)?.trim())
}

fn parse_key(text: &str) -> std::io::Result<SecretKey> {
    let bytes = hex::decode(text).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("attestor key is not hexadecimal: {error}"),
        )
    })?;
    SecretKey::from_slice(&bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("attestor key is not a secp256k1 scalar: {error}"),
        )
    })
}

fn generate_key() -> SecretKey {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    loop {
        rand::thread_rng().fill_bytes(&mut bytes);
        if let Ok(key) = SecretKey::from_slice(&bytes) {
            return key;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attestor_key_is_created_private_and_reused() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-cold-stake-attestor-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cold-stake-attestor.key");
        let first = load_or_create_attestor_key(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let second = load_or_create_attestor_key(&path).unwrap();
        assert_eq!(attestor_pubkey_hex(&first), attestor_pubkey_hex(&second));
        assert_eq!(attestor_pubkey_hex(&first).len(), 66);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_first_use_converges_on_one_key() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-cold-stake-attestor-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cold-stake-attestor.key");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    attestor_pubkey_hex(&load_or_create_attestor_key(&path).unwrap())
                })
            })
            .collect();
        let keys: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_corrupt_key_file_is_reported_not_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-cold-stake-attestor-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cold-stake-attestor.key");
        std::fs::write(&path, b"not-a-key").unwrap();
        let error = load_or_create_attestor_key(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), b"not-a-key");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
