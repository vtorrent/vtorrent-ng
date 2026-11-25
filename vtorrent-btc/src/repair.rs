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

/// Test-only seams. None of this module, its call sites, or its environment
/// variable exists in a production build because it is compiled out with
/// `cfg(test)`.
#[cfg(test)]
mod test_hooks {
    use std::{cell::Cell, sync::OnceLock};

    /// Exit status used by the interruption child so the parent can prove the
    /// requested persistent step was reached instead of a clean completion.
    pub(super) const EXIT_CODE: i32 = 86;
    pub(super) const INTERRUPT_ENV: &str = "VTR_REPAIR_INTERRUPT";

    pub(super) const STAGE_CREATED: &str = "stage-created";
    pub(super) const STAGED_MAINNET_MARKER: &str = "staged-mainnet-marker";
    pub(super) const STAGED_MAINNET_STATE: &str = "staged-mainnet-state";
    pub(super) const STAGED_REGTEST_MARKER: &str = "staged-regtest-marker";
    pub(super) const STAGED_REGTEST_STATE: &str = "staged-regtest-state";
    pub(super) const DRAFT_INTENT: &str = "draft-intent";
    pub(super) const STAGE_SYNCED: &str = "stage-synced";
    pub(super) const WALLET_DIR_SYNCED: &str = "wallet-dir-synced";
    pub(super) const INTENT_PUBLISHED: &str = "intent-published";
    pub(super) const PUBLISH_SYNCED: &str = "publish-synced";
    pub(super) const RESUME_SYNCED: &str = "resume-synced";
    pub(super) const MAINNET_MARKER_PUBLISHED: &str = "mainnet-marker-published";
    pub(super) const MAINNET_STATE_PUBLISHED: &str = "mainnet-state-published";
    pub(super) const REGTEST_MARKER_PUBLISHED: &str = "regtest-marker-published";
    pub(super) const REGTEST_STATE_PUBLISHED: &str = "regtest-state-published";
    pub(super) const MOVES_SYNCED: &str = "moves-synced";
    pub(super) const STAGE_MOVES_SYNCED: &str = "stage-moves-synced";
    pub(super) const INTENT_RETIRED: &str = "intent-retired";
    pub(super) const RETIREMENT_SYNCED: &str = "retirement-synced";
    pub(super) const STAGE_REMOVED: &str = "stage-removed";

    pub(super) const ALL_POINTS: [&str; 20] = [
        STAGE_CREATED,
        STAGED_MAINNET_MARKER,
        STAGED_MAINNET_STATE,
        STAGED_REGTEST_MARKER,
        STAGED_REGTEST_STATE,
        DRAFT_INTENT,
        STAGE_SYNCED,
        WALLET_DIR_SYNCED,
        INTENT_PUBLISHED,
        PUBLISH_SYNCED,
        RESUME_SYNCED,
        MAINNET_MARKER_PUBLISHED,
        MAINNET_STATE_PUBLISHED,
        REGTEST_MARKER_PUBLISHED,
        REGTEST_STATE_PUBLISHED,
        MOVES_SYNCED,
        STAGE_MOVES_SYNCED,
        INTENT_RETIRED,
        RETIREMENT_SYNCED,
        STAGE_REMOVED,
    ];

    fn interrupt_request() -> Option<&'static str> {
        static REQUEST: OnceLock<Option<String>> = OnceLock::new();
        REQUEST
            .get_or_init(|| std::env::var(INTERRUPT_ENV).ok())
            .as_deref()
    }

    pub(super) fn after(point: &str) {
        if interrupt_request() == Some(point) {
            std::process::exit(EXIT_CODE);
        }
    }

    thread_local! {
        static UNSUPPORTED_NO_REPLACE: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn unsupported_no_replace() -> bool {
        UNSUPPORTED_NO_REPLACE.with(|armed| armed.get())
    }

    /// Arms the unsupported-platform outcome on this thread only. Other test
    /// threads keep using the real platform move.
    pub(super) struct UnsupportedNoReplace;

    impl UnsupportedNoReplace {
        pub(super) fn arm() -> Self {
            UNSUPPORTED_NO_REPLACE.with(|armed| armed.set(true));
            Self
        }
    }

    impl Drop for UnsupportedNoReplace {
        fn drop(&mut self) {
            UNSUPPORTED_NO_REPLACE.with(|armed| armed.set(false));
        }
    }
}

