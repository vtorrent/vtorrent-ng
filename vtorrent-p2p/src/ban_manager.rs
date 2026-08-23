/// Peer scoring and ban manager for vTorrent.
///
/// Tracks misbehaviour scores for peers and automatically bans IPs that
/// exceed the threshold. This protects the node from:
/// - Eclipse attacks (peers that feed invalid data)
/// - Spam attacks (peers that flood the mempool or network)
/// - DoS attacks (peers that send oversized or malformed messages)
///
/// ## Scoring Model
///
/// Each peer starts with a score of 0. Misbehaviour adds points.
/// When a peer's score reaches `BAN_THRESHOLD` (100), the IP is banned
/// for `BAN_DURATION_SECS` (24 hours by default).
///
/// | Offence | Points |
/// |---|---|
/// | Invalid block header | 20 |
/// | Invalid transaction | 10 |
/// | Duplicate inv | 5 |
/// | Oversized message | 20 |
/// | Malformed message | 20 |
/// | Invalid peer address | 2 |
/// | Bloom filter too large | 10 |
/// | Stale block (>2h old) | 5 |
/// | Unknown message type | 1 |
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Default misbehaviour score threshold before banning.
pub const BAN_THRESHOLD: u32 = 100;

/// Default ban duration (24 hours).
pub const BAN_DURATION_SECS: u64 = 24 * 60 * 60;

/// A misbehaviour offence with its penalty score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Misbehaviour {
    /// Sent a block with an invalid header.
    InvalidBlockHeader,
    /// Sent a block with invalid transactions.
    InvalidBlock,
    /// Sent an invalid transaction.
    InvalidTransaction,
    /// Sent a duplicate inventory item.
    DuplicateInv,
    /// Sent an oversized message (> MAX_PAYLOAD_SIZE).
    OversizedMessage,
    /// Sent a malformed/unparseable message.
    MalformedMessage,
    /// Sent an invalid peer address (e.g., private IP as public).
    InvalidPeerAddr,
    /// Sent a bloom filter that exceeds size limits.
    BloomFilterTooLarge,
    /// Sent a stale block (timestamp > 2 hours old).
    StaleBlock,
    /// Sent an unknown message type.
    UnknownMessage,
    /// Custom penalty with explicit score.
    Custom(u32),
}

impl Misbehaviour {
    /// Returns the penalty score for this offence.
    pub fn score(self) -> u32 {
        match self {
            Misbehaviour::InvalidBlockHeader => 20,
            Misbehaviour::InvalidBlock => 20,
            Misbehaviour::InvalidTransaction => 10,
            Misbehaviour::DuplicateInv => 5,
            Misbehaviour::OversizedMessage => 20,
            Misbehaviour::MalformedMessage => 20,
            Misbehaviour::InvalidPeerAddr => 2,
            Misbehaviour::BloomFilterTooLarge => 10,
            Misbehaviour::StaleBlock => 5,
            Misbehaviour::UnknownMessage => 1,
            Misbehaviour::Custom(n) => n,
        }
    }
}

/// A ban record for an IP address.
#[derive(Debug, Clone)]
pub struct BanRecord {
    /// When the ban was applied.
    pub banned_at: Instant,
    /// How long the ban lasts.
    pub duration: Duration,
    /// Reason for the ban.
    pub reason: String,
}

impl BanRecord {
    /// Returns true if this ban has expired.
    pub fn is_expired(&self) -> bool {
        self.banned_at.elapsed() >= self.duration
    }
}

/// Per-peer misbehaviour state.
#[derive(Debug, Clone)]
struct PeerScore {
    /// Accumulated misbehaviour score.
    score: u32,
    /// Number of offences recorded.
    offence_count: u32,
    /// When the last offence was recorded.
    last_seen: Instant,
}

impl PeerScore {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            score: 0,
            offence_count: 0,
            last_seen: now,
        }
    }
}

/// The ban manager — tracks scores and bans for all peers.
#[derive(Debug)]
pub struct BanManager {
    /// Misbehaviour scores per IP.
    scores: HashMap<IpAddr, PeerScore>,
    /// Active bans per IP.
    bans: HashMap<IpAddr, BanRecord>,
    /// Score threshold before banning.
    ban_threshold: u32,
    /// How long bans last.
    ban_duration: Duration,
}

