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
pub fn piece_length(metainfo: &Metainfo, index: u32) -> u64 {
    let start = index as u64 * metainfo.piece_length;
    let remaining = metainfo.total_size.saturating_sub(start);
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
    let Some(base) = sanitize_path(download_dir, std::slice::from_ref(&metainfo.name)) else {
        return false;
    };
    let mut all_ok = true;
    for (file_index, file_offset, bytes) in layout.piece_segments(piece_index, piece_data) {
        let file = &metainfo.files[file_index];
        // Build the output path, rejecting any component that would escape the
        // download directory (absolute paths, `..`, or empty components).
        let Some(path) = sanitize_path(&base, &file.path) else {
            all_ok = false;
            continue;
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::error!("Piece {}: create_dir_all failed: {}", piece_index, e);
                all_ok = false;
                continue;
            }
        }
        use tokio::io::AsyncSeekExt;
        let write_result: std::result::Result<(), std::io::Error> = async {
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .await?;
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
    let base = sanitize_path(download_dir, std::slice::from_ref(&metainfo.name))?;
    let piece_len = piece_length(metainfo, piece_index);
    let mut out = Vec::with_capacity(piece_len as usize);
    for (file_index, file_offset, byte_count) in layout.piece_segment_ranges(piece_index, piece_len)
    {
        let file = &metainfo.files[file_index];
        let path = sanitize_path(&base, &file.path)?;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut f = tokio::fs::File::open(&path).await.ok()?;
        let _ = f.seek(std::io::SeekFrom::Start(file_offset)).await;
        let mut buf = vec![0u8; byte_count as usize];
        f.read_exact(&mut buf).await.ok()?;
        out.extend_from_slice(&buf);
    }
    Some(out)
}

pub fn sanitize_path(base: &std::path::Path, components: &[String]) -> Option<std::path::PathBuf> {
    let mut path = base.to_path_buf();
    for comp in components {
        if comp.is_empty()
            || comp == "."
            || comp == ".."
            || comp.contains('/')
            || comp.contains('\\')
            // Windows: a colon makes the component an NTFS Alternate Data
            // Stream ("file.txt:ads") or drive-relative path ("C:x").
            || comp.contains(':')
            || std::path::Path::new(comp).is_absolute()
        {
            return None;
        }
        path.push(comp);
    }
    Some(path)
}
