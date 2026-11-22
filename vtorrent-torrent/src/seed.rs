//! Inbound BitTorrent seeding.
//!
//! [`crate::engine`] drives outbound downloads: it dials peers, requests
//! pieces, and pays them. This module is the other half. It accepts inbound
//! peer connections, serves verified pieces for torrents we hold, and signs
//! bandwidth receipts so the downloading peer can settle and pay us.
//!
//! Serving is deliberately conservative:
//! - only torrents registered in the [`SeederRegistry`] are answered, so an
//!   arbitrary info-hash handshake cannot make us read the disk;
//! - every piece is re-hashed against the metainfo before its first block is
//!   sent, so a corrupted or truncated file is never served as valid;
//! - requests are bounded by connection count, per-connection outstanding
//!   requests, duplicate suppression, the shared upload limiter, and the
//!   session's seed-ratio target;
//! - the listener is opt-in and must be enabled explicitly by the operator.

use crate::engine::PeerConnection;
use crate::engine_disk::{piece_length, read_piece_from_disk};
use crate::error::{Result, TorrentError};
use crate::limits::TorrentRateLimiter;
use crate::metadata::{self, build_ut_vtr_address, build_ut_vtr_handshake};
use crate::metainfo::Metainfo;
use crate::peer_wire::PeerMessage;
use crate::session::SessionManager;
use crate::tracker::{tracker_target_allowed_with, AnnounceEvent, AnnounceRequest, HttpTracker};
use serde_bencode::value::Value;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use vtorrent_core::keys::PrivateKey;

/// Largest block we will serve. Real clients request 16 KiB.
const MAX_SERVED_BLOCK: u32 = 32 * 1024;
/// Outstanding requests one connection may accumulate.
const MAX_OUTSTANDING_PER_PEER: usize = 64;
/// How often we re-announce to trackers while seeding.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(1_800);
/// Retry cadence for a receipt that a peer has not yet observed.
const RECEIPT_RETRY: Duration = Duration::from_secs(1);
/// An idle connection is dropped after this long.
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Tracker announces attempted per torrent, bounded like the downloader.
const MAX_TRACKER_ATTEMPTS: usize = 8;

/// A torrent this node can serve.
struct SeedTorrent {
    session_id: String,
    metainfo: Metainfo,
    download_dir: PathBuf,
    /// Pieces already verified against the metainfo, so repeated block
    /// requests for the same piece hashes it once.
    verified: StdMutex<HashSet<u32>>,
}

impl SeedTorrent {
    fn new(session_id: String, metainfo: Metainfo, download_dir: PathBuf) -> Self {
        Self {
            session_id,
            metainfo,
            download_dir,
            verified: StdMutex::new(HashSet::new()),
        }
    }

    /// Whether the piece on disk matches its metainfo hash, caching success.
    async fn verified_piece(&self, index: u32) -> bool {
        {
            let verified = self
                .verified
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if verified.contains(&index) {
                return true;
            }
        }
        let Some(expected) = self.metainfo.pieces.get(index as usize) else {
            return false;
        };
        let Some(data) = read_piece_from_disk(&self.metainfo, &self.download_dir, index).await
        else {
            return false;
        };
        if data.len() != piece_length(&self.metainfo, index) as usize {
            return false;
        }
        if Sha1::digest(&data).as_slice() != expected.as_slice() {
            tracing::warn!(
                piece = index,
                "Refusing to seed a piece whose bytes do not match the metainfo"
            );
            return false;
        }
        self.verified
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(index);
        true
    }

    /// Read one requested block, verifying the whole piece first.
    async fn block(&self, index: u32, begin: u32, length: u32) -> Option<Vec<u8>> {
        if !self.verified_piece(index).await {
            return None;
        }
        let data = read_piece_from_disk(&self.metainfo, &self.download_dir, index).await?;
        let start = begin as usize;
        let end = start.saturating_add(length as usize).min(data.len());
        if start >= end {
            return None;
        }
        Some(data[start..end].to_vec())
    }
}

/// Torrents this node will serve to inbound peers.
#[derive(Default)]
pub struct SeederRegistry {
    entries: RwLock<HashMap<[u8; 20], Arc<SeedTorrent>>>,
}

