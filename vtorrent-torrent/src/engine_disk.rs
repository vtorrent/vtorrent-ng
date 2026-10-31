//! Disk I/O for the torrent engine: piece persistence, resume checkpoints,
//! and on-disk verification.

use crate::engine::FileLayout;
use crate::metainfo::Metainfo;
use tokio::io::AsyncWriteExt;

pub fn resume_file_path(
    download_dir: &std::path::Path,
    info_hash: &[u8; 20],
) -> std::path::PathBuf {
    download_dir.join(format!("{}.vtorrent", hex::encode(info_hash)))
}

/// Verify a piece's bytes on disk hash to the expected SHA1. Used to validate
/// resume bitfields: stale bits must not mark missing/corrupt data as had.
pub async fn piece_on_disk_matches(
    metainfo: &Metainfo,
    download_dir: &std::path::Path,
    piece_index: u32,
) -> bool {
    let Some(data) = read_piece_from_disk(metainfo, download_dir, piece_index).await else {
        return false;
    };
    if data.len() != piece_length(metainfo, piece_index) as usize {
        return false;
    }
    match metainfo.pieces.get(piece_index as usize) {
        Some(expected) => {
            use sha1::{Digest as _, Sha1};
            let computed: [u8; 20] = Sha1::digest(&data).into();
            &computed == expected
        }
        None => false,
    }
}

/// The length of a piece (the last piece may be shorter).
///
/// Returns 0 for an out-of-range index or when the offset would overflow, so
/// a peer-supplied `Piece` index can never panic the task under
/// `overflow-checks`.
pub fn piece_length(metainfo: &Metainfo, index: u32) -> u64 {
    let Some(start) = (index as u64).checked_mul(metainfo.piece_length) else {
        return 0;
    };
    if start >= metainfo.total_size {
        return 0;
    }
    let remaining = metainfo.total_size - start;
    remaining.min(metainfo.piece_length)
}

/// Write a verified piece's data to the correct file(s) on disk.
///
/// Returns `false` if any segment could not be written (open, seek, or write
/// failure — disk full, permissions, IO error). The caller must NOT mark the
/// piece as had when this fails, otherwise the download is permanently
/// corrupted: the hole is never re-requested and gets served to peers.
pub async fn write_piece_to_disk(
    metainfo: &Metainfo,
    download_dir: &std::path::Path,
    piece_index: u32,
    piece_data: &[u8],
) -> bool {
    let layout = FileLayout::new(&metainfo.files, metainfo.piece_length);
    // The torrent name is also untrusted and must not escape the download dir.
    let Some(base) = safe_directory(download_dir, std::slice::from_ref(&metainfo.name), true).await
    else {
        return false;
    };
    if !ensure_torrent_marker(&base, &metainfo.info_hash).await {
        return false;
    }
    let mut all_ok = true;
    for (file_index, file_offset, bytes) in layout.piece_segments(piece_index, piece_data) {
        let file = &metainfo.files[file_index];
        // Resolve and create the parent with no symlink components; the final
        // open also uses O_NOFOLLOW on Unix.
        let Some(path) = safe_file_path(&base, &file.path, true).await else {
            all_ok = false;
            continue;
        };
        use tokio::io::AsyncSeekExt;
        let write_result: std::result::Result<(), std::io::Error> = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.create(true).truncate(false).write(true);
            #[cfg(unix)]
            options.custom_flags(libc::O_NOFOLLOW);
            let mut f = options.open(&path).await?;
            f.seek(std::io::SeekFrom::Start(file_offset)).await?;
            f.write_all(&bytes).await?;
            f.flush().await?;
            Ok(())
        }
        .await;
        if let Err(e) = write_result {
            tracing::error!(
                "Piece {}: writing {:?} failed: {} — piece will be re-requested",
                piece_index,
                path,
                e
            );
            all_ok = false;
        }
    }
    all_ok
}