impl Default for BanManager {
    fn default() -> Self {
        Self::new(BAN_THRESHOLD, Duration::from_secs(BAN_DURATION_SECS))
    }
}

impl BanManager {
    /// Create a new BanManager with custom threshold and duration.
    pub fn new(ban_threshold: u32, ban_duration: Duration) -> Self {
        Self {
            scores: HashMap::new(),
            bans: HashMap::new(),
            ban_threshold,
            ban_duration,
        }
    }

    /// Record a misbehaviour offence for a peer IP.
    ///
    /// Returns `true` if the peer was banned as a result of this offence.
    pub fn record_misbehaviour(&mut self, ip: IpAddr, offence: Misbehaviour) -> bool {
        let penalty = offence.score();
        let new_score = {
            let entry = self.scores.entry(ip).or_insert_with(PeerScore::new);
            entry.score = entry.score.saturating_add(penalty);
            entry.offence_count += 1;
            entry.last_seen = Instant::now();
            entry.score
        };

        tracing::debug!(
            "Peer {} misbehaviour: {:?} (+{} pts, total {})",
            ip,
            offence,
            penalty,
            new_score
        );

        if new_score >= self.ban_threshold {
            self.ban_ip(
                ip,
                format!("Score {} >= threshold {}", new_score, self.ban_threshold),
            );
            true
        } else {
            false
        }
    }

    /// Manually ban an IP address with a reason.
    pub fn ban_ip(&mut self, ip: IpAddr, reason: String) {
        tracing::warn!("Banning peer {}: {}", ip, reason);
        self.bans.insert(
            ip,
            BanRecord {
                banned_at: Instant::now(),
                duration: self.ban_duration,
                reason,
            },
        );
        // Reset score after ban
        self.scores.remove(&ip);
    }

    /// Returns true if the given IP is currently banned.
    pub fn is_banned(&self, ip: IpAddr) -> bool {
        if let Some(ban) = self.bans.get(&ip) {
            !ban.is_expired()
        } else {
            false
        }
    }

    /// Returns the current misbehaviour score for an IP (0 if not tracked).
    pub fn score(&self, ip: IpAddr) -> u32 {
        self.scores.get(&ip).map(|s| s.score).unwrap_or(0)
    }

    /// Remove a ban for an IP (manual unban).
    pub fn unban(&mut self, ip: IpAddr) {
        self.bans.remove(&ip);
        tracing::info!("Manually unbanned peer {}", ip);
    }

    /// Maximum number of tracked peer scores to prevent memory exhaustion.
    const MAX_TRACKED_PEERS: usize = 50_000;

    /// Prune expired bans and decay old scores.
    ///
    /// Should be called periodically (e.g., every hour).
    pub fn prune(&mut self) {
        // Remove expired bans
        self.bans.retain(|ip, ban| {
            if ban.is_expired() {
                tracing::debug!("Ban expired for {}", ip);
                false
            } else {
                true
            }
        });

        // Decay scores: halve scores older than 1 hour
        let decay_threshold = Duration::from_secs(3600);
        for score in self.scores.values_mut() {
            if score.last_seen.elapsed() >= decay_threshold {
                score.score /= 2;
            }
        }

        // Remove peers with zero score
        self.scores.retain(|_, s| s.score > 0);

        // If still over limit, evict the least-recently-seen peers
        if self.scores.len() > Self::MAX_TRACKED_PEERS {
            let mut entries: Vec<_> = self.scores.iter().collect();
            entries.sort_by_key(|(_, s)| s.last_seen);
            let excess = entries.len() - Self::MAX_TRACKED_PEERS * 3 / 4;
            let to_remove: Vec<IpAddr> = entries
                .into_iter()
                .take(excess)
                .map(|(ip, _)| *ip)
                .collect();
            for ip in to_remove {
                self.scores.remove(&ip);
            }
        }
    }

    /// Returns the number of currently banned IPs.
    pub fn ban_count(&self) -> usize {
        self.bans.values().filter(|b| !b.is_expired()).count()
    }

