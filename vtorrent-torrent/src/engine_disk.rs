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
    let Some(base) = safe_directory(download_dir, std::slice::from_ref(&metainfo.name)).await
    else {
        return false;
    };
    let mut all_ok = true;
    for (file_index, file_offset, bytes) in layout.piece_segments(piece_index, piece_data) {
        let file = &metainfo.files[file_index];
        // Resolve and create the parent with no symlink components; the final
        // open also uses O_NOFOLLOW on Unix.
        let Some(path) = safe_file_path(&base, &file.path).await else {
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
    let base = safe_directory(download_dir, std::slice::from_ref(&metainfo.name)).await?;
    let piece_len = piece_length(metainfo, piece_index);
    let mut out = Vec::with_capacity(piece_len as usize);
    for (file_index, file_offset, byte_count) in layout.piece_segment_ranges(piece_index, piece_len)
    {
        let file = &metainfo.files[file_index];
        let path = safe_file_path(&base, &file.path).await?;
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
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
) -> Option<std::path::PathBuf> {
    let (name, parents) = components.split_last()?;
    if !valid_component(name) {
        return None;
    }
    let parent = safe_directory(base, parents).await?;
    let path = parent.join(name);
    if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return None;
        }
    }
    Some(path)
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
