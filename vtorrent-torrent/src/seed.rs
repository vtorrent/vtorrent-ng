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
//! - requests are bounded by connection count, canonical block geometry,
//!   duplicate suppression, the shared upload limiter, and the session's
//!   seed-ratio target;
//! - the listener is opt-in and must be enabled explicitly by the operator.

use crate::engine::PeerConnection;
use crate::engine_disk::{piece_length, read_block_from_disk, read_piece_from_disk};
use crate::error::{Result, TorrentError};
use crate::limits::TorrentRateLimiter;
use crate::metadata::{
    self, build_ut_vtr_address, build_ut_vtr_handshake, OUR_RECEIPT_ID, OUR_UT_VTR_ID,
};
use crate::metainfo::Metainfo;
use crate::peer_wire::{valid_block_request, PeerMessage};
use crate::session::SessionManager;
use crate::tracker::{tracker_target_allowed_with, AnnounceEvent, AnnounceRequest, HttpTracker};
use serde_bencode::value::Value;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use vtorrent_core::keys::PrivateKey;

/// How often we re-announce to trackers while seeding.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(1_800);
/// How often the announce task looks for newly registered torrents.
const ANNOUNCE_SCAN: Duration = Duration::from_secs(60);
/// Retry cadence for a receipt that a peer has not yet observed.
const RECEIPT_RETRY: Duration = Duration::from_secs(1);
/// An idle connection is dropped after this long.
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Tracker announces attempted per torrent, bounded like the downloader.
const MAX_TRACKER_ATTEMPTS: usize = 8;
/// Receipts from one peer are verified at most this often, across reconnects.
const PEER_RECEIPT_INTERVAL: Duration = Duration::from_secs(25);
/// Concurrent connections one source IP may hold.
const MAX_CONNECTIONS_PER_IP: usize = 8;

/// Read the engine's verified resume bitfield for a completed torrent.
///
/// The engine writes this file only after each bit has been verified against
/// the data. Registration itself is gated on the session being complete, so
/// trusting these bits is equivalent to trusting the download that produced
/// the files. Any piece not in the file is still hashed on its first request.
async fn resume_verified(metainfo: &Metainfo, download_dir: &std::path::Path) -> HashSet<u32> {
    let mut verified = HashSet::new();
    let Ok(bytes) = tokio::fs::read(crate::engine_disk::resume_file_path(
        download_dir,
        &metainfo.info_hash,
    ))
    .await
    else {
        return verified;
    };
    let count = metainfo.piece_count as usize;
    for (index, byte) in bytes.iter().enumerate() {
        for bit in 0..8 {
            let piece = index * 8 + bit;
            if piece < count && byte & (0x80 >> bit) != 0 {
                verified.insert(piece as u32);
            }
        }
    }
    verified
}

/// A torrent this node can serve.
struct SeedTorrent {
    session_id: String,
    metainfo: Metainfo,
    download_dir: PathBuf,
    /// Pieces already verified against the metainfo, so repeated block
    /// requests for the same piece hash it once.
    verified: StdMutex<HashSet<u32>>,
    /// Pieces that failed verification, with a retry deadline. Without this a
    /// corrupt piece re-read the whole piece on every distinct block request.
    failed: StdMutex<HashMap<u32, std::time::Instant>>,
    /// Bound concurrent whole-piece verification reads. Each permit can hold
    /// one piece-sized buffer, so this is the real memory bound on the
    /// first-request path.
    verify_limit: tokio::sync::Semaphore,
    /// Cancelled when the torrent is unregistered, so an in-flight connection
    /// stops serving immediately instead of holding an `Arc` forever.
    cancel: CancellationToken,
}

impl SeedTorrent {
    fn new(
        session_id: String,
        metainfo: Metainfo,
        download_dir: PathBuf,
        verified: HashSet<u32>,
    ) -> Self {
        Self {
            session_id,
            metainfo,
            download_dir,
            verified: StdMutex::new(verified),
            failed: StdMutex::new(HashMap::new()),
            verify_limit: tokio::sync::Semaphore::new(4),
            cancel: CancellationToken::new(),
        }
    }

