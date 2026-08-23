use crate::error::{Result, TorrentError};
use crate::incentive::{aggregate_summary, IncentiveSummary, PeerBandwidthAccount};
use crate::metainfo::Metainfo;
use crate::tracker::TrackerPeer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Maximum number of incentive accounts per session to prevent memory
/// exhaustion from unbounded peer tracking.
const MAX_INCENTIVE_ACCOUNTS: usize = 10_000;

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
        let account = self
            .incentive_accounts
            .entry(peer_address.to_string())
            .or_insert_with(|| PeerBandwidthAccount::new(peer_address.to_string()));
        account.record_download(bytes);
        account.touch(now);
    }

    /// Record bytes uploaded to a peer.
    pub fn record_upload(&mut self, peer_address: &str, bytes: u64) {
        self.bytes_uploaded = self.bytes_uploaded.saturating_add(bytes);
        self.evict_stale_accounts();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let account = self
            .incentive_accounts
            .entry(peer_address.to_string())
            .or_insert_with(|| PeerBandwidthAccount::new(peer_address.to_string()));
        account.record_upload(bytes);
        account.touch(now);
    }

    /// Evict accounts with zero bandwidth and least-recently-active accounts
    /// when over limit. Never evicts by settlement age alone: never-settled
    /// accounts hold pending earnings for active peers.
    fn evict_stale_accounts(&mut self) {
        if self.incentive_accounts.len() <= MAX_INCENTIVE_ACCOUNTS {
            return;
        }
        // First pass: remove accounts with no activity
        self.incentive_accounts
            .retain(|_, a| a.bytes_uploaded > 0 || a.bytes_downloaded > 0);
        // If still over limit, remove the least-recently-active accounts.
        if self.incentive_accounts.len() > MAX_INCENTIVE_ACCOUNTS {
            let mut entries: Vec<_> = self.incentive_accounts.iter().collect();
            entries.sort_by_key(|(_, a)| a.last_active);
            let excess = entries.len() - MAX_INCENTIVE_ACCOUNTS * 3 / 4;
            let to_remove: Vec<String> = entries
                .into_iter()
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
#[derive(Debug, Default)]
pub struct SessionManager {
    sessions: HashMap<String, TorrentSession>,
}

impl SessionManager {
    pub fn new() -> Self {
        SessionManager {
            sessions: HashMap::new(),
        }
    }

    /// Add a new session and return its ID.
    pub fn add_session(&mut self, session: TorrentSession) -> String {
        let id = session.id.clone();
        self.sessions.insert(id.clone(), session);
        id
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
        let id = manager.add_session(session);
        assert!(manager.get_session(&id).is_ok());
        assert!(manager.get_session("nonexistent").is_err());
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
}