/// Result of copying verified pieces into a fresh managed download directory.
pub struct PartialMigration {
    pub copied_pieces: u32,
    pub missing_pieces: u32,
}

/// Copy only verified legacy pieces into a new directory without modifying the source.
#[cfg(target_os = "linux")]
pub async fn migrate_partial_torrent(
    metainfo: &Metainfo,
    source: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<PartialMigration> {
    use sha1::{Digest, Sha1};
    use std::os::unix::ffi::OsStrExt;
    let invalid =
        || std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid torrent metainfo");
    if metainfo.piece_length == 0
        || metainfo.piece_length > 16 * 1024 * 1024
        || metainfo.total_size == 0
        || metainfo.piece_count == 0
        || metainfo.piece_count > 1_000_000
        || metainfo.pieces.len() != metainfo.piece_count as usize
        || metainfo
            .total_size
            .checked_add(metainfo.piece_length - 1)
            .map(|n| n / metainfo.piece_length)
            != Some(u64::from(metainfo.piece_count))
        || metainfo
            .files
            .iter()
            .try_fold(0u64, |total, file| total.checked_add(file.length))
            != Some(metainfo.total_size)
    {
        return Err(invalid());
    }
    if std::fs::symlink_metadata(source)?.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "source directory must not be a symlink",
        ));
    }
    let source = std::fs::canonicalize(source)?;
    let parent = std::fs::canonicalize(target.parent().ok_or_else(invalid)?)?;
    let name = target.file_name().ok_or_else(invalid)?;
    let target = parent.join(name);
    if target.starts_with(&source) || source.starts_with(&target) {
        return Err(std::io::Error::other(
            "source and destination must be separate directories",
        ));
    }
    let stage = parent.join(format!(".vtorrent-migration-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&stage)?;
    let result = async {
        let base = safe_directory(&stage, std::slice::from_ref(&metainfo.name), true)
            .await
            .ok_or_else(invalid)?;
        if !ensure_torrent_marker(&base, &metainfo.info_hash).await {
            return Err(invalid());
        }
        let mut tracker = crate::scheduler::PieceTracker::new(metainfo.piece_count);
        let mut copied = 0u32;
        for index in 0..metainfo.piece_count {
            if let Some(data) = read_piece_from_disk(metainfo, &source, index).await {
                if data.len() == piece_length(metainfo, index) as usize
                    && <[u8; 20]>::from(Sha1::digest(&data)) == metainfo.pieces[index as usize]
                {
                    if !write_piece_to_disk(metainfo, &stage, index, &data).await {
                        return Err(std::io::Error::other("failed to write verified piece"));
                    }
                    tracker.mark_have(index);
                    copied += 1;
                }
            }
        }
        let resume = resume_file_path(&stage, &metainfo.info_hash);
        let mut file = tokio::fs::File::create(&resume).await?;
        file.write_all(&tracker.serialize_have_bitfield()).await?;
        file.sync_all().await?;
        for index in 0..metainfo.piece_count {
            if tracker.has_piece(index) && !piece_on_disk_matches(metainfo, &stage, index).await {
                return Err(std::io::Error::other("copied piece failed verification"));
            }
        }
        let source_name =
            std::ffi::CString::new(stage.as_os_str().as_bytes()).map_err(|_| invalid())?;
        let target_name =
            std::ffi::CString::new(target.as_os_str().as_bytes()).map_err(|_| invalid())?;
        // RENAME_NOREPLACE prevents an existing destination from being replaced even under a race.
        let status = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source_name.as_ptr(),
                libc::AT_FDCWD,
                target_name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(PartialMigration {
            copied_pieces: copied,
            missing_pieces: metainfo.piece_count - copied,
        })
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    result
}

/// Partial migration is unavailable without an atomic no-replace directory rename.
#[cfg(not(target_os = "linux"))]
pub async fn migrate_partial_torrent(
    _: &Metainfo,
    _: &std::path::Path,
    _: &std::path::Path,
) -> std::io::Result<PartialMigration> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "partial migration requires Linux renameat2",
    ))
}