#[cfg(test)]
fn staging_point(role: &str) -> &'static str {
    match role {
        "mainnet-marker" => test_hooks::STAGED_MAINNET_MARKER,
        "mainnet-state" => test_hooks::STAGED_MAINNET_STATE,
        "regtest-marker" => test_hooks::STAGED_REGTEST_MARKER,
        "regtest-state" => test_hooks::STAGED_REGTEST_STATE,
        _ => unreachable!("unknown repair role"),
    }
}

#[cfg(test)]
fn publication_point(role: &str) -> &'static str {
    match role {
        "mainnet-marker" => test_hooks::MAINNET_MARKER_PUBLISHED,
        "mainnet-state" => test_hooks::MAINNET_STATE_PUBLISHED,
        "regtest-marker" => test_hooks::REGTEST_MARKER_PUBLISHED,
        "regtest-state" => test_hooks::REGTEST_STATE_PUBLISHED,
        _ => unreachable!("unknown repair role"),
    }
}

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

#[derive(Debug, thiserror::Error)]
#[error("{context}: {source}; preserve files; assisted recovery required")]
struct AssistedRecovery {
    context: String,
    #[source]
    source: io::Error,
}

#[derive(Debug, thiserror::Error)]
#[error("BTC repair is pending; resume with wallet-owner authorization")]
struct PendingRepair;

/// True when a writer lock is contended.
///
/// Classification is based on the typed marker [`crate::utxo::lock_state_path`]
/// attaches, so raw OS errno values (such as Unix `EDOM`, also 33) cannot be
/// mistaken for lock contention.
pub fn is_writer_contention(error: &io::Error) -> bool {
    crate::utxo::is_writer_contention(error)
}

/// True for the pending-record refusal returned by [`RepairPaths::ensure_idle`].
pub fn is_repair_pending(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<PendingRepair>())
}

/// True for an ownership conflict or invalid journal that another authenticated retry cannot fix.
pub fn requires_assisted_recovery(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<AssistedRecovery>())
}

fn assisted(context: &str, source: io::Error) -> io::Error {
    io::Error::new(
        source.kind(),
        AssistedRecovery {
            context: context.into(),
            source,
        },
    )
}

fn validation(context: &str, cause: io::Error) -> io::Error {
    if !requires_assisted_recovery(&cause)
        && matches!(
            cause.kind(),
            io::ErrorKind::NotFound
                | io::ErrorKind::InvalidData
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::AlreadyExists
        )
    {
        assisted(context, cause)
    } else {
        cause
    }
}

fn role_tag(operation: &[u8; 16], role: &str) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"vtorrent-btc-repair-tag-v2")
        .chain_update(operation)
        .chain_update(role)
        .finalize()
        .into()
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
        return Err(assisted(
            "BTC repair preflight",
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("BTC file {} already exists", path.display()),
            ),
        ));
    }
    Ok(())
}