    /// Whether the piece on disk matches its metainfo hash, caching success
    /// and briefly caching failure so a corrupt piece cannot force a full read
    /// and hash on every block request.
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
        let now = std::time::Instant::now();
        {
            let failed = self
                .failed
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if failed.get(&index).is_some_and(|until| *until > now) {
                return false;
            }
        }
        // At most four pieces are read and hashed at once. Waiters re-check
        // the cache after acquiring the permit, so concurrent first requests
        // for the same piece hash it once.
        let Ok(_permit) = self.verify_limit.acquire().await else {
            return false;
        };
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
            self.note_failure(index);
            return false;
        };
        if data.len() != piece_length(&self.metainfo, index) as usize {
            self.note_failure(index);
            return false;
        }
        if Sha1::digest(&data).as_slice() != expected.as_slice() {
            tracing::warn!(
                piece = index,
                "Refusing to seed a piece whose bytes do not match the metainfo"
            );
            self.note_failure(index);
            return false;
        }
        self.failed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&index);
        self.verified
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(index);
        true
    }

    fn note_failure(&self, index: u32) {
        self.failed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(index, std::time::Instant::now() + Duration::from_secs(60));
    }

    /// Read one requested block, verifying the whole piece first.
    ///
    /// Verification reads and hashes the whole piece once; after that, only the
    /// requested byte range is read. Re-reading the entire piece for every
    /// 16 KiB block would let one peer multiply disk I/O by the piece size.
    async fn block(&self, index: u32, begin: u32, length: u32) -> Option<Vec<u8>> {
        if length == 0 {
            return None;
        }
        let piece_len = piece_length(&self.metainfo, index);
        if begin as u64 + length as u64 > piece_len {
            return None;
        }
        if !self.verified_piece(index).await {
            return None;
        }
        read_block_from_disk(&self.metainfo, &self.download_dir, index, begin, length).await
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

    /// Register a torrent we hold.
    ///
    /// Replaces a stale entry for the same info-hash (for example one left by
    /// a session that was evicted and then re-added) so the new session's
    /// completion is not a no-op against the old session id.
    pub async fn register(&self, session_id: String, metainfo: Metainfo, download_dir: PathBuf) {
        let info_hash = metainfo.info_hash;
        let verified = resume_verified(&metainfo, &download_dir).await;
        let new = Arc::new(SeedTorrent::new(
            session_id,
            metainfo,
            download_dir,
            verified,
        ));
        let mut entries = self.entries.write().await;
        if let Some(old) = entries.insert(info_hash, new) {
            old.cancel.cancel();
        }
    }

    /// Register only if no *live* session with a different id owns the
    /// info-hash. A completion callback that read a session before a
    /// remove/re-add must not replace the newer session's entry.
    pub async fn register_if_live(
        &self,
        session_id: String,
        metainfo: Metainfo,
        download_dir: PathBuf,
        sessions: &RwLock<SessionManager>,
    ) -> bool {
        let info_hash = metainfo.info_hash;
        let verified = resume_verified(&metainfo, &download_dir).await;
        let new = Arc::new(SeedTorrent::new(
            session_id.clone(),
            metainfo,
            download_dir,
            verified,
        ));
        let mut entries = self.entries.write().await;
        if let Some(existing) = entries.get(&info_hash) {
            if existing.session_id != session_id
                && sessions
                    .read()
                    .await
                    .get_session(&existing.session_id)
                    .is_ok()
            {
                return false;
            }
        }
        if let Some(old) = entries.insert(info_hash, new) {
            old.cancel.cancel();
        }
        true
    }

    /// Drop entries whose session no longer exists (evicted terminal sessions).
    pub async fn retain_sessions(&self, live: &HashSet<String>) {
        let mut entries = self.entries.write().await;
        let mut removed = Vec::new();
        entries.retain(|_, torrent| {
            if live.contains(&torrent.session_id) {
                true
            } else {
                removed.push(torrent.cancel.clone());
                false
            }
        });
        for token in removed {
            token.cancel();
        }
    }

    /// Stop serving a torrent, including on connections already open.
    pub async fn unregister(&self, info_hash: &[u8; 20]) {
        let mut entries = self.entries.write().await;
        if let Some(torrent) = entries.remove(info_hash) {
            torrent.cancel.cancel();
        }
    }

    /// Stop serving only if the entry still belongs to `session_id`.
    ///
    /// Removal and registration can race: an unconditional unregister would
    /// cancel a newer session that re-added the same torrent.
    pub async fn unregister_session(&self, info_hash: &[u8; 20], session_id: &str) {
        let mut entries = self.entries.write().await;
        let matches = entries
            .get(info_hash)
            .is_some_and(|torrent| torrent.session_id == session_id);
        if matches {
            if let Some(torrent) = entries.remove(info_hash) {
                torrent.cancel.cancel();
            }
        }
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
    /// Last accepted receipt per peer address, shared across that peer's
    /// connections so reconnecting cannot reset the 30 s verification budget.
    pub receipt_throttle: Arc<StdMutex<HashMap<String, std::time::Instant>>>,
    pub cancel: CancellationToken,
}

/// Aggregate IPv6 sources by /64 so one host cannot rotate addresses inside
/// its own prefix to bypass the per-IP cap.
fn connection_ip_key(ip: std::net::IpAddr) -> std::net::IpAddr {
    match ip {
        std::net::IpAddr::V6(v6) => {
            let segments = v6.segments();
            std::net::IpAddr::V6(std::net::Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            ))
        }
        ip => ip,
    }
}

