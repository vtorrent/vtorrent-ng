use crate::error::{Result, TorrentError};
use crate::incentive::{
    aggregate_summary, IncentiveSummary, PeerBandwidthAccount, MIN_PAYMENT_BYTES, MIN_PAYMENT_SATS,
};
use crate::metainfo::Metainfo;
use crate::tracker::TrackerPeer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Maximum number of incentive accounts per session to prevent memory
/// exhaustion from unbounded peer tracking.
const MAX_INCENTIVE_ACCOUNTS: usize = 10_000;
/// An idle, debt-free account with no receipt may be evicted even if its
/// current settlement window is large (for example a peer that never sends a
/// receipt, so the window is never reset by settlement).
const ACCOUNT_IDLE_EVICT_SECS: u64 = 3_600;

/// The state of a torrent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Waiting to start (queued).
    Queued,
    /// Connecting to tracker and peers.
    Connecting,
    /// Actively downloading.
    Downloading,
    /// Download complete, seeding to others.
    Seeding,
    /// Paused by the user.
    Paused,
    /// Error state.
    Error,
    /// Stopped.
    Stopped,
}

impl SessionState {
    /// Whether the session has finished and no longer consumes a download
    /// slot. `Seeding` counts as terminal here because the engine's peer tasks
    /// have all exited (see `run_engine`'s final state update) — nothing is
    /// actively running.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            SessionState::Seeding | SessionState::Error | SessionState::Stopped
        )
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionState::Queued => write!(f, "Queued"),
            SessionState::Connecting => write!(f, "Connecting"),
            SessionState::Downloading => write!(f, "Downloading"),
            SessionState::Seeding => write!(f, "Seeding"),
            SessionState::Paused => write!(f, "Paused"),
            SessionState::Error => write!(f, "Error"),
            SessionState::Stopped => write!(f, "Stopped"),
        }
    }
}

/// A single torrent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentSession {
    /// Unique session ID.
    pub id: String,
    /// The torrent metadata.
    pub metainfo: Metainfo,
    /// Current state.
    pub state: SessionState,
    /// Number of bytes downloaded.
    pub bytes_downloaded: u64,
    /// Number of bytes uploaded.
    pub bytes_uploaded: u64,
    /// Download speed in bytes/sec (rolling average).
    pub download_speed: u64,
    /// Upload speed in bytes/sec (rolling average).
    pub upload_speed: u64,
    /// Connected peers.
    pub peers: Vec<TrackerPeer>,
    /// Incentive accounts per peer address.
    pub incentive_accounts: HashMap<String, PeerBandwidthAccount>,
    /// Our VTR wallet address (for receiving incentive payments).
    pub wallet_address: String,
    /// Error message if state == Error.
    pub error: Option<String>,
    /// Unix timestamp when the session was created.
    pub created_at: u64,
    /// Unix timestamp of last activity.
    pub last_active: u64,
}