fn create_file(path: &Path, bytes: &[u8], tag: Option<&[u8; 32]>) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    if let Some(tag) = tag {
        provenance::write(&file, path, tag)?;
    }
    file.sync_all()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod provenance {
    use super::*;
    use std::os::fd::AsRawFd;

    const NAME: &std::ffi::CStr = c"user.vtorrent.btc-repair";

    pub(super) fn write(file: &File, _: &Path, tag: &[u8; 32]) -> io::Result<()> {
        let (fd, name, value, len) = (
            file.as_raw_fd(),
            NAME.as_ptr(),
            tag.as_ptr().cast(),
            tag.len(),
        );
        // Each call borrows the open fd and tag buffer only for the duration of the syscall.
        #[cfg(target_os = "linux")]
        let rc = unsafe { libc::fsetxattr(fd, name, value, len, libc::XATTR_CREATE) };
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::fsetxattr(fd, name, value, len, 0, libc::XATTR_CREATE) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn read(file: &File, _: &Path) -> io::Result<[u8; 32]> {
        let mut tag = [0u8; 32];
        let (fd, name, value, len) = (
            file.as_raw_fd(),
            NAME.as_ptr(),
            tag.as_mut_ptr().cast(),
            tag.len(),
        );
        #[cfg(target_os = "linux")]
        let rc = unsafe { libc::fgetxattr(fd, name, value, len) };
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::fgetxattr(fd, name, value, len, 0, 0) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            #[cfg(target_os = "linux")]
            const MISSING: i32 = libc::ENODATA;
            #[cfg(target_os = "macos")]
            const MISSING: i32 = libc::ENOATTR;
            if matches!(err.raw_os_error(), Some(MISSING | libc::ERANGE)) {
                return Err(assisted("missing or invalid repair ownership tag", err));
            }
            return Err(err);
        }
        if rc as usize != tag.len() {
            return Err(invalid("invalid repair ownership tag length"));
        }
        Ok(tag)
    }
}