/// Read a complete piece back from disk so it can be served to peers.
///
/// This is the inverse of `write_piece_to_disk`: it opens each file touched by
/// the piece and reads the bytes back, reassembling the full piece.
pub async fn read_piece_from_disk(
    metainfo: &Metainfo,
    download_dir: &std::path::Path,
    piece_index: u32,
) -> Option<Vec<u8>> {
    let layout = FileLayout::new(&metainfo.files, metainfo.piece_length);
    let base = safe_directory(download_dir, std::slice::from_ref(&metainfo.name), false).await?;
    let piece_len = piece_length(metainfo, piece_index);
    let mut out = Vec::with_capacity(piece_len as usize);
    for (file_index, file_offset, byte_count) in layout.piece_segment_ranges(piece_index, piece_len)
    {
        let file = &metainfo.files[file_index];
        let path = safe_file_path(&base, &file.path, false).await?;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut options = tokio::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut f = options.open(&path).await.ok()?;
        let _ = f.seek(std::io::SeekFrom::Start(file_offset)).await;
        let mut buf = vec![0u8; byte_count as usize];
        f.read_exact(&mut buf).await.ok()?;
        out.extend_from_slice(&buf);
    }
    Some(out)
}

async fn safe_directory(
    base: &std::path::Path,
    components: &[String],
    create: bool,
) -> Option<std::path::PathBuf> {
    let base = tokio::fs::canonicalize(base).await.ok()?;
    let mut current = base.clone();
    for component in components {
        if !valid_component(component) {
            return None;
        }
        current.push(component);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => return None,
            Ok(_) => {}
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&current).await.ok()?;
            }
            Err(_) => return None,
        }
    }
    let current = tokio::fs::canonicalize(&current).await.ok()?;
    current.starts_with(&base).then_some(current)
}

async fn safe_file_path(
    base: &std::path::Path,
    components: &[String],
    create: bool,
) -> Option<std::path::PathBuf> {
    let (name, parents) = components.split_last()?;
    if !valid_component(name) || name == ".vtorrent-torrent" {
        return None;
    }
    let parent = safe_directory(base, parents, create).await?;
    let path = parent.join(name);
    if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return None;
        }
    }
    Some(path)
}

fn marker_value(info_hash: &[u8; 20]) -> String {
    format!("v1:{}\n", hex::encode(info_hash))
}

async fn read_marker(path: &std::path::Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path).await?;
    if file.metadata().await?.len() > 128 {
        return Err(std::io::Error::other(
            "torrent ownership marker is too large",
        ));
    }
    let mut value = String::new();
    file.read_to_string(&mut value).await?;
    Ok(value)
}

