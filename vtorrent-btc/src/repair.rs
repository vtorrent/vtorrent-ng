//! Durable publication of both desktop BTC states after wallet-owner authorization.
use crate::utxo::{lock_state_path, UtxoSet, INITIALIZED};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const ROLES: [&str; 4] = [
    "mainnet-marker",
    "mainnet-state",
    "regtest-marker",
    "regtest-state",
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u8,
    wallet: [u8; 32],
    operation: [u8; 16],
    artifacts: [Artifact; 4],
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Artifact {
    role: String,
    identity: Identity,
    digest: [u8; 32],
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Identity {
    volume: u64,
    file: u128,
    created: std::time::SystemTime,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn require_absent(path: &Path) -> io::Result<()> {
    if present(path)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "BTC file {} already exists; preserve it for assisted recovery",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn create_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_dir(path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .write(true)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(path)?.sync_all()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // Both pointers remain NUL-terminated for the syscall. Neither operation permits replacement.
    #[cfg(target_os = "linux")]
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut chars: Vec<_> = path.as_os_str().encode_wide().collect();
        if chars.contains(&0) {
            return Err(invalid("NUL in repair path"));
        }
        chars.push(0);
        Ok(chars)
    }
    let (from, to) = (wide(from)?, wide(to)?);
    // No COPY_ALLOWED or REPLACE_EXISTING. Both UTF-16 buffers live through the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn no_replace(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

fn regular(path: &Path) -> io::Result<(File, Identity)> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid("repair artifact is not a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(invalid("repair artifact is not a regular file"));
    }
    #[cfg(unix)]
    let (volume, index, links) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), u128::from(meta.ino()), meta.nlink())
    };
    #[cfg(windows)]
    let (volume, index, links) = {
        use std::{
            mem::{size_of, MaybeUninit},
            os::windows::io::AsRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::*;
        let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        let mut id = MaybeUninit::<FILE_ID_INFO>::uninit();
        // Win32 initializes each output on success; both queries address the same open file.
        unsafe {
            if GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) == 0
                || GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileIdInfo,
                    id.as_mut_ptr().cast(),
                    size_of::<FILE_ID_INFO>() as u32,
                ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let (info, id) = (info.assume_init(), id.assume_init());
            if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(invalid("unexpected reparse point"));
            }
            (
                id.VolumeSerialNumber,
                u128::from_le_bytes(id.FileId.Identifier),
                info.nNumberOfLinks as u64,
            )
        }
    };
    if links != 1 || index == 0 {
        return Err(invalid("repair requires an identifiable single-link file"));
    }
    Ok((
        file,
        Identity {
            volume,
            file: index,
            created: meta.created()?,
        },
    ))
}

fn inspect(path: &Path, role: &str) -> io::Result<Artifact> {
    let (mut file, identity) = regular(path)?;
    let mut digest = Sha256::new();
    io::copy(&mut file, &mut digest)?;
    Ok(Artifact {
        role: role.to_owned(),
        identity,
        digest: digest.finalize().into(),
    })
}

pub struct RepairPaths {
    mainnet: PathBuf,
    regtest: PathBuf,
}

impl RepairPaths {
    pub fn new(mainnet: PathBuf, regtest: PathBuf) -> io::Result<Self> {
        let id = mainnet
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("btc_utxos-mainnet-"))
            .and_then(|s| s.strip_suffix(".json"))
            .filter(|s| {
                s.len() == 16
                    && s.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
            .ok_or_else(|| invalid("unexpected mainnet wallet path"))?;
        if regtest != mainnet.with_file_name(format!("btc_utxos-regtest-{id}.json")) {
            return Err(invalid("unexpected regtest wallet path"));
        }
        let dir = mainnet.parent().unwrap();
        let dir = fs::canonicalize(if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        })?;
        Ok(Self {
            mainnet: dir.join(mainnet.file_name().unwrap()),
            regtest: dir.join(regtest.file_name().unwrap()),
        })
    }