impl TorrentSession {
    /// Create a new torrent session.
    pub fn new(metainfo: Metainfo, wallet_address: String) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        TorrentSession {
            id: Uuid::new_v4().to_string(),
            metainfo,
            state: SessionState::Queued,
            bytes_downloaded: 0,
            bytes_uploaded: 0,
            download_speed: 0,
            upload_speed: 0,
            peers: Vec::new(),
            incentive_accounts: HashMap::new(),
            wallet_address,
            error: None,
            created_at: now,
            last_active: now,
        }
    }

    /// Calculate download progress as a percentage (0.0 – 100.0).
    pub fn progress(&self) -> f64 {
        if self.metainfo.total_size == 0 {
            return 100.0;
        }
        (self.bytes_downloaded as f64 / self.metainfo.total_size as f64) * 100.0
    }

    /// Check if the download is complete.
    pub fn is_complete(&self) -> bool {
        self.bytes_downloaded >= self.metainfo.total_size
    }

    /// Get the incentive summary for this session.
    pub fn incentive_summary(&self) -> IncentiveSummary {
        let accounts: Vec<_> = self.incentive_accounts.values().cloned().collect();
        aggregate_summary(&accounts)
    }

    /// Record bytes downloaded from a peer.
    pub fn record_download(&mut self, peer_address: &str, bytes: u64) {
        self.bytes_downloaded = self.bytes_downloaded.saturating_add(bytes);
        self.evict_stale_accounts();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Some(account) = self.account_for(peer_address) {
            account.record_download(bytes);
            account.touch(now);
        }
    }

    /// Record bytes uploaded to a peer.
    pub fn record_upload(&mut self, peer_address: &str, bytes: u64) {
        self.bytes_uploaded = self.bytes_uploaded.saturating_add(bytes);
        self.evict_stale_accounts();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Some(account) = self.account_for(peer_address) {
            account.record_upload(bytes);
            account.touch(now);
        }
    }

    /// Get or create an incentive account while keeping the map bounded.
    ///
    /// A peer id is attacker-chosen until it presents a VTR address, so an
    /// unbounded map is a trivial memory-exhaustion vector. Unpayable accounts
    /// are evicted first, then accounts with negligible traffic that hold no
    /// receipt or debt. Without the second tier, an Internet peer could fill
    /// the map with valid addresses and one-byte transfers and permanently
    /// disable accounting for the torrent.
    fn account_for(&mut self, peer_address: &str) -> Option<&mut PeerBandwidthAccount> {
        self.evict_stale_accounts();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if !self.incentive_accounts.contains_key(peer_address)
            && self.incentive_accounts.len() >= MAX_INCENTIVE_ACCOUNTS
        {
            let unpayable = self
                .incentive_accounts
                .iter()
                .filter(|(addr, a)| {
                    a.unpaid_satoshis == 0
                        && !a.payment_in_flight
                        && vtorrent_core::address::Address::parse(addr).is_err()
                })
                .min_by_key(|(_, a)| a.last_active)
                .map(|(addr, _)| addr.clone());
            let victim = unpayable.or_else(|| {
                self.incentive_accounts
                    .iter()
                    .filter(|(_, a)| {
                        // Dust is not a payable liability. A receipt only
                        // blocks eviction when it agrees to a positive amount;
                        // a zero or disputed receipt must not pin the map.
                        a.unpaid_satoshis < MIN_PAYMENT_SATS
                            && !a.payment_in_flight
                            && a.pending_payment.is_none()
                            && a
                                .agreed_owed()
                                .is_none_or(|agreed| agreed.satoshis == 0)
                            // Current settlement window, or an idle account
                            // whose window was never reset because no receipt
                            // arrived. Lifetime totals are deliberately not
                            // used or a popular settled peer becomes immortal.
                            && (a.bytes_uploaded.saturating_add(a.bytes_downloaded)
                                < MIN_PAYMENT_BYTES
                                || now.saturating_sub(a.last_active)
                                    >= ACCOUNT_IDLE_EVICT_SECS)
                    })
                    .min_by_key(|(_, a)| a.last_active)
                    .map(|(addr, _)| addr.clone())
            });
            match victim {
                Some(addr) => {
                    self.incentive_accounts.remove(&addr);
                }
                None => {
                    tracing::debug!(
                        peer = peer_address,
                        "Incentive account cap reached; not tracking peer"
                    );
                    return None;
                }
            }
        }
        Some(
            self.incentive_accounts
                .entry(peer_address.to_string())
                .or_insert_with(|| PeerBandwidthAccount::new(peer_address.to_string())),
        )
    }

    /// Evict accounts with zero bandwidth and least-recently-active accounts
    /// when over limit. Never evicts by settlement age alone: never-settled
    /// accounts hold pending earnings for active peers.
    fn evict_stale_accounts(&mut self) {
        if self.incentive_accounts.len() <= MAX_INCENTIVE_ACCOUNTS {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // First pass: remove accounts with no activity or liability. Dust and
        // zero/disputed receipts are not liabilities.
        self.incentive_accounts.retain(|_, a| {
            a.bytes_uploaded > 0
                || a.bytes_downloaded > 0
                || a.unpaid_satoshis >= MIN_PAYMENT_SATS
                || a.payment_in_flight
                || a.pending_payment.is_some()
                || a.agreed_owed().is_some_and(|agreed| agreed.satoshis > 0)
        });
        // If still over limit, remove the least-recently-active accounts —
        // but only those with no pending earnings: evicting an account with
        // unsettled bandwidth would silently destroy what we owe the peer.
        if self.incentive_accounts.len() > MAX_INCENTIVE_ACCOUNTS {
            let mut entries: Vec<_> = self.incentive_accounts.iter().collect();
            entries.sort_by_key(|(_, a)| a.last_active);
            let excess = entries.len() - MAX_INCENTIVE_ACCOUNTS * 3 / 4;
            let to_remove: Vec<String> = entries
                .into_iter()
                .filter(|(_, a)| {
                    a.unpaid_satoshis < MIN_PAYMENT_SATS
                        && !a.payment_in_flight
                        && a.pending_payment.is_none()
                        && a.agreed_owed().is_none_or(|agreed| agreed.satoshis == 0)
                        && (a.bytes_uploaded == 0 && a.bytes_downloaded == 0
                            || now.saturating_sub(a.last_active) >= ACCOUNT_IDLE_EVICT_SECS)
                        && a.total_earned_satoshis == a.total_paid_satoshis
                })
                .take(excess)
                .map(|(addr, _)| addr.clone())
                .collect();
            for addr in to_remove {
                self.incentive_accounts.remove(&addr);
            }
        }
    }

    /// Format total size as human-readable.
    pub fn size_display(&self) -> String {
        format_bytes(self.metainfo.total_size)
    }

    /// Whether removing this session would discard an incentive debt or a
    /// payment awaiting confirmation.
    pub fn has_outstanding_incentives(&self) -> bool {
        self.incentive_accounts
            .values()
            .any(|account| account.blocks_removal())
    }

    /// Format downloaded bytes as human-readable.
    pub fn downloaded_display(&self) -> String {
        format_bytes(self.bytes_downloaded)
    }

    /// Format upload speed as human-readable.
    pub fn upload_speed_display(&self) -> String {
        format!("{}/s", format_bytes(self.upload_speed))
    }

    /// Format download speed as human-readable.
    pub fn download_speed_display(&self) -> String {
        format!("{}/s", format_bytes(self.download_speed))
    }
}