/// Releases one per-IP connection slot when a task ends, including on panic.
struct IpConnectionSlot {
    counts: Arc<StdMutex<HashMap<std::net::IpAddr, usize>>>,
    ip: std::net::IpAddr,
}

impl Drop for IpConnectionSlot {
    fn drop(&mut self) {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = counts.get_mut(&self.ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.ip);
            }
        }
    }
}

/// Bind the inbound listener.
///
/// The daemon calls this before publishing the seeder registry, so a bind
/// failure is a startup error instead of a node that reports `seeding: true`
/// while accepting nothing.
pub async fn bind_seeder(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|error| TorrentError::Io(format!("torrent seed listener: {error}")))
}

/// Run the inbound seeder until `cancel` fires.
pub async fn run_seeder(
    listener: tokio::net::TcpListener,
    config: SeederConfig,
    state: SeederState,
) -> Result<()> {
    let bound = listener
        .local_addr()
        .map_err(|error| TorrentError::Io(error.to_string()))?;
    // An ephemeral listen port (`...:0`) must advertise the port the OS chose,
    // not the configured zero.
    let advertised_port = config.advertised_port.unwrap_or_else(|| bound.port());
    let registered = state.registry.len().await;
    tracing::info!(
        listen = %bound,
        advertised_port,
        torrents = registered,
        max_connections = config.max_connections,
        "Torrent seeder listening for inbound peers"
    );

    spawn_tracker_announces(
        advertised_port,
        state.our_peer_id,
        Arc::clone(&state.registry),
        Arc::clone(&state.sessions),
        state.cancel.clone(),
    );

    let slots = Arc::new(tokio::sync::Semaphore::new(config.max_connections.max(1)));
    let per_ip: Arc<StdMutex<HashMap<std::net::IpAddr, usize>>> =
        Arc::new(StdMutex::new(HashMap::new()));
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
        let ip = connection_ip_key(addr.ip());
        {
            let mut counts = per_ip.lock().unwrap_or_else(|e| e.into_inner());
            let count = counts.entry(ip).or_insert(0);
            if *count >= MAX_CONNECTIONS_PER_IP {
                tracing::debug!(peer = %addr, "Refusing torrent peer: per-IP connection cap");
                drop(permit);
                continue;
            }
            *count += 1;
        }
        let ip_slot = IpConnectionSlot {
            counts: Arc::clone(&per_ip),
            ip,
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _ip_slot = ip_slot;
            if let Err(error) = serve_connection(stream, addr, state).await {
                tracing::debug!(peer = %addr, "Inbound torrent peer ended: {}", error);
            }
            drop(permit);
        });
    }

    tracing::info!("Torrent seeder stopped");
    Ok(())
}

