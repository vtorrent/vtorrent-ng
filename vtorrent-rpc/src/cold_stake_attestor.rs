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

#[cfg(all(test, unix))]
thread_local! {
    static FAIL_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(std::io::Error::other("injected directory sync failure"));
    }
    std::fs::File::open(path)?.sync_all()
}

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
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let key = parse_key(text.trim())?;
            #[cfg(unix)]
            sync_directory(parent)?;
            return Ok(key);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let key = generate_key();
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(hex::encode(key.secret_bytes()).as_bytes())?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.error),
    }
    #[cfg(unix)]
    sync_directory(parent)?;
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

    #[cfg(unix)]
    #[test]
    fn created_key_is_private_even_with_a_stale_temp_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cold-stake-attestor.key");
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, b"stale").unwrap();
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o644)).unwrap();

        load_or_create_attestor_key(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn separate_processes_agree_on_first_use() {
        const CHILD: &str = "VTR_ATTESTOR_RACE_CHILD";
        if let Some(dir) = std::env::var_os(CHILD) {
            let dir = std::path::PathBuf::from(dir);
            let key = load_or_create_attestor_key(&dir.join("attestor.key")).unwrap();
            std::fs::write(
                dir.join(format!("result-{}", std::process::id())),
                attestor_pubkey_hex(&key),
            )
            .unwrap();
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut children: Vec<_> = (0..16)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .arg("--exact")
                    .arg("cold_stake_attestor::tests::separate_processes_agree_on_first_use")
                    .env(CHILD, dir.path())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let expected = attestor_pubkey_hex(
            &load_or_create_attestor_key(&dir.path().join("attestor.key")).unwrap(),
        );
        let results: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("result-"))
            .collect();
        assert_eq!(results.len(), 16);
        for result in results {
            assert_eq!(std::fs::read_to_string(result.path()).unwrap(), expected);
        }
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

    #[cfg(unix)]
    #[test]
    fn existing_key_is_not_returned_if_directory_sync_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attestor.key");
        let original = load_or_create_attestor_key(&path).unwrap();
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(true));
        assert!(load_or_create_attestor_key(&path).is_err());
        assert_eq!(load_or_create_attestor_key(&path).unwrap(), original);
    }
}