/// Manages all active torrent sessions.
#[derive(Default)]
pub struct SessionManager {
    sessions: HashMap<String, TorrentSession>,
    journal: Option<std::sync::Arc<dyn crate::incentive::IncentiveJournal>>,
}

/// Maximum concurrent torrent sessions. Each session spawns a download task
/// and holds peer connections, so an unbounded count is a resource-exhaustion
/// vector via repeated `add_torrent` calls.
pub const MAX_TORRENT_SESSIONS: usize = 64;

/// Maximum *retained* sessions, active or finished. A finished session still
/// holds its full metainfo (up to 20 MB of piece hashes), and the engine never
/// removes sessions itself, so without this bound a client could add torrents
/// that complete and keep every metainfo resident forever.
pub const MAX_RETAINED_SESSIONS: usize = 256;

impl SessionManager {
    pub fn new() -> Self {
        SessionManager {
            sessions: HashMap::new(),
            journal: None,
        }
    }

    /// Install the durable incentive-account journal used by peer tasks.
    pub fn set_journal(&mut self, journal: std::sync::Arc<dyn crate::incentive::IncentiveJournal>) {
        self.journal = Some(journal);
    }

    pub fn journal(&self) -> Option<std::sync::Arc<dyn crate::incentive::IncentiveJournal>> {
        self.journal.clone()
    }

    /// Find a live account by torrent and peer, for journal reconciliation.
    pub fn find_account(
        &self,
        info_hash: &[u8; 20],
        peer_address: &str,
    ) -> Option<crate::incentive::PeerBandwidthAccount> {
        self.sessions
            .values()
            .find(|session| session.metainfo.info_hash == *info_hash)
            .and_then(|session| session.incentive_accounts.get(peer_address))
            .cloned()
    }