/// Announce every registered torrent promptly, then keep re-announcing.
///
/// A short scan means a torrent registered after the seeder started is
/// discoverable immediately instead of waiting for the next 30-minute tick.
/// The first announce for a torrent is `started`, later ones carry no event.
fn spawn_tracker_announces(
    port: u16,
    peer_id: [u8; 20],
    registry: Arc<SeederRegistry>,
    sessions: Arc<RwLock<SessionManager>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut announced: HashMap<[u8; 20], std::time::Instant> = HashMap::new();
        loop {
            // Evicted sessions must not be announced forever. Prune the
            // registry before taking the announce snapshot.
            let live_sessions: HashSet<String> = {
                let guard = sessions.read().await;
                guard
                    .list_sessions()
                    .iter()
                    .map(|session| session.id.clone())
                    .collect()
            };
            registry.retain_sessions(&live_sessions).await;
            let torrents = registry.snapshot().await;
            let now = std::time::Instant::now();
            for torrent in &torrents {
                let first = !announced.contains_key(&torrent.metainfo.info_hash);
                let due = announced
                    .get(&torrent.metainfo.info_hash)
                    .is_none_or(|last| now.duration_since(*last) >= ANNOUNCE_INTERVAL);
                if due {
                    let event = if first {
                        AnnounceEvent::Started
                    } else {
                        AnnounceEvent::None
                    };
                    announce_to_trackers(&torrent.metainfo, port, peer_id, event).await;
                    announced.insert(torrent.metainfo.info_hash, std::time::Instant::now());
                }
            }
            // Forget torrents that were unregistered so a re-added torrent
            // announces `started` again.
            let live: HashSet<[u8; 20]> = torrents
                .iter()
                .map(|torrent| torrent.metainfo.info_hash)
                .collect();
            announced.retain(|info_hash, _| live.contains(info_hash));

            tokio::select! {
                _ = tokio::time::sleep(ANNOUNCE_SCAN) => {}
                _ = cancel.cancelled() => return,
            }
        }
    });
}