impl SeederRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a torrent we hold. Idempotent for the same info-hash.
    pub async fn register(&self, session_id: String, metainfo: Metainfo, download_dir: PathBuf) {
        let mut entries = self.entries.write().await;
        entries
            .entry(metainfo.info_hash)
            .or_insert_with(|| Arc::new(SeedTorrent::new(session_id, metainfo, download_dir)));
    }

    /// Stop serving a torrent.
    pub async fn unregister(&self, info_hash: &[u8; 20]) {
        self.entries.write().await.remove(info_hash);
    }

    async fn get(&self, info_hash: &[u8; 20]) -> Option<Arc<SeedTorrent>> {
        self.entries.read().await.get(info_hash).cloned()
    }

    async fn snapshot(&self) -> Vec<Arc<SeedTorrent>> {
        self.entries.read().await.values().cloned().collect()
    }

    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

/// Listener configuration.
#[derive(Debug, Clone)]
pub struct SeederConfig {
    /// Address to accept inbound peer connections on.
    pub listen_addr: SocketAddr,
    /// Port advertised to trackers. Defaults to `listen_addr.port()`; a NAT
    /// mapping may differ from the local bind port.
    pub advertised_port: Option<u16>,
    /// Maximum simultaneous inbound connections.
    pub max_connections: usize,
}

impl SeederConfig {
    pub fn port(&self) -> u16 {
        self.advertised_port
            .unwrap_or_else(|| self.listen_addr.port())
    }
}

/// Shared state for every inbound connection.
#[derive(Clone)]
pub struct SeederState {
    pub registry: Arc<SeederRegistry>,
    pub sessions: Arc<RwLock<SessionManager>>,
    pub limits: Arc<TorrentRateLimiter>,
    pub seed_ratio_target: Arc<RwLock<f64>>,
    /// Hot-wallet WIF, read per receipt so seeding can sign once the wallet is
    /// unlocked and stops signing when it is locked again.
    pub wallet_wif: Arc<RwLock<Option<zeroize::Zeroizing<String>>>>,
    pub our_peer_id: [u8; 20],
    pub cancel: CancellationToken,
}

/// Run the inbound seeder until `cancel` fires.
///
/// Fails when the listen address cannot be bound, so a daemon asked to publish
/// a port fails visibly instead of silently not seeding.
pub async fn run_seeder(config: SeederConfig, state: SeederState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(config.listen_addr)
        .await
        .map_err(|error| TorrentError::Io(format!("torrent seed listener: {error}")))?;
    let bound = listener
        .local_addr()
        .map_err(|error| TorrentError::Io(error.to_string()))?;
    let registered = state.registry.len().await;
    tracing::info!(
        listen = %bound,
        advertised_port = config.port(),
        torrents = registered,
        max_connections = config.max_connections,
        "Torrent seeder listening for inbound peers"
    );

    spawn_tracker_announces(
        config.clone(),
        Arc::clone(&state.registry),
        state.cancel.clone(),
    );

    let slots = Arc::new(tokio::sync::Semaphore::new(config.max_connections.max(1)));
    loop {
        let permit = match slots.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };
        let accepted = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, addr) = match accepted {
            Ok(pair) => pair,
            Err(error) => {
                tracing::debug!("Torrent seed accept failed: {}", error);
                drop(permit);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if !tracker_target_allowed_with(addr.ip(), crate::tracker::private_targets_allowed()) {
            tracing::debug!(peer = %addr, "Refusing inbound torrent peer by address policy");
            drop(permit);
            continue;
        }
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, addr, state).await {
                tracing::debug!(peer = %addr, "Inbound torrent peer ended: {}", error);
            }
            drop(permit);
        });
    }

    tracing::info!("Torrent seeder stopped");
    Ok(())
}

/// Announce every registered torrent, then repeat periodically.
fn spawn_tracker_announces(
    config: SeederConfig,
    registry: Arc<SeederRegistry>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            for torrent in registry.snapshot().await {
                announce_to_trackers(&torrent.metainfo, config.port()).await;
            }
            tokio::select! {
                _ = tokio::time::sleep(ANNOUNCE_INTERVAL) => {}
                _ = cancel.cancelled() => return,
            }
        }
    });
}