    /// Number of active sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether there are no active sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Add a new session and return its ID.
    ///
    /// Returns `None` when the *active* session cap is reached. Completed
    /// sessions (`Seeding`/`Error`/`Stopped`) do not count, and the oldest
    /// completed one is evicted to make room — otherwise 64 `add_torrent`
    /// calls would permanently lock out new sessions, since the engine never
    /// removes them itself.
    pub fn add_session(&mut self, session: TorrentSession) -> Option<String> {
        let active = self
            .sessions
            .values()
            .filter(|s| !s.state.is_terminal())
            .count();
        if active >= MAX_TORRENT_SESSIONS {
            // Try to evict the oldest completed session to make room.
            let oldest_terminal = self
                .sessions
                .iter()
                .filter(|(_, s)| s.state.is_terminal() && !s.has_outstanding_incentives())
                .min_by_key(|(_, s)| s.last_active)
                .map(|(id, _)| id.clone())?;
            self.sessions.remove(&oldest_terminal);
        }
        // Bound the retained set even when most sessions are finished: a
        // finished session keeps its metainfo resident.
        while self.sessions.len() >= MAX_RETAINED_SESSIONS {
            let oldest_terminal = self
                .sessions
                .iter()
                .filter(|(_, s)| s.state.is_terminal() && !s.has_outstanding_incentives())
                .min_by_key(|(_, s)| s.last_active)
                .map(|(id, _)| id.clone())?;
            self.sessions.remove(&oldest_terminal);
        }
        let id = session.id.clone();
        self.sessions.insert(id.clone(), session);
        Some(id)
    }

    /// Get a session by ID.
    pub fn get_session(&self, id: &str) -> Result<&TorrentSession> {
        self.sessions
            .get(id)
            .ok_or_else(|| TorrentError::SessionNotFound(id.to_string()))
    }

    /// Get a mutable session by ID.
    pub fn get_session_mut(&mut self, id: &str) -> Result<&mut TorrentSession> {
        self.sessions
            .get_mut(id)
            .ok_or_else(|| TorrentError::SessionNotFound(id.to_string()))
    }

    /// Remove a session.
    pub fn remove_session(&mut self, id: &str) -> Option<TorrentSession> {
        self.sessions.remove(id)
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> Vec<&TorrentSession> {
        let mut sessions: Vec<_> = self.sessions.values().collect();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.created_at));
        sessions
    }

    /// Iterate over all sessions mutably.
    pub fn sessions_mut(&mut self) -> impl Iterator<Item = &mut TorrentSession> {
        self.sessions.values_mut()
    }

    /// Count sessions by state.
    pub fn count_by_state(&self, state: SessionState) -> usize {
        self.sessions.values().filter(|s| s.state == state).count()
    }

    /// Total VTR earned across all sessions (in satoshis).
    pub fn total_earned_satoshis(&self) -> u64 {
        self.sessions
            .values()
            .map(|s| s.incentive_summary().total_earned_satoshis)
            .sum()
    }
}

fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    const KB: u64 = 1024;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metainfo::TorrentFile;

    fn make_metainfo() -> Metainfo {
        Metainfo {
            info_hash: [0xAA; 20],
            name: "test.iso".into(),
            total_size: 1024 * 1024 * 1024, // 1 GB
            piece_length: 262144,
            piece_count: 4096,
            pieces: Vec::new(),
            announce: Some("http://tracker.example.com/announce".into()),
            announce_list: vec![],
            files: vec![TorrentFile {
                path: vec!["test.iso".into()],
                length: 1024 * 1024 * 1024,
                md5sum: None,
            }],
            created_at: None,
            comment: None,
            is_private: false,
        }
    }

    #[test]
    fn test_session_progress() {
        let mut session =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        assert_eq!(session.progress(), 0.0);
        session.bytes_downloaded = 512 * 1024 * 1024; // 512 MB
        assert!((session.progress() - 50.0).abs() < 0.001);
        session.bytes_downloaded = 1024 * 1024 * 1024; // 1 GB
        assert!(session.is_complete());
    }

    #[test]
    fn test_session_manager_add_get() {
        let mut manager = SessionManager::new();
        let session =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        let id = manager
            .add_session(session)
            .expect("session cap not reached");
        assert!(manager.get_session(&id).is_ok());
        assert!(manager.get_session("nonexistent").is_err());
    }

    #[test]
    fn test_session_manager_enforces_cap() {
        let mut manager = SessionManager::new();
        for _ in 0..MAX_TORRENT_SESSIONS {
            let session =
                TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
            assert!(manager.add_session(session).is_some());
        }
        // The cap must reject further sessions rather than growing unbounded.
        let extra =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        assert!(manager.add_session(extra).is_none());
        assert_eq!(manager.len(), MAX_TORRENT_SESSIONS);
    }

    #[test]
    fn retained_sessions_are_bounded_even_when_finished() {
        let mut manager = SessionManager::new();
        for index in 0..MAX_RETAINED_SESSIONS {
            let mut session =
                TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
            session.state = SessionState::Seeding;
            session.last_active = index as u64;
            assert!(manager.add_session(session).is_some());
        }
        assert_eq!(manager.len(), MAX_RETAINED_SESSIONS);
        let mut extra =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        extra.state = SessionState::Seeding;
        extra.last_active = u64::MAX;
        assert!(manager.add_session(extra).is_some());
        assert_eq!(
            manager.len(),
            MAX_RETAINED_SESSIONS,
            "retained sessions must stay bounded"
        );
    }

    #[test]
    fn test_completed_sessions_do_not_block_new_ones() {
        // The engine never removes sessions, so a hard count cap would lock
        // out new adds after 64 completions. Terminal sessions must not count
        // and must be evictable.
        let mut manager = SessionManager::new();
        for _ in 0..MAX_TORRENT_SESSIONS {
            let mut s =
                TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
            s.state = SessionState::Seeding; // terminal
            assert!(manager.add_session(s).is_some());
        }
        // A new session must still be accepted, evicting a completed one.
        let extra =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        assert!(
            manager.add_session(extra).is_some(),
            "a completed session must be evicted to make room"
        );
    }

    #[test]
    fn test_active_sessions_still_enforce_the_cap() {
        let mut manager = SessionManager::new();
        for _ in 0..MAX_TORRENT_SESSIONS {
            let mut s =
                TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
            s.state = SessionState::Downloading; // active
            assert!(manager.add_session(s).is_some());
        }
        let extra =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        assert!(
            manager.add_session(extra).is_none(),
            "active sessions must still hit the cap"
        );
    }

    #[test]
    fn test_session_manager_count_by_state() {
        let mut manager = SessionManager::new();
        let mut s1 = TorrentSession::new(make_metainfo(), "addr1".into());
        let mut s2 = TorrentSession::new(make_metainfo(), "addr2".into());
        s1.state = SessionState::Downloading;
        s2.state = SessionState::Seeding;
        manager.add_session(s1);
        manager.add_session(s2);
        assert_eq!(manager.count_by_state(SessionState::Downloading), 1);
        assert_eq!(manager.count_by_state(SessionState::Seeding), 1);
        assert_eq!(manager.count_by_state(SessionState::Paused), 0);
    }

    #[test]
    fn test_record_upload_download() {
        let mut session =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        session.record_download("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6", 100 * 1024 * 1024);
        session.record_upload("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6", 50 * 1024 * 1024);
        assert_eq!(session.bytes_downloaded, 100 * 1024 * 1024);
        assert_eq!(session.bytes_uploaded, 50 * 1024 * 1024);
        assert!(session
            .incentive_accounts
            .contains_key("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6"));
    }

    #[test]
    fn a_settled_account_with_large_lifetime_history_is_evictable() {
        let mut session =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        let address = |index: u64| {
            let mut hash160 = [0u8; 20];
            hash160[..8].copy_from_slice(&index.to_be_bytes());
            vtorrent_core::address::Address::from_hash160(&hash160, 70)
                .expect("valid address")
                .to_string()
        };
        // A long-lived peer with >10 MB of lifetime traffic, then a settlement
        // resets the current window.
        let settled = address(0);
        session.record_upload(&settled, MIN_PAYMENT_BYTES + 1);
        {
            let account = session.incentive_accounts.get_mut(&settled).unwrap();
            assert!(account.total_bytes_uploaded > MIN_PAYMENT_BYTES);
            account.settle_agreed(1, 0, 0);
            // Make it the unambiguous LRU victim (all test accounts share a
            // wall-clock second, so last_active would otherwise tie).
            account.last_active = 1;
            assert_eq!(account.bytes_uploaded, 0);
        }
        for index in 1..MAX_INCENTIVE_ACCOUNTS as u64 {
            session.record_upload(&address(index), 1);
        }
        let newcomer = address(MAX_INCENTIVE_ACCOUNTS as u64 + 1);
        session.record_upload(&newcomer, 1);
        assert!(
            session.incentive_accounts.contains_key(&newcomer),
            "lifetime history must not make an old account immortal"
        );
        assert!(
            !session.incentive_accounts.contains_key(&settled),
            "the settled, idle account should be the eviction victim"
        );
    }

    #[test]
    fn dust_debt_and_zero_value_receipts_do_not_block_removal() {
        let mut session =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        let peer = "VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6".to_string();
        session.record_download(&peer, 1);
        let wallet_address = session.wallet_address.clone();
        {
            let account = session.incentive_accounts.get_mut(&peer).unwrap();
            account.unpaid_satoshis = 100; // below the payment minimum
        }
        assert!(!session.has_outstanding_incentives());
        {
            let account = session.incentive_accounts.get_mut(&peer).unwrap();
            account.unpaid_satoshis = 0;
            account.record_receipt(vtorrent_core::receipt::BandwidthReceipt {
                info_hash: [0u8; 20],
                window_start: 0,
                window_end: 1,
                uploaded_by_me: 0,
                downloaded_by_me: 0,
                peer: wallet_address,
                signer: peer.clone(),
                settlement_id: 1,
                epoch: 1,
            });
        }
        assert!(
            !session.has_outstanding_incentives(),
            "a zero-value receipt must not pin the session"
        );
    }

    #[test]
    fn valid_accounts_with_negligible_traffic_are_evictable_at_the_cap() {
        let mut session =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        let address = |index: u64| {
            let mut hash160 = [0u8; 20];
            hash160[..8].copy_from_slice(&index.to_be_bytes());
            vtorrent_core::address::Address::from_hash160(&hash160, 70)
                .expect("valid address")
                .to_string()
        };
        for index in 0..MAX_INCENTIVE_ACCOUNTS as u64 {
            session.record_upload(&address(index), 1);
        }
        assert_eq!(
            session.incentive_accounts.len(),
            MAX_INCENTIVE_ACCOUNTS,
            "precondition: the map is exactly at the cap"
        );

        let newcomer = address(MAX_INCENTIVE_ACCOUNTS as u64 + 1);
        session.record_upload(&newcomer, 1);
        assert!(
            session.incentive_accounts.contains_key(&newcomer),
            "a new valid peer must still get an account at the cap"
        );
        assert_eq!(
            session.incentive_accounts.len(),
            MAX_INCENTIVE_ACCOUNTS,
            "eviction must keep the map bounded"
        );
    }

    #[test]
    fn un_receipted_pending_debt_blocks_removal() {
        let mut session =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        let peer = "VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6".to_string();
        session.record_download(&peer, 1024 * 1024 * 1024);
        assert!(
            session.has_outstanding_incentives(),
            "a positive un-receipted pending debt must block removal"
        );
    }

    #[test]
    fn positive_dust_receipt_does_not_block_removal() {
        let mut session =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        let peer = "VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6".to_string();
        session.record_download(&peer, 1_000);
        let wallet_address = session.wallet_address.clone();
        session
            .incentive_accounts
            .get_mut(&peer)
            .unwrap()
            .record_receipt(vtorrent_core::receipt::BandwidthReceipt {
                info_hash: [0u8; 20],
                window_start: 0,
                window_end: 1,
                uploaded_by_me: 1_000,
                downloaded_by_me: 0,
                peer: wallet_address,
                signer: peer.clone(),
                settlement_id: 1,
                epoch: 1,
            });
        assert!(
            !session.has_outstanding_incentives(),
            "a positive sub-minimum receipt must not pin the session"
        );
    }

    #[test]
    fn add_session_does_not_evict_pending_unreceipted_debt() {
        let mut manager = SessionManager::new();
        for index in 0..(MAX_RETAINED_SESSIONS - 1) {
            let mut session =
                TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
            session.state = SessionState::Seeding;
            session.last_active = 100 + index as u64;
            manager.add_session(session);
        }
        let mut pending =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        pending.state = SessionState::Seeding;
        pending.last_active = 0;
        pending.record_download("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6", 1024 * 1024 * 1024);
        let pending_id = pending.id.clone();
        manager.add_session(pending);
        let mut newcomer =
            TorrentSession::new(make_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        newcomer.state = SessionState::Seeding;
        newcomer.last_active = 1_000;
        manager.add_session(newcomer);
        assert!(
            manager.sessions.contains_key(&pending_id),
            "a pending un-receipted debt must not be evicted at the retained-session cap"
        );
    }
}