async fn ensure_torrent_marker(base: &std::path::Path, info_hash: &[u8; 20]) -> bool {
    let path = base.join(".vtorrent-torrent");
    let expected = marker_value(info_hash);
    match read_marker(&path).await {
        Ok(value) => return value == expected,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    if base
        .read_dir()
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(true)
    {
        tracing::warn!(
            "Refusing unowned torrent directory {}; explicit verified adoption is required",
            base.display()
        );
        return false;
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    match options.open(&path).await {
        Ok(mut file) => {
            file.write_all(expected.as_bytes()).await.is_ok() && file.sync_all().await.is_ok()
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_marker(&path)
            .await
            .is_ok_and(|value| value == expected),
        Err(_) => false,
    }
}

/// Adopt a complete legacy download only after verifying every torrent piece.
pub async fn adopt_existing_torrent(
    metainfo: &Metainfo,
    download_dir: &std::path::Path,
) -> std::io::Result<()> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "existing torrent data is incomplete, mismatched, or unsafe",
        )
    };
    if metainfo.piece_length == 0
        || metainfo.piece_length > 16 * 1024 * 1024
        || metainfo.total_size == 0
        || metainfo.piece_count == 0
        || metainfo.piece_count > 1_000_000
        || metainfo.pieces.len() != metainfo.piece_count as usize
        || metainfo
            .total_size
            .checked_add(metainfo.piece_length - 1)
            .map(|n| n / metainfo.piece_length)
            != Some(u64::from(metainfo.piece_count))
        || metainfo
            .files
            .iter()
            .try_fold(0u64, |total, file| total.checked_add(file.length))
            != Some(metainfo.total_size)
    {
        return Err(invalid());
    }
    let base = safe_directory(download_dir, std::slice::from_ref(&metainfo.name), false)
        .await
        .ok_or_else(invalid)?;
    for piece in 0..metainfo.piece_count {
        if !piece_on_disk_matches(metainfo, download_dir, piece).await {
            return Err(invalid());
        }
    }
    let path = base.join(".vtorrent-torrent");
    match read_marker(&path).await {
        Ok(value) if value == marker_value(&metainfo.info_hash) => return Ok(()),
        Ok(value) if value == "vtorrent\n" => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err(invalid()),
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(&path).await?;
    file.write_all(marker_value(&metainfo.info_hash).as_bytes())
        .await?;
    file.sync_all().await
}

fn valid_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.contains('/')
        && !component.contains('\\')
        && !component.contains(':')
        && !std::path::Path::new(component).is_absolute()
}

pub fn sanitize_path(base: &std::path::Path, components: &[String]) -> Option<std::path::PathBuf> {
    let mut path = base.to_path_buf();
    for comp in components {
        if !valid_component(comp) {
            return None;
        }
        path.push(comp);
    }
    Some(path)
}

#[cfg(test)]
mod adoption_tests {
    use super::*;
    use crate::metainfo::TorrentFile;
    use sha1::{Digest, Sha1};

    fn fixture(hash: u8) -> Metainfo {
        Metainfo {
            info_hash: [hash; 20],
            name: "payload".into(),
            total_size: 4,
            piece_length: 4,
            piece_count: 1,
            pieces: vec![Sha1::digest(b"good").into()],
            announce: None,
            announce_list: vec![],
            files: vec![TorrentFile {
                path: vec!["data".into()],
                length: 4,
                md5sum: None,
            }],
            created_at: None,
            comment: None,
            is_private: false,
        }
    }