    fn dir(&self) -> &Path {
        self.mainnet.parent().unwrap()
    }
    fn targets(&self) -> [PathBuf; 4] {
        [
            self.mainnet.with_extension("initialized"),
            self.mainnet.clone(),
            self.regtest.with_extension("initialized"),
            self.regtest.clone(),
        ]
    }
    fn stage(&self, operation: &[u8; 16]) -> PathBuf {
        self.dir()
            .join(format!(".btc-repair-{}", hex::encode(operation)))
    }

    /// Check while holding the network writer lock before opening either state.
    pub fn ensure_idle(&self) -> io::Result<()> {
        if present(&self.intent())? {
            return Err(invalid(
                "BTC repair is pending; resume with wallet-owner authorization",
            ));
        }
        Ok(())
    }

    /// Initialize or resume after the caller authenticates this wallet and attests no prior history.
    pub fn run(&self, wallet_id: &[u8; 32]) -> io::Result<()> {
        if self.mainnet.file_name().unwrap()
            != format!("btc_utxos-mainnet-{}.json", hex::encode(&wallet_id[..8])).as_str()
        {
            return Err(invalid("BTC repair path does not match wallet"));
        }
        let _main_lock = lock_state_path(&self.mainnet)?;
        let _reg_lock = lock_state_path(&self.regtest)?;
        for name in [
            "btc_utxos.json",
            "btc_utxos-mainnet.json",
            "btc_utxos-regtest.json",
            "btc_utxos-testnet.json",
        ] {
            let path = self.dir().join(name);
            require_absent(&path)?;
            require_absent(&path.with_extension("initialized"))?;
        }
        if !present(&self.intent())? {
            for target in self.targets() {
                require_absent(&target)?;
            }
            self.prepare(wallet_id)?;
        }
        let (file, _) = regular(&self.intent())?;
        if file.metadata()?.len() > 8192 {
            return Err(invalid("oversized BTC repair intent"));
        }
        let intent: Intent = serde_json::from_reader(file.take(8193))?;
        if intent.version != 1 || intent.wallet != *wallet_id {
            return Err(invalid("BTC repair record version or wallet mismatch"));
        }
        let stage = self.stage(&intent.operation);
        if !fs::symlink_metadata(&stage)?.is_dir() {
            return Err(invalid("invalid repair staging directory"));
        }
        let targets = self.targets();
        let mut pending = Vec::new();
        for ((role, target), record) in ROLES.iter().zip(&targets).zip(&intent.artifacts) {
            let staged = stage.join(role);
            let current = match (present(&staged)?, present(target)?) {
                (true, false) => &staged,
                (false, true) => target,
                _ => return Err(invalid("missing or conflicting BTC repair artifact")),
            };
            if inspect(current, role)? != *record {
                return Err(invalid(
                    "BTC repair artifact changed; preserve it for assisted recovery",
                ));
            }
            if current == &staged {
                pending.push((staged, target));
            }
        }
        // A previous process may have exited immediately after publishing intent, before fsync.
        sync_dir(self.dir())?;
        for (from, to) in pending {
            no_replace(&from, to)?;
        }
        sync_dir(self.dir())?;
        sync_dir(&stage)?;
        fs::remove_file(self.intent())?;
        sync_dir(self.dir())?;
        let _ = fs::remove_dir(stage);
        Ok(())
    }
    fn intent(&self) -> PathBuf {
        self.mainnet.with_extension("repair-pending")
    }
    fn prepare(&self, wallet_id: &[u8; 32]) -> io::Result<PathBuf> {
        let operation = rand::random();
        let stage = self.stage(&operation);
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&stage)?;
        let state = UtxoSet::new().serialized()?;
        let mut artifacts = Vec::new();
        for (role, bytes) in ROLES.iter().zip([INITIALIZED, &state, INITIALIZED, &state]) {
            let path = stage.join(role);
            create_file(&path, bytes)?;
            artifacts.push(inspect(&path, role)?);
        }
        let intent = Intent {
            version: 1,
            wallet: *wallet_id,
            operation,
            artifacts: artifacts
                .try_into()
                .map_err(|_| invalid("expected four repair artifacts"))?,
        };
        let draft = stage.join("intent");
        create_file(&draft, &serde_json::to_vec(&intent)?)?;
        sync_dir(&stage)?;
        sync_dir(self.dir())?;
        no_replace(&draft, &self.intent())?;
        sync_dir(self.dir())?;
        Ok(stage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: [u8; 32] = [1; 32];

    fn fixture(case: &str) -> (PathBuf, RepairPaths) {
        let dir = std::env::temp_dir().join(format!(
            "repair-{case}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let paths = RepairPaths::new(
            dir.join("btc_utxos-mainnet-0101010101010101.json"),
            dir.join("btc_utxos-regtest-0101010101010101.json"),
        )
        .unwrap();
        (dir, paths)
    }

    #[test]
    fn preserves_unowned_files() {
        for case in [
            "legacy",
            "partial",
            "orphan",
            "malformed",
            "wrong-id",
            "changed",
            "replacement",
        ] {
            let (dir, paths) = fixture(case);
            let mut id = ID;
            let path = match case {
                "legacy" => dir.join("btc_utxos.json"),
                "partial" => paths.regtest.clone(),
                "orphan" => paths.regtest.with_extension("initialized"),
                "malformed" => paths.intent(),
                _ => {
                    let stage = paths.prepare(&ID).unwrap();
                    fs::rename(stage.join("regtest-state"), &paths.regtest).unwrap();
                    match case {
                        "wrong-id" => id[31] ^= 1,
                        "changed" => fs::write(&paths.regtest, b"changed").unwrap(),
                        "replacement" => {
                            let original = dir.join("saved");
                            fs::rename(&paths.regtest, &original).unwrap();
                            fs::copy(original, &paths.regtest).unwrap();
                        }
                        _ => unreachable!(),
                    }
                    paths.regtest.clone()
                }
            };
            if !path.exists() {
                fs::write(
                    &path,
                    if case == "orphan" {
                        INITIALIZED
                    } else {
                        b"original"
                    },
                )
                .unwrap();
            }
            let bytes = fs::read(&path).unwrap();
            let intent = fs::read(paths.intent()).ok();
            assert!(paths.run(&id).is_err(), "{case}");
            assert_eq!(fs::read(path).unwrap(), bytes);
            assert_eq!(fs::read(paths.intent()).ok(), intent);
            assert!(!paths.mainnet.exists());
            assert!(!paths.mainnet.with_extension("initialized").exists());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn resumes_without_rewriting_an_installed_file() {
        let (dir, paths) = fixture("resume");
        let stage = paths.prepare(&ID).unwrap();
        assert!(paths.ensure_idle().is_err());
        no_replace(&stage.join("mainnet-state"), &paths.mainnet).unwrap();
        let before = inspect(&paths.mainnet, "mainnet-state").unwrap();
        paths.run(&ID).unwrap();
        assert!(before == inspect(&paths.mainnet, "mainnet-state").unwrap());
        paths.ensure_idle().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exclusive_move_keeps_both_names_on_collision() {
        let (dir, _) = fixture("collision");
        let (from, to) = (dir.join("from"), dir.join("to"));
        fs::write(&from, b"from").unwrap();
        fs::write(&to, b"to").unwrap();
        assert!(no_replace(&from, &to).is_err());
        assert_eq!(fs::read(from).unwrap(), b"from");
        assert_eq!(fs::read(to).unwrap(), b"to");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn publishes_both_states_once() {
        let (dir, paths) = fixture("fresh");
        paths.run(&ID).unwrap();
        for path in [&paths.mainnet, &paths.regtest] {
            assert!(path.is_file());
            assert_eq!(
                fs::read(path.with_extension("initialized")).unwrap(),
                b"vtorrent-btc-initialized-v1\n"
            );
        }
        assert!(!paths.intent().exists());
        assert!(paths.run(&ID).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