    /// Returns all currently banned IPs and their ban records.
    pub fn list_bans(&self) -> Vec<(IpAddr, &BanRecord)> {
        self.bans
            .iter()
            .filter(|(_, ban)| !ban.is_expired())
            .map(|(ip, ban)| (*ip, ban))
            .collect()
    }

    /// Returns the misbehaviour score for all tracked peers.
    pub fn list_scores(&self) -> Vec<(IpAddr, u32)> {
        self.scores.iter().map(|(ip, s)| (*ip, s.score)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn ip(a: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, a))
    }

    #[test]
    fn test_no_ban_below_threshold() {
        let mut mgr = BanManager::new(100, Duration::from_secs(3600));
        let banned = mgr.record_misbehaviour(ip(1), Misbehaviour::DuplicateInv); // +5
        assert!(!banned);
        assert!(!mgr.is_banned(ip(1)));
        assert_eq!(mgr.score(ip(1)), 5);
    }

    #[test]
    fn test_ban_at_threshold() {
        let mut mgr = BanManager::new(100, Duration::from_secs(3600));
        // 5 invalid blocks = 100 points
        for _ in 0..5 {
            mgr.record_misbehaviour(ip(2), Misbehaviour::InvalidBlock);
        }
        assert!(mgr.is_banned(ip(2)));
        assert_eq!(mgr.ban_count(), 1);
    }

    #[test]
    fn test_manual_ban() {
        let mut mgr = BanManager::default();
        mgr.ban_ip(ip(3), "test ban".to_string());
        assert!(mgr.is_banned(ip(3)));
    }

    #[test]
    fn test_unban() {
        let mut mgr = BanManager::default();
        mgr.ban_ip(ip(4), "test".to_string());
        assert!(mgr.is_banned(ip(4)));
        mgr.unban(ip(4));
        assert!(!mgr.is_banned(ip(4)));
    }

    #[test]
    fn test_score_accumulates() {
        let mut mgr = BanManager::new(100, Duration::from_secs(3600));
        mgr.record_misbehaviour(ip(5), Misbehaviour::InvalidTransaction); // +10
        mgr.record_misbehaviour(ip(5), Misbehaviour::DuplicateInv); // +5
        mgr.record_misbehaviour(ip(5), Misbehaviour::UnknownMessage); // +1
        assert_eq!(mgr.score(ip(5)), 16);
    }

    #[test]
    fn test_score_resets_after_ban() {
        let mut mgr = BanManager::new(10, Duration::from_secs(3600));
        mgr.record_misbehaviour(ip(6), Misbehaviour::InvalidBlock); // +20 > 10 → banned
        assert!(mgr.is_banned(ip(6)));
        // Score should be cleared after ban
        assert_eq!(mgr.score(ip(6)), 0);
    }

    #[test]
    fn test_prune_removes_zero_scores() {
        let mut mgr = BanManager::new(100, Duration::from_secs(3600));
        mgr.record_misbehaviour(ip(7), Misbehaviour::UnknownMessage); // +1
        assert_eq!(mgr.score(ip(7)), 1);
        // Manually zero out the score to simulate decay
        mgr.scores.get_mut(&ip(7)).unwrap().score = 0;
        mgr.prune();
        assert_eq!(mgr.score(ip(7)), 0);
        assert_eq!(mgr.list_scores().len(), 0);
    }

    #[test]
    fn test_list_bans() {
        let mut mgr = BanManager::default();
        mgr.ban_ip(ip(8), "reason A".to_string());
        mgr.ban_ip(ip(9), "reason B".to_string());
        assert_eq!(mgr.list_bans().len(), 2);
    }

    #[test]
    fn test_misbehaviour_scores() {
        assert_eq!(Misbehaviour::InvalidBlockHeader.score(), 20);
        assert_eq!(Misbehaviour::InvalidTransaction.score(), 10);
        assert_eq!(Misbehaviour::DuplicateInv.score(), 5);
        assert_eq!(Misbehaviour::UnknownMessage.score(), 1);
        assert_eq!(Misbehaviour::Custom(42).score(), 42);
    }

    #[test]
    fn test_unknown_ip_not_banned() {
        let mgr = BanManager::default();
        assert!(!mgr.is_banned(ip(99)));
        assert_eq!(mgr.score(ip(99)), 0);
    }
}