/// Announce that we are serving `metainfo` on `port`.
async fn announce_to_trackers(
    metainfo: &Metainfo,
    port: u16,
    peer_id: [u8; 20],
    event: AnnounceEvent,
) {
    let tracker = match HttpTracker::new() {
        Ok(tracker) => tracker,
        Err(error) => {
            tracing::debug!("Could not create the tracker client: {}", error);
            return;
        }
    };
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
            event,
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
    // This connection only ever serves. Requests are 17 bytes and the largest
    // legitimate inbound payload is a bitfield or extension message, so cap
    // inbound buffering independently of the torrent's piece size.
    conn.set_max_message_len(crate::peer_wire::MAX_INBOUND_FRAME);

    // Registration only happens for a complete session, so every piece was
    // verified when the download finished. Advertise all of them. Each piece
    // is still re-hashed before its first block, so a file corrupted after
    // registration is refused rather than served. Deriving the bitfield from a
    // missing resume file would advertise nothing and hide the whole torrent.
    let piece_count = torrent.metainfo.piece_count as usize;
    let mut bits = vec![0u8; piece_count.div_ceil(8)];
    for index in 0..piece_count {
        bits[index / 8] |= 0x80 >> (index % 8);
    }
    conn.send(&PeerMessage::Bitfield { bits }).await?;
    conn.send(&PeerMessage::Unchoke).await?;
    conn.send(&PeerMessage::Extended {
        id: 0,
        payload: build_ut_vtr_handshake(OUR_UT_VTR_ID),
    })
    .await?;

    let our_address = {
        let guard = state.sessions.read().await;
        guard
            .get_session(&torrent.session_id)
            .map(|session| session.wallet_address.clone())
            .unwrap_or_default()
    };
    let mut peer_address: Option<String> = None;
    let mut last_peer_receipt_at: Option<std::time::Instant> = None;
    let mut peer_ut_vtr_id: Option<u8> = None;
    let mut peer_receipt_id: Option<u8> = None;
    let mut address_sent = false;
    let mut last_receipt_at: Option<std::time::Instant> = None;
    // Recently served requests. A re-request inside the window is dropped
    // instead of being counted and re-read, so a peer cannot inflate the
    // receipt with duplicates by replaying the same Request. Bounded per
    // connection: entries expire, and a flood clears the map.
    let served_requests: StdMutex<HashMap<(u32, u32, u32), std::time::Instant>> =
        StdMutex::new(HashMap::new());
    // One epoch per connection. A fresh epoch on every receipt would make the
    // receiver treat each one as a peer restart and defeat its settlement-id
    // monotonicity guard.
    let receipt_epoch = now_nanos();
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
            if let (Some(key), Some(id), Some(peer)) = (
                receipt_key.as_ref(),
                peer_receipt_id,
                peer_address.as_deref(),
            ) {
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
                        epoch: receipt_epoch,
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
        let message = match tokio::select! {
            _ = torrent.cancel.cancelled() => return Ok(()),
            _ = state.cancel.cancelled() => return Ok(()),
            result = tokio::time::timeout(wait, conn.recv()) => result,
        } {
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
                        if let Some(Value::Int(id)) = map.get(b"ut_vtr".as_slice()) {
                            if (1..=u8::MAX as i64).contains(id) {
                                peer_ut_vtr_id = Some(*id as u8);
                            }
                        }
                        if let Some(Value::Int(id)) = map.get(b"ut_vtr_receipt".as_slice()) {
                            if (1..=u8::MAX as i64).contains(id) {
                                peer_receipt_id = Some(*id as u8);
                            }
                        }
                    }
                }
                // Send our address on the id the PEER advertised for receiving
                // it, now that we know it.
                if !address_sent {
                    if let Some(id) = peer_ut_vtr_id {
                        if conn
                            .send(&PeerMessage::Extended {
                                id,
                                payload: build_ut_vtr_address(&our_address),
                            })
                            .await
                            .is_ok()
                        {
                            address_sent = true;
                        }
                    }
                }
            }
            PeerMessage::Extended { id, payload } => {
                // A peer sends receipts to us on the id WE advertised, not on
                // the id it advertised for receiving.
                if id == OUR_RECEIPT_ID {
                    let Some(peer) = peer_address.as_deref() else {
                        continue;
                    };
                    // Bound verification both per connection and per address.
                    // A peer that rotates its self-asserted address on one
                    // connection must not get a fresh ECDSA/fsync budget.
                    let now = std::time::Instant::now();
                    if last_peer_receipt_at
                        .is_some_and(|last| now.duration_since(last) < PEER_RECEIPT_INTERVAL)
                    {
                        continue;
                    }
                    {
                        let mut seen = state
                            .receipt_throttle
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        seen.retain(|_, at| now.duration_since(*at) < PEER_RECEIPT_INTERVAL);
                        if seen
                            .get(peer)
                            .is_some_and(|at| now.duration_since(*at) < PEER_RECEIPT_INTERVAL)
                        {
                            continue;
                        }
                        if seen.len() >= 10_000 {
                            if let Some(oldest) = seen
                                .iter()
                                .min_by_key(|(_, at)| **at)
                                .map(|(address, _)| address.clone())
                            {
                                seen.remove(&oldest);
                            }
                        }
                        seen.insert(peer.to_string(), now);
                    }
                    last_peer_receipt_at = Some(now);
                    record_peer_receipt(
                        &state,
                        &torrent.session_id,
                        &info_hash,
                        peer_address.as_deref(),
                        &our_address,
                        &payload,
                    )
                    .await;
                } else if let Ok(address) = metadata::parse_ut_vtr_address(&payload) {
                    // One identity per connection. A later address is ignored;
                    // otherwise a peer could rotate the throttle key at will.
                    if peer_address.is_none() {
                        tracing::debug!(peer = %addr, %address, "Inbound peer identified");
                        peer_address = Some(address);
                        // Sign promptly: a fast download can finish before the
                        // periodic tick, and a peer that never receives a
                        // receipt can never verify what it owes us.
                        next_receipt = std::time::Instant::now();
                    }
                }
            }
            PeerMessage::Request {
                index,
                begin,
                length,
            } => {
                if !valid_block_request(&torrent.metainfo, index, begin, length) {
                    continue;
                }
                {
                    let now = std::time::Instant::now();
                    let mut served = served_requests
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    served.retain(|_, deadline| *deadline > now);
                    if served.len() > 4096 {
                        // Pathological distinct-request flood: forget the
                        // window rather than grow without bound.
                        served.clear();
                    }
                    if served.contains_key(&(index, begin, length)) {
                        continue;
                    }
                }
                if seeding_ratio_reached(&state, &torrent.session_id).await {
                    continue;
                }
                // The session may have been removed or evicted after this
                // connection was accepted. Never serve content the operator
                // can no longer see or delete.
                if state
                    .sessions
                    .read()
                    .await
                    .get_session(&torrent.session_id)
                    .is_err()
                {
                    return Ok(());
                }
                let block_result = torrent.block(index, begin, length).await;
                // A torrent removed while its piece was being read must not
                // send that piece.
                if torrent.cancel.is_cancelled() {
                    return Ok(());
                }
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
                        served_requests
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(
                                (index, begin, length),
                                std::time::Instant::now() + Duration::from_secs(10),
                            );
                        uploaded = uploaded.saturating_add(block.len() as u64);
                        record_upload(&state, &torrent.session_id, &peer_key, block.len() as u64)
                            .await;
                        state
                            .limits
                            .consume_upload(block.len() as u64, &state.cancel)
                            .await;
                    }
                }
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
    info_hash: &[u8; 20],
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
    // A zero-value receipt proves nothing and must not pin the session as
    // "outstanding incentives"; a remote peer can mint these for free.
    if receipt.uploaded_by_me == 0 && receipt.downloaded_by_me == 0 {
        return;
    }
    // A receipt is bound to one torrent's transfer. Accepting one signed for a
    // different info-hash would corrupt this account's baselines and journal
    // the state under the wrong key.
    if receipt.info_hash != *info_hash {
        return;
    }
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
    let (already, downloaded_from_peer) = {
        let guard = state.sessions.read().await;
        guard
            .get_session(session_id)
            .ok()
            .and_then(|session| session.incentive_accounts.get(peer_address))
            .map(|account| {
                (
                    account.peer_epoch == Some(receipt.epoch)
                        && receipt.settlement_id <= account.last_receipt_id,
                    account.total_bytes_downloaded,
                )
            })
            .unwrap_or((false, 0))
    };
    if already {
        return;
    }
    // This connection uploaded to the peer; the peer should not be claiming
    // larger cumulative uploads to us than we actually recorded. Reject an
    // implausible claim before it becomes a permanent dispute.
    if receipt.uploaded_by_me > downloaded_from_peer {
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
        let journal = {
            let guard = state.sessions.read().await;
            guard.journal()
        };
        if let Some(journal) = journal {
            let peer = peer_address.to_string();
            let info_hash = *info_hash;
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

/// Nanosecond timestamp, used as the stable receipt epoch for one connection.
fn now_nanos() -> u64 {
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
    async fn re_registration_replaces_a_stale_session_and_unregister_is_session_keyed() {
        let registry = SeederRegistry::new();
        let dir = tempfile::tempdir().unwrap();
        let meta = metainfo([5u8; 20], b"abcdef");
        registry
            .register("old".into(), meta.clone(), dir.path().into())
            .await;
        registry
            .register("new".into(), meta.clone(), dir.path().into())
            .await;
        let entry = registry.get(&[5u8; 20]).await.unwrap();
        assert_eq!(entry.session_id, "new");

        registry.unregister_session(&[5u8; 20], "old").await;
        assert!(
            registry.get(&[5u8; 20]).await.is_some(),
            "a stale session must not remove the newer entry"
        );
        registry.unregister_session(&[5u8; 20], "new").await;
        assert!(registry.get(&[5u8; 20]).await.is_none());
    }

    #[tokio::test]
    async fn serving_refuses_a_piece_that_does_not_match_the_metainfo() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"the real content of this torrent";
        let torrent = Arc::new(SeedTorrent::new(
            "session".into(),
            metainfo([3u8; 20], data),
            dir.path().into(),
            HashSet::new(),
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
            HashSet::new(),
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