    fn root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "vtr-adoption-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("payload")).unwrap();
        root
    }

    #[tokio::test]
    async fn verified_existing_torrent_can_be_adopted_but_other_hash_cannot_write() {
        let root = root();
        std::fs::write(root.join("payload/data"), b"good").unwrap();
        let torrent = fixture(1);
        adopt_existing_torrent(&torrent, &root).await.unwrap();
        assert!(write_piece_to_disk(&torrent, &root, 0, b"good").await);
        assert!(!write_piece_to_disk(&fixture(2), &root, 0, b"evil").await);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn generic_legacy_marker_requires_full_verification_before_upgrade() {
        let root = root();
        std::fs::write(root.join("payload/data"), b"good").unwrap();
        std::fs::write(root.join("payload/.vtorrent-torrent"), b"vtorrent\n").unwrap();
        assert!(!write_piece_to_disk(&fixture(1), &root, 0, b"good").await);
        adopt_existing_torrent(&fixture(1), &root).await.unwrap();
        assert!(write_piece_to_disk(&fixture(1), &root, 0, b"good").await);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn mismatched_or_incomplete_data_is_not_adopted() {
        let root = root();
        std::fs::write(root.join("payload/data"), b"bad!").unwrap();
        assert!(adopt_existing_torrent(&fixture(1), &root).await.is_err());
        assert!(!root.join("payload/.vtorrent-torrent").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn partial_migration_copies_only_verified_pieces_and_never_overwrites_target() {
        let source = root();
        std::fs::write(source.join("payload/data"), b"goodBAD!last").unwrap();
        let mut torrent = fixture(3);
        torrent.total_size = 12;
        torrent.piece_count = 3;
        torrent.files[0].length = 12;
        torrent.pieces = [b"good", b"miss", b"last"]
            .iter()
            .map(|p| Sha1::digest(p).into())
            .collect();
        let target = source.with_extension("migrated");
        let result = migrate_partial_torrent(&torrent, &source, &target)
            .await
            .unwrap();
        assert_eq!((result.copied_pieces, result.missing_pieces), (2, 1));
        assert_eq!(
            std::fs::read(source.join("payload/data")).unwrap(),
            b"goodBAD!last"
        );
        assert_eq!(
            std::fs::read(target.join("payload/data")).unwrap(),
            b"good\0\0\0\0last"
        );
        assert_eq!(
            std::fs::read(resume_file_path(&target, &torrent.info_hash)).unwrap(),
            [0xa0]
        );
        assert!(migrate_partial_torrent(&torrent, &source, &target)
            .await
            .is_err());
        std::fs::remove_dir_all(source).unwrap();
        std::fs::remove_dir_all(target).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn partial_migration_verifies_pieces_across_file_boundaries() {
        let source = root();
        std::fs::write(source.join("payload/a"), b"go").unwrap();
        std::fs::write(source.join("payload/b"), b"odBAD!last").unwrap();
        let mut torrent = fixture(4);
        torrent.total_size = 12;
        torrent.piece_count = 3;
        torrent.files = vec![
            TorrentFile {
                path: vec!["a".into()],
                length: 2,
                md5sum: None,
            },
            TorrentFile {
                path: vec!["b".into()],
                length: 10,
                md5sum: None,
            },
        ];
        torrent.pieces = [b"good", b"miss", b"last"]
            .iter()
            .map(|p| Sha1::digest(p).into())
            .collect();
        let target = source.with_extension("migrated");
        let result = migrate_partial_torrent(&torrent, &source, &target)
            .await
            .unwrap();
        assert_eq!((result.copied_pieces, result.missing_pieces), (2, 1));
        assert_eq!(std::fs::read(target.join("payload/a")).unwrap(), b"go");
        assert_eq!(
            std::fs::read(target.join("payload/b")).unwrap(),
            b"od\0\0\0\0last"
        );
        assert_eq!(
            std::fs::read(resume_file_path(&target, &torrent.info_hash)).unwrap(),
            [0xa0]
        );
        std::fs::remove_dir_all(source).unwrap();
        std::fs::remove_dir_all(target).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn partial_migration_does_not_copy_symlinked_source() {
        let source = root();
        let outside = source.join("outside");
        std::fs::write(&outside, b"good").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("payload/data")).unwrap();
        let target = source.with_extension("migrated");
        let result = migrate_partial_torrent(&fixture(5), &source, &target)
            .await
            .unwrap();
        assert_eq!((result.copied_pieces, result.missing_pieces), (0, 1));
        assert!(!target.join("payload/data").exists());
        assert_eq!(std::fs::read(&outside).unwrap(), b"good");
        std::fs::remove_dir_all(source).unwrap();
        std::fs::remove_dir_all(target).unwrap();
    }

    #[tokio::test]
    async fn torrent_content_cannot_overwrite_the_ownership_marker() {
        let root = root();
        let mut torrent = fixture(1);
        torrent.files[0].path = vec![".vtorrent-torrent".into()];
        assert!(!write_piece_to_disk(&torrent, &root, 0, b"good").await);
        assert_eq!(
            std::fs::read_to_string(root.join("payload/.vtorrent-torrent")).unwrap(),
            marker_value(&torrent.info_hash)
        );
        assert!(adopt_existing_torrent(&torrent, &root).await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_data_is_never_adopted() {
        let root = root();
        let outside = root.join("outside");
        std::fs::write(&outside, b"good").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("payload/data")).unwrap();
        assert!(adopt_existing_torrent(&fixture(1), &root).await.is_err());
        assert!(!root.join("payload/.vtorrent-torrent").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