#[cfg(windows)]
mod provenance {
    use super::*;
    fn stream(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(":vtorrent.btc-repair");
        name.into()
    }
    pub(super) fn write(_: &File, path: &Path, tag: &[u8; 32]) -> io::Result<()> {
        let mut stream = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(stream(path))?;
        stream.write_all(tag)?;
        stream.sync_all()
    }
    pub(super) fn read(_: &File, path: &Path) -> io::Result<[u8; 32]> {
        let mut stream = File::open(stream(path))?;
        let mut tag = [0; 32];
        if stream.metadata()?.len() != tag.len() as u64 {
            return Err(invalid("invalid repair ownership stream length"));
        }
        stream.read_exact(&mut tag)?;
        Ok(tag)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod provenance {
    use super::*;
    pub(super) fn write(_: &File, _: &Path, _: &[u8; 32]) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn read(_: &File, _: &Path) -> io::Result<[u8; 32]> {
        Err(io::ErrorKind::Unsupported.into())
    }
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
    #[cfg(test)]
    if test_hooks::unsupported_no_replace() {
        return Err(io::ErrorKind::Unsupported.into());
    }
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
    #[cfg(test)]
    if test_hooks::unsupported_no_replace() {
        return Err(io::ErrorKind::Unsupported.into());
    }
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

fn describe(mut file: File, identity: Identity, role: &str) -> io::Result<Artifact> {
    let mut digest = Sha256::new();
    io::copy(&mut file, &mut digest)?;
    Ok(Artifact {
        role: role.to_owned(),
        identity,
        digest: digest.finalize().into(),
    })
}

fn inspect_tagged(path: &Path, role: &str, operation: &[u8; 16]) -> io::Result<Artifact> {
    let (file, identity) = regular(path)?;
    if provenance::read(&file, path)? != role_tag(operation, role) {
        return Err(invalid("repair ownership tag mismatch"));
    }
    describe(file, identity, role)
}

fn verify_artifact(
    path: &Path,
    role: &str,
    operation: &[u8; 16],
    record: &Artifact,
    journal_volume: u64,
) -> io::Result<()> {
    let actual = inspect_tagged(path, role, operation)?;
    if actual.identity.volume != journal_volume {
        return Err(invalid(
            "repair artifact and intent are on different volumes",
        ));
    }
    // Unix st_dev is meaningful within this mount, not across a reboot.
    if actual.role != record.role
        || actual.digest != record.digest
        || actual.identity.file != record.identity.file
        || actual.identity.created != record.identity.created
        || (cfg!(windows) && actual.identity.volume != record.identity.volume)
    {
        return Err(invalid("BTC repair artifact identity or digest changed"));
    }
    Ok(())
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
            return Err(io::Error::new(io::ErrorKind::InvalidData, PendingRepair));
        }
        Ok(())
    }

    /// Initialize or resume after the caller authenticates this wallet and attests no prior history.
    pub fn run(&self, wallet_id: &[u8; 32]) -> io::Result<()> {
        if self.mainnet.file_name().unwrap()
            != format!("btc_utxos-mainnet-{}.json", hex::encode(&wallet_id[..8])).as_str()
        {
            return Err(assisted(
                "BTC repair authorization",
                invalid("BTC repair path does not match wallet"),
            ));
        }
        let lock = |path: &Path| {
            lock_state_path(path).map_err(|e| {
                if e.kind() == io::ErrorKind::InvalidInput {
                    assisted("invalid BTC writer path", e)
                } else {
                    e
                }
            })
        };
        let _main_lock = lock(&self.mainnet)?;
        let _reg_lock = lock(&self.regtest)?;
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
        self.resume(wallet_id)
            .map_err(|e| validation("BTC repair journal", e))
    }

    fn resume(&self, wallet_id: &[u8; 32]) -> io::Result<()> {
        let (file, journal_identity) = regular(&self.intent())?;
        if file.metadata()?.len() > 8192 {
            return Err(invalid("oversized BTC repair intent"));
        }
        let mut bytes = Vec::new();
        file.take(8193).read_to_end(&mut bytes)?;
        let intent: Intent = serde_json::from_slice(&bytes).map_err(|e| {
            assisted(
                "malformed BTC repair intent",
                io::Error::new(io::ErrorKind::InvalidData, e),
            )
        })?;
        if intent.version != 2 || intent.wallet != *wallet_id {
            return Err(invalid("BTC repair record version or wallet mismatch"));
        }
        let stage = self.stage(&intent.operation);
        if !fs::symlink_metadata(&stage)
            .map_err(|e| validation("BTC repair staging directory", e))?
            .is_dir()
        {
            return Err(invalid("invalid repair staging directory"));
        }
        let targets = self.targets();
        let mut pending = Vec::new();
        for ((role, target), record) in ROLES.iter().zip(&targets).zip(&intent.artifacts) {
            let staged = stage.join(role);
            let current = match (present(&staged)?, present(target)?) {
                (true, false) => &staged,
                (false, _) => target,
                (true, true) => {
                    return Err(invalid("conflicting staged and final BTC repair artifacts"))
                }
            };
            verify_artifact(
                current,
                role,
                &intent.operation,
                record,
                journal_identity.volume,
            )
            .map_err(|e| validation(role, e))?;
            if current == &staged {
                pending.push((staged, target));
            }
        }
        // A previous process may have exited immediately after publishing intent, before fsync.
        sync_dir(self.dir())?;
        #[cfg(test)]
        test_hooks::after(test_hooks::RESUME_SYNCED);
        for (from, to) in pending {
            no_replace(&from, to)?;
            #[cfg(test)]
            test_hooks::after(publication_point(
                ROLES[targets
                    .iter()
                    .position(|target| target == to)
                    .expect("pending repair target has a role")],
            ));
        }
        sync_dir(self.dir())?;
        #[cfg(test)]
        test_hooks::after(test_hooks::MOVES_SYNCED);
        sync_dir(&stage)?;
        #[cfg(test)]
        test_hooks::after(test_hooks::STAGE_MOVES_SYNCED);
        fs::remove_file(self.intent())?;
        #[cfg(test)]
        test_hooks::after(test_hooks::INTENT_RETIRED);
        sync_dir(self.dir())?;
        #[cfg(test)]
        test_hooks::after(test_hooks::RETIREMENT_SYNCED);
        let _ = fs::remove_dir(stage);
        #[cfg(test)]
        test_hooks::after(test_hooks::STAGE_REMOVED);
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
        #[cfg(test)]
        test_hooks::after(test_hooks::STAGE_CREATED);
        let state = UtxoSet::new().serialized()?;
        let mut artifacts = Vec::new();
        for (role, bytes) in ROLES.iter().zip([INITIALIZED, &state, INITIALIZED, &state]) {
            let path = stage.join(role);
            create_file(&path, bytes, Some(&role_tag(&operation, role)))?;
            #[cfg(test)]
            test_hooks::after(staging_point(role));
            artifacts.push(inspect_tagged(&path, role, &operation)?);
        }
        let intent = Intent {
            version: 2,
            wallet: *wallet_id,
            operation,
            artifacts: artifacts
                .try_into()
                .map_err(|_| invalid("expected four repair artifacts"))?,
        };
        let draft = stage.join("intent");
        create_file(&draft, &serde_json::to_vec(&intent)?, None)?;
        #[cfg(test)]
        test_hooks::after(test_hooks::DRAFT_INTENT);
        sync_dir(&stage)?;
        #[cfg(test)]
        test_hooks::after(test_hooks::STAGE_SYNCED);
        sync_dir(self.dir())?;
        #[cfg(test)]
        test_hooks::after(test_hooks::WALLET_DIR_SYNCED);
        no_replace(&draft, &self.intent())?;
        #[cfg(test)]
        test_hooks::after(test_hooks::INTENT_PUBLISHED);
        sync_dir(self.dir())?;
        #[cfg(test)]
        test_hooks::after(test_hooks::PUBLISH_SYNCED);
        Ok(stage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    const ID: [u8; 32] = [1; 32];

    fn inspect(path: &Path, role: &str) -> io::Result<Artifact> {
        let (file, identity) = regular(path)?;
        describe(file, identity, role)
    }

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

    const SEED: [u8; 64] = [7; 64];
    const CHILD_ENV: &str = "VTR_REPAIR_CHILD";
    const CHILD_DIR_ENV: &str = "VTR_REPAIR_CHILD_DIR";
    const RESUME_REFUSAL_ENV: &str = "VTR_REPAIR_ALLOW_REFUSAL";

    fn paths_for(dir: &Path) -> RepairPaths {
        RepairPaths::new(
            dir.join("btc_utxos-mainnet-0101010101010101.json"),
            dir.join("btc_utxos-regtest-0101010101010101.json"),
        )
        .unwrap()
    }

    fn child_paths() -> RepairPaths {
        let dir = std::env::var_os(CHILD_DIR_ENV).expect("child repair directory");
        paths_for(&PathBuf::from(dir))
    }

    fn run_child_mode(mode: &str) {
        match mode {
            "interrupt" => {
                let paths = child_paths();
                paths.run(&ID).unwrap();
                panic!("repair completed without reaching the requested interruption point");
            }
            "resume" => {
                let allow_refusal = std::env::var_os(RESUME_REFUSAL_ENV).is_some();
                match child_paths().run(&ID) {
                    Ok(()) => println!("REPAIR_RESUME=completed"),
                    Err(error) if allow_refusal && requires_assisted_recovery(&error) => {
                        println!("REPAIR_RESUME=refused");
                    }
                    Err(error) => panic!("resume failed: {error}"),
                }
            }
            "hold-locks" => {
                let paths = child_paths();
                let _mainnet = lock_state_path(&paths.mainnet).unwrap();
                let _regtest = lock_state_path(&paths.regtest).unwrap();
                println!("REPAIR_LOCKS=ready");
                std::io::stdout().flush().unwrap();
                let mut release = [0u8; 1];
                let _ = std::io::stdin().read_exact(&mut release);
            }
            other => panic!("unknown child mode {other}"),
        }
    }

    fn spawn_repair_child(test: &str, mode: &str, dir: &Path) -> std::process::Child {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_ENV, mode)
            .env(CHILD_DIR_ENV, dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        crate::utxo::fork_gate::GATE.spawn(|| command.spawn().unwrap())
    }

    fn run_repair_child(
        test: &str,
        mode: &str,
        dir: &Path,
        interrupt: Option<&str>,
        allow_refusal: bool,
    ) -> std::process::Output {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_ENV, mode)
            .env(CHILD_DIR_ENV, dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        match interrupt {
            Some(point) => command.env(test_hooks::INTERRUPT_ENV, point),
            None => command.env_remove(test_hooks::INTERRUPT_ENV),
        };
        if allow_refusal {
            command.env(RESUME_REFUSAL_ENV, "1");
        } else {
            command.env_remove(RESUME_REFUSAL_ENV);
        }
        let child = crate::utxo::fork_gate::GATE.spawn(|| command.spawn().unwrap());
        child.wait_with_output().unwrap()
    }

    fn published_files(paths: &RepairPaths) -> Vec<(PathBuf, Vec<u8>)> {
        paths
            .targets()
            .into_iter()
            .filter_map(|path| fs::read(&path).ok().map(|bytes| (path, bytes)))
            .collect()
    }

    fn tree_snapshot(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    out.insert(path.clone(), Vec::new());
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(dir, &mut out);
        out
    }

    fn staging_present(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".btc-repair-")
        })
    }