/// Announce that we are serving `metainfo` on `port`.
async fn announce_to_trackers(metainfo: &Metainfo, port: u16) {
    let tracker = match HttpTracker::new() {
        Ok(tracker) => tracker,
        Err(error) => {
            tracing::debug!("Could not create the tracker client: {}", error);
            return;
        }
    };
    // The tracker only needs a stable peer id per process.
    let peer_id = [0x2du8; 20];
    for url in metainfo
        .all_trackers()
        .into_iter()
        .take(MAX_TRACKER_ATTEMPTS)
    {
        if !url.starts_with("http") {
            continue;
        }
        let request = AnnounceRequest {
            tracker_url: url.clone(),
            info_hash: metainfo.info_hash,
            peer_id,
            port,
            uploaded: 0,
            downloaded: 0,
            left: 0,
            event: AnnounceEvent::Completed,
            num_want: 50,
        };
        if let Err(error) = tracker.announce(&request).await {
            tracing::debug!(tracker = %url, "Seed announce failed: {}", error);
        }
    }
}

/// Serve one inbound peer connection.
async fn serve_connection(stream: TcpStream, addr: SocketAddr, state: SeederState) -> Result<()> {
    // The peer names the torrent first. Answer only for one we hold, so an
    // arbitrary info-hash cannot make us read the disk.
    let (info_hash, peer_id, mut conn) = PeerConnection::accept(stream).await?;
    let Some(torrent) = state.registry.get(&info_hash).await else {
        return Err(TorrentError::PeerWireError("unknown info-hash".into()));
    };
    conn.accept_reply(info_hash, state.our_peer_id).await?;
    let piece_bytes = torrent.metainfo.piece_length.max(16 * 1024) as usize;
    conn.set_max_message_len(piece_bytes.saturating_add(64 * 1024));

    // Announce the pieces the engine already verified when it finished the
    // download. Re-hashing a whole large torrent here would stall the first
    // inbound peer; each piece is still verified lazily before its first block
    // is served, so a corrupted file cannot be uploaded as valid.
    let mut bits = vec![0u8; (torrent.metainfo.piece_count as usize).div_ceil(8)];
    if let Ok(resume) = tokio::fs::read(crate::engine_disk::resume_file_path(
        &torrent.download_dir,
        &torrent.metainfo.info_hash,
    ))
    .await
    {
        for (index, byte) in resume.iter().take(bits.len()).enumerate() {
            bits[index] = *byte;
        }
    } else {
        // No resume file: all bits stay clear, and pieces are granted as they
        // verify when requested.
        tracing::debug!("Seeding a torrent with no verified resume bitfield");
    }
    conn.send(&PeerMessage::Bitfield { bits }).await?;
    conn.send(&PeerMessage::Unchoke).await?;
    conn.send(&PeerMessage::Extended {
        id: 0,
        payload: build_ut_vtr_handshake(1),
    })
    .await?;

    let our_address = {
        let guard = state.sessions.read().await;
        guard
            .get_session(&torrent.session_id)
            .map(|session| session.wallet_address.clone())
            .unwrap_or_default()
    };
    conn.send(&PeerMessage::Extended {
        id: 1,
        payload: build_ut_vtr_address(&our_address),
    })
    .await?;

    let served_requests: StdMutex<HashMap<(u32, u32, u32), std::time::Instant>> =
        StdMutex::new(HashMap::new());
    let outstanding = AtomicUsize::new(0);
    let mut peer_address: Option<String> = None;
    let mut receipt_id: Option<u8> = None;
    let mut last_receipt_at: Option<std::time::Instant> = None;
    // Receipts are cumulative. Send the first promptly (a client may finish and
    // disconnect quickly) and another whenever the reported total has advanced,
    // respecting the receiver's 30 s rate limit.
    let mut next_receipt = std::time::Instant::now() + RECEIPT_RETRY;
    let mut uploaded: u64 = 0;
    let mut reported_uploaded: u64 = 0;
    let mut last_frame_at = std::time::Instant::now();

    loop {
        if state.cancel.is_cancelled() {
            return Ok(());
        }
        if std::time::Instant::now() >= next_receipt {
            next_receipt = std::time::Instant::now() + RECEIPT_RETRY;
            let receipt_key = state
                .wallet_wif
                .read()
                .await
                .as_deref()
                .and_then(|wif| PrivateKey::from_wif(wif).ok());
            if let (Some(key), Some(id), Some(peer)) =
                (receipt_key.as_ref(), receipt_id, peer_address.as_deref())
            {
                // The receiver drops receipts inside a 30 s window, so only
                // report when the total advanced and that window has passed.
                let due =
                    last_receipt_at.is_none_or(|last| last.elapsed() >= Duration::from_secs(30));
                if due && uploaded > reported_uploaded {
                    let now = now_secs();
                    let receipt = vtorrent_core::receipt::BandwidthReceipt {
                        info_hash,
                        window_start: 0,
                        window_end: now,
                        uploaded_by_me: uploaded,
                        downloaded_by_me: 0,
                        peer: peer.to_string(),
                        signer: our_address.clone(),
                        settlement_id: now,
                        epoch: seed_epoch(),
                    };
                    if let Ok(signature) = receipt.sign(key) {
                        if let Ok(public_key) = key.public_key() {
                            let pubkey = if key.is_compressed() {
                                public_key.serialize().to_vec()
                            } else {
                                public_key.serialize_uncompressed().to_vec()
                            };
                            let envelope = vtorrent_core::receipt::SignedBandwidthReceipt {
                                receipt,
                                pubkey,
                                signature,
                            };
                            if conn
                                .send(&PeerMessage::Extended {
                                    id,
                                    payload: metadata::encode_receipt(&envelope),
                                })
                                .await
                                .is_ok()
                            {
                                last_receipt_at = Some(std::time::Instant::now());
                                reported_uploaded = uploaded;
                            }
                        }
                    }
                }
            }
        }

        // Wake for the next receipt even when the peer is idle: a fast
        // download finishes and the connection then blocks in `recv` forever,
        // so a pure idle timeout would never let the receipt timer fire.
        let wait = next_receipt
            .saturating_duration_since(std::time::Instant::now())
            .min(CONNECTION_IDLE_TIMEOUT)
            .max(Duration::from_millis(50));
        let message = match tokio::time::timeout(wait, conn.recv()).await {
            Ok(result) => {
                last_frame_at = std::time::Instant::now();
                result?
            }
            Err(_) => {
                // No frame arrived; loop to emit a due receipt, then idle out
                // only after a real inactivity window.
                if std::time::Instant::now() >= next_receipt {
                    continue;
                }
                if last_frame_at.elapsed() >= CONNECTION_IDLE_TIMEOUT {
                    return Ok(());
                }
                continue;
            }
        };
        match message {
            PeerMessage::Extended { id: 0, payload } => {
                if let Some(Ok(Value::Dict(dict))) =
                    crate::bencode_guard::parse_untrusted::<Value>(&payload)
                {
                    if let Some(Value::Dict(map)) = dict.get(b"m".as_slice()) {
                        if let Some(Value::Int(id)) = map.get(b"ut_vtr_receipt".as_slice()) {
                            if (1..=u8::MAX as i64).contains(id) {
                                receipt_id = Some(*id as u8);
                            }
                        }
                    }
                }
            }
            PeerMessage::Extended { id, payload } => {
                if Some(id) == receipt_id {
                    record_peer_receipt(
                        &state,
                        &torrent.session_id,
                        peer_address.as_deref(),
                        &our_address,
                        &payload,
                    )
                    .await;
                } else if let Ok(address) = metadata::parse_ut_vtr_address(&payload) {
                    tracing::debug!(peer = %addr, %address, "Inbound peer identified");
                    peer_address = Some(address);
                    // Sign promptly: a fast download can finish before the
                    // periodic tick, and a peer that never receives a receipt
                    // can never verify what it owes us.
                    next_receipt = std::time::Instant::now();
                }
            }
            PeerMessage::Request {
                index,
                begin,
                length,
            } => {
                if length == 0 || length > MAX_SERVED_BLOCK || index >= torrent.metainfo.piece_count
                {
                    continue;
                }
                // Only suppress a request that is still being served on this
                // connection. A longer window would silently refuse a
                // legitimate re-request after a cancel or a reconnect.
                {
                    let mut requests = served_requests
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    requests.retain(|_, deadline| *deadline > std::time::Instant::now());
                    if requests.contains_key(&(index, begin, length)) {
                        continue;
                    }
                    requests.insert((index, begin, length), std::time::Instant::now());
                }
                if outstanding.fetch_add(1, Ordering::SeqCst) >= MAX_OUTSTANDING_PER_PEER {
                    outstanding.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                if seeding_ratio_reached(&state, &torrent.session_id).await {
                    outstanding.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let block_result = torrent.block(index, begin, length).await;
                // The request is no longer in flight once the read finishes.
                served_requests
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&(index, begin, length));
                if let Some(block) = block_result {
                    let peer_key = peer_address.clone().unwrap_or_else(|| hex::encode(peer_id));
                    if conn
                        .send(&PeerMessage::Piece {
                            index,
                            begin,
                            data: block.clone(),
                        })
                        .await
                        .is_ok()
                    {
                        uploaded = uploaded.saturating_add(block.len() as u64);
                        record_upload(&state, &torrent.session_id, &peer_key, block.len() as u64)
                            .await;
                        state
                            .limits
                            .consume_upload(block.len() as u64, &state.cancel)
                            .await;
                    }
                }
                outstanding.fetch_sub(1, Ordering::SeqCst);
            }
            PeerMessage::KeepAlive
            | PeerMessage::Interested
            | PeerMessage::NotInterested
            | PeerMessage::Choke
            | PeerMessage::Unchoke
            | PeerMessage::Cancel { .. }
            | PeerMessage::Have { .. }
            | PeerMessage::Bitfield { .. }
            | PeerMessage::Handshake { .. }
            | PeerMessage::Port { .. } => {}
            PeerMessage::Piece { .. } => {
                // We never request from an inbound peer; ignore unsolicited
                // piece data instead of buffering it.
            }
        }
    }
}

/// Stop serving once the session's seed-ratio target is reached.
async fn seeding_ratio_reached(state: &SeederState, session_id: &str) -> bool {
    let target = *state.seed_ratio_target.read().await;
    if target <= 0.0 {
        return false;
    }
    let guard = state.sessions.read().await;
    guard
        .get_session(session_id)
        .map(|session| {
            session.bytes_downloaded > 0
                && (session.bytes_uploaded as f64) >= (session.bytes_downloaded as f64) * target
        })
        .unwrap_or(false)
}

/// Credit uploaded bytes to the session and its incentive account.
async fn record_upload(state: &SeederState, session_id: &str, peer_key: &str, bytes: u64) {
    let mut guard = state.sessions.write().await;
    if let Ok(session) = guard.get_session_mut(session_id) {
        session.record_upload(peer_key, bytes);
    }
}

/// Verify and record a signed receipt from the peer we are uploading to.
///
/// The peer signs that it downloaded `uploaded_by_me` bytes from us, and names
/// our address in `peer`. We record only when it matches what we credited.
async fn record_peer_receipt(
    state: &SeederState,
    session_id: &str,
    peer_address: Option<&str>,
    our_address: &str,
    payload: &[u8],
) {
    let Some(peer_address) = peer_address else {
        return;
    };
    let Ok(envelope) = metadata::decode_receipt(payload) else {
        return;
    };
    let receipt = &envelope.receipt;
    let now = now_secs();
    if receipt.window_start > receipt.window_end
        || receipt.window_end > now.saturating_add(600)
        || receipt.settlement_id > now.saturating_add(600)
    {
        return;
    }
    // From the peer's point of view: it uploaded nothing to us, it downloaded
    // from us, it addresses our wallet, and it signs as itself.
    if receipt.peer != our_address || receipt.signer != peer_address {
        return;
    }
    let already = {
        let guard = state.sessions.read().await;
        guard
            .get_session(session_id)
            .ok()
            .and_then(|session| session.incentive_accounts.get(peer_address))
            .map(|account| {
                account.peer_epoch == Some(receipt.epoch)
                    && receipt.settlement_id <= account.last_receipt_id
            })
            .unwrap_or(false)
    };
    if already {
        return;
    }
    if !envelope.verify().unwrap_or(false) {
        return;
    }
    let updated = {
        let mut guard = state.sessions.write().await;
        guard.get_session_mut(session_id).ok().and_then(|session| {
            let account = session.incentive_accounts.get_mut(peer_address)?;
            if !account.record_receipt(receipt.clone()) {
                return None;
            }
            Some(account.clone())
        })
    };
    if let Some(account) = updated {
        let info_hash = receipt.info_hash;
        let journal = {
            let guard = state.sessions.read().await;
            guard.journal()
        };
        if let Some(journal) = journal {
            let peer = peer_address.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                journal.upsert(info_hash, &peer, account.receipt_epoch, &account)
            })
            .await;
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Signs the receipts we send from the seed side.
fn seed_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn metainfo(info_hash: [u8; 20], data: &[u8]) -> Metainfo {
        let pieces: Vec<[u8; 20]> = data.chunks(64).map(|c| Sha1::digest(c).into()).collect();
        Metainfo {
            info_hash,
            name: "seed-test".into(),
            total_size: data.len() as u64,
            piece_length: 64,
            piece_count: pieces.len() as u32,
            pieces,
            announce: None,
            announce_list: Vec::new(),
            files: vec![crate::metainfo::TorrentFile {
                path: vec!["seed-test".into()],
                length: data.len() as u64,
                md5sum: None,
            }],
            created_at: None,
            comment: None,
            is_private: false,
        }
    }

    #[tokio::test]
    async fn an_unknown_torrent_is_not_registered() {
        let registry = SeederRegistry::new();
        let dir = tempfile::tempdir().unwrap();
        registry
            .register(
                "session".into(),
                metainfo([1u8; 20], b"abcdef"),
                dir.path().into(),
            )
            .await;
        assert!(registry.get(&[1u8; 20]).await.is_some());
        assert!(registry.get(&[2u8; 20]).await.is_none());
        registry.unregister(&[1u8; 20]).await;
        assert!(registry.is_empty().await);
    }

    #[tokio::test]
    async fn serving_refuses_a_piece_that_does_not_match_the_metainfo() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"the real content of this torrent";
        let torrent = Arc::new(SeedTorrent::new(
            "session".into(),
            metainfo([3u8; 20], data),
            dir.path().into(),
        ));
        let base = dir.path().join("seed-test");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join(".vtorrent-torrent"),
            format!("v1:{}\n", hex::encode([3u8; 20])),
        )
        .unwrap();
        std::fs::write(base.join("seed-test"), data).unwrap();
        assert!(
            torrent.verified_piece(0).await,
            "valid data must be servable"
        );
        assert_eq!(torrent.block(0, 0, 8).await.unwrap(), &data[..8]);

        // One flipped byte must make the piece unservable rather than sent.
        let mut corrupt = data.to_vec();
        corrupt[0] ^= 0xff;
        std::fs::write(base.join("seed-test"), &corrupt).unwrap();
        let fresh = Arc::new(SeedTorrent::new(
            "session".into(),
            metainfo([3u8; 20], data),
            dir.path().into(),
        ));
        assert!(
            !fresh.verified_piece(0).await,
            "a corrupted piece must never be served"
        );
        assert!(fresh.block(0, 0, 8).await.is_none());
    }

    #[tokio::test]
    async fn the_server_handshake_rejects_a_peer_without_extension_support() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            PeerConnection::accept(stream).await
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                &PeerMessage::Handshake {
                    info_hash: [9u8; 20],
                    peer_id: [8u8; 20],
                    // BEP-10 bit deliberately clear.
                    reserved: [0u8; 8],
                }
                .encode(),
            )
            .await
            .unwrap();
        assert!(
            server.await.unwrap().is_err(),
            "a peer without the extension protocol can only consume uploads unpaid"
        );
    }

    #[tokio::test]
    async fn the_server_handshake_returns_the_requested_torrent() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (hash, peer, mut conn) = PeerConnection::accept(stream).await.unwrap();
            conn.accept_reply(hash, [0x2du8; 20]).await.unwrap();
            (hash, peer)
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                &PeerMessage::Handshake {
                    info_hash: [4u8; 20],
                    peer_id: [5u8; 20],
                    reserved: [0, 0, 0, 0, 0, 0x10, 0, 0],
                }
                .encode(),
            )
            .await
            .unwrap();
        let mut reply = [0u8; 68];
        client.read_exact(&mut reply).await.unwrap();
        let (hash, peer) = server.await.unwrap();
        assert_eq!(hash, [4u8; 20]);
        assert_eq!(peer, [5u8; 20]);
        match PeerMessage::decode_handshake(&reply).unwrap().unwrap().0 {
            PeerMessage::Handshake {
                info_hash: echoed,
                peer_id: ours,
                ..
            } => {
                assert_eq!(echoed, [4u8; 20], "the server must echo the torrent");
                assert_eq!(ours, [0x2du8; 20]);
            }
            other => panic!("expected a handshake, got {other:?}"),
        }
    }
}