    fn open_both_after_repair(paths: &RepairPaths) {
        for (network, path) in [
            (bitcoin::Network::Bitcoin, &paths.mainnet),
            (bitcoin::Network::Regtest, &paths.regtest),
        ] {
            drop(
                crate::wallet::BtcWallet::with_persistence_checked(
                    SEED,
                    network,
                    path.clone(),
                    || paths.ensure_idle(),
                )
                .unwrap(),
            );
            drop(crate::wallet::BtcWallet::with_persistence(SEED, network, path.clone()).unwrap());
        }
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
    fn matching_synthetic_identity_and_bytes_require_a_role_tag() {
        let (dir, _) = fixture("untagged");
        let path = dir.join("fixture");
        fs::write(&path, INITIALIZED).unwrap();
        let record = inspect(&path, "mainnet-marker").unwrap();
        assert!(record == inspect(&path, "mainnet-marker").unwrap());
        assert!(verify_artifact(
            &path,
            "mainnet-marker",
            &[7; 16],
            &record,
            record.identity.volume
        )
        .is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn remounted_volume_accepts_only_current_same_volume_and_role_tag() {
        let (dir, paths) = fixture("remount");
        let stage = paths.prepare(&ID).unwrap();
        let mut intent: Intent =
            serde_json::from_slice(&fs::read(paths.intent()).unwrap()).unwrap();
        let artifact = &mut intent.artifacts[0];
        let current_volume = artifact.identity.volume;
        artifact.identity.volume ^= 1;
        let path = stage.join(ROLES[0]);
        verify_artifact(&path, ROLES[0], &intent.operation, artifact, current_volume).unwrap();
        assert!(verify_artifact(
            &path,
            ROLES[0],
            &intent.operation,
            artifact,
            current_volume ^ 1
        )
        .is_err());
        let mut other_operation = intent.operation;
        other_operation[0] ^= 1;
        assert!(
            verify_artifact(&path, ROLES[0], &other_operation, artifact, current_volume).is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_and_contention_errors_are_typed() {
        let (dir, paths) = fixture("classes");
        fs::write(paths.intent(), b"pending").unwrap();
        let pending = paths.ensure_idle().unwrap_err();
        assert!(is_repair_pending(&pending), "{pending}");
        assert!(!requires_assisted_recovery(&pending), "{pending}");
        assert!(!is_writer_contention(&pending), "{pending}");

        let state = dir.join("state.json");
        let held = crate::utxo::lock_state_path(&state).unwrap();
        let contended = crate::utxo::lock_state_path(&state).unwrap_err();
        assert!(is_writer_contention(&contended), "{contended}");
        assert!(!is_repair_pending(&contended), "{contended}");
        drop(held);

        // A bare kind or raw errno is not a lock-layer marker. On Unix raw 33
        // is EDOM, so kind/errno-only classification would be wrong.
        let plain_would_block = io::Error::from(io::ErrorKind::WouldBlock);
        assert!(!is_writer_contention(&plain_would_block));
        let unix_edom = io::Error::from_raw_os_error(33);
        assert!(!is_writer_contention(&unix_edom));

        let other = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(!is_writer_contention(&other));
        assert!(!is_repair_pending(&other));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn assisted_recovery_is_distinct_from_retryable_errors() {
        for case in ["malformed", "old-version", "stage", "artifact", "locked"] {
            let (dir, paths) = fixture(case);
            let stage = paths.prepare(&ID).unwrap();
            let mut lock = None;
            match case {
                "malformed" => fs::write(paths.intent(), b"{").unwrap(),
                "old-version" => {
                    let mut intent: Intent =
                        serde_json::from_slice(&fs::read(paths.intent()).unwrap()).unwrap();
                    intent.version = 1;
                    fs::write(paths.intent(), serde_json::to_vec(&intent).unwrap()).unwrap();
                }
                "stage" => fs::remove_dir_all(stage).unwrap(),
                "artifact" => fs::remove_file(stage.join("regtest-state")).unwrap(),
                "locked" => lock = Some(lock_state_path(&paths.mainnet).unwrap()),
                _ => unreachable!(),
            }
            let err = paths.run(&ID).unwrap_err();
            assert_eq!(requires_assisted_recovery(&err), case != "locked", "{err}");
            if case != "locked" {
                assert!(
                    err.to_string()
                        .contains("preserve files; assisted recovery required"),
                    "{err}"
                );
                let details = err
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<AssistedRecovery>()
                    .unwrap();
                match case {
                    "stage" | "artifact" => {
                        assert_eq!(details.source.kind(), io::ErrorKind::NotFound);
                        assert!(details.source.raw_os_error().is_some());
                    }
                    "malformed" => {
                        assert!(details.source.get_ref().unwrap().is::<serde_json::Error>())
                    }
                    _ => (),
                }
            }
            assert!(paths.targets().iter().all(|p| !p.exists()));
            drop(lock);
            fs::remove_dir_all(dir).unwrap();
        }
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
        for (network, path) in [
            (bitcoin::Network::Bitcoin, &paths.mainnet),
            (bitcoin::Network::Regtest, &paths.regtest),
        ] {
            crate::wallet::BtcWallet::with_persistence([0; 64], network, path.clone()).unwrap();
        }
        assert!(paths.run(&ID).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resumes_after_process_exit() {
        const TEST: &str = "repair::tests::resumes_after_process_exit";
        if let Some(mode) = std::env::var_os(CHILD_ENV) {
            run_child_mode(mode.to_str().unwrap());
            return;
        }

        for point in test_hooks::ALL_POINTS {
            let (dir, paths) = fixture(&format!("exit-{point}"));

            let output = run_repair_child(TEST, "interrupt", &dir, Some(point), false);
            assert_eq!(
                output.status.code(),
                Some(test_hooks::EXIT_CODE),
                "{point}: interruption was not reached; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );

            let published = published_files(&paths);
            if paths.intent().exists() {
                let error = paths.ensure_idle().unwrap_err();
                assert!(is_repair_pending(&error), "{point}: {error}");
            } else {
                paths.ensure_idle().unwrap();
            }
            let before = tree_snapshot(&dir);

            let retired = matches!(
                point,
                test_hooks::INTENT_RETIRED
                    | test_hooks::RETIREMENT_SYNCED
                    | test_hooks::STAGE_REMOVED
            );
            let output = run_repair_child(TEST, "resume", &dir, None, retired);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "{point}: resume child failed; stdout: {stdout}; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if retired {
                assert!(
                    stdout.contains("REPAIR_RESUME=refused"),
                    "{point}: a completed repair must refuse a repeat, but resumed: {stdout}"
                );
                assert_eq!(
                    tree_snapshot(&dir),
                    before,
                    "{point}: a refused repeat changed wallet files"
                );
            } else {
                assert!(
                    stdout.contains("REPAIR_RESUME=completed"),
                    "{point}: resume did not complete: {stdout}"
                );
                for (path, bytes) in &published {
                    assert_eq!(
                        &fs::read(path).unwrap(),
                        bytes,
                        "{point}: {} was rewritten after publication",
                        path.display()
                    );
                }
            }

            for target in paths.targets() {
                assert!(target.is_file(), "{point}: {}", target.display());
            }
            paths.ensure_idle().unwrap();
            open_both_after_repair(&paths);

            let before_repeat = tree_snapshot(&dir);
            let error = paths.run(&ID).unwrap_err();
            assert!(requires_assisted_recovery(&error), "{point}: {error}");
            assert_eq!(
                tree_snapshot(&dir),
                before_repeat,
                "{point}: a refused repeat request changed wallet files"
            );

            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn os_releases_repair_locks_when_the_holder_dies() {
        const TEST: &str = "repair::tests::os_releases_repair_locks_when_the_holder_dies";
        if let Some(mode) = std::env::var_os(CHILD_ENV) {
            run_child_mode(mode.to_str().unwrap());
            return;
        }

        let (dir, paths) = fixture("writer-death");
        let mut child = spawn_repair_child(TEST, "hold-locks", &dir);
        let stdout = child.stdout.take().unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(stdout);
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                if line.trim() == "REPAIR_LOCKS=ready" {
                    break;
                }
            }
            let _ = ready_tx.send(line.trim().to_owned());
        });
        let line = ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the lock holder must report readiness");
        assert_eq!(line, "REPAIR_LOCKS=ready");

        for path in [&paths.mainnet, &paths.regtest] {
            let error = lock_state_path(path).unwrap_err();
            assert!(is_writer_contention(&error), "{error}");
        }
        let error = paths.run(&ID).unwrap_err();
        assert!(is_writer_contention(&error), "{error}");
        assert!(!paths.intent().exists());
        assert!(!staging_present(&dir));

        let _ = child.kill();
        assert!(!child.wait().unwrap().success());
        reader.join().unwrap();
        drop(child.stdin.take());

        for path in [&paths.mainnet, &paths.regtest] {
            drop(lock_state_path(path).unwrap());
        }
        paths.run(&ID).unwrap();
        open_both_after_repair(&paths);
        assert!(requires_assisted_recovery(&paths.run(&ID).unwrap_err()));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_no_replace_preserves_existing_files() {
        let (dir, paths) = fixture("unsupported-partial");
        let stage = paths.prepare(&ID).unwrap();
        let targets = paths.targets();
        no_replace(&stage.join(ROLES[0]), &targets[0]).unwrap();
        no_replace(&stage.join(ROLES[1]), &targets[1]).unwrap();
        let published = [
            fs::read(&targets[0]).unwrap(),
            fs::read(&targets[1]).unwrap(),
        ];
        let staged = [ROLES[2], ROLES[3]].map(|role| fs::read(stage.join(role)).unwrap());
        let intent = fs::read(paths.intent()).unwrap();

        {
            let _armed = test_hooks::UnsupportedNoReplace::arm();
            let error = paths.run(&ID).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{error}");
        }

        for (path, bytes) in targets[..2].iter().zip(&published) {
            assert_eq!(
                &fs::read(path).unwrap(),
                bytes,
                "{} changed",
                path.display()
            );
        }
        for (role, bytes) in [ROLES[2], ROLES[3]].iter().zip(&staged) {
            assert_eq!(
                &fs::read(stage.join(role)).unwrap(),
                bytes,
                "{role} changed"
            );
        }
        assert_eq!(fs::read(paths.intent()).unwrap(), intent);
        assert!(!targets[2].exists());
        assert!(!targets[3].exists());

        paths.run(&ID).unwrap();
        open_both_after_repair(&paths);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_no_replace_creates_no_final_state() {
        let (dir, paths) = fixture("unsupported-fresh");
        {
            let _armed = test_hooks::UnsupportedNoReplace::arm();
            let error = paths.run(&ID).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{error}");
        }
        assert!(paths.targets().iter().all(|path| !path.exists()));
        assert!(!paths.intent().exists());

        paths.run(&ID).unwrap();
        open_both_after_repair(&paths);
        fs::remove_dir_all(dir).unwrap();
    }
}
