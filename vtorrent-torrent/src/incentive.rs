use serde::{Deserialize, Serialize};

/// VTR incentive constants.
///
/// The incentive model rewards seeders and charges leechers a small VTR fee
/// per MB downloaded. This creates a sustainable economy where:
/// - Seeders earn VTR for sharing bandwidth
/// - Leechers pay VTR for faster downloads from incentivized peers
/// - The network self-regulates: more VTR reward = more seeders = faster downloads
pub const VTR_PER_GB_SEEDED: f64 = 1.0; // 1 VTR per GB uploaded to peers
pub const VTR_PER_GB_DOWNLOADED: f64 = 0.5; // 0.5 VTR per GB downloaded from incentivized peers
pub const MIN_PAYMENT_BYTES: u64 = 10 * 1024 * 1024; // Minimum 10 MB before payment is triggered
pub const PAYMENT_INTERVAL_SECS: u64 = 300; // Payment settled every 5 minutes
pub const COIN: u64 = 1_000_000; // 1 VTR = 1,000,000 satoshis

/// Tracks bandwidth exchanged with a single peer for incentive accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerBandwidthAccount {
    /// The peer's VTR address (for payment).
    pub peer_address: String,
    /// Bytes we have uploaded to this peer since last settlement.
    pub bytes_uploaded: u64,
    /// Bytes we have downloaded from this peer since last settlement.
    pub bytes_downloaded: u64,
    /// Total VTR earned from this peer (in satoshis).
    pub total_earned_satoshis: u64,
    /// Total VTR paid to this peer (in satoshis).
    pub total_paid_satoshis: u64,
    /// Unix timestamp of last settlement.
    pub last_settlement: u64,
    /// Whether this peer is participating in the incentive scheme.
    pub incentive_enabled: bool,
}

impl PeerBandwidthAccount {
    pub fn new(peer_address: String) -> Self {
        PeerBandwidthAccount {
            peer_address,
            bytes_uploaded: 0,
            bytes_downloaded: 0,
            total_earned_satoshis: 0,
            total_paid_satoshis: 0,
            last_settlement: 0,
            incentive_enabled: true,
        }
    }

    /// Record bytes uploaded to this peer.
    pub fn record_upload(&mut self, bytes: u64) {
        self.bytes_uploaded = self.bytes_uploaded.saturating_add(bytes);
    }

    /// Record bytes downloaded from this peer.
    pub fn record_download(&mut self, bytes: u64) {
        self.bytes_downloaded = self.bytes_downloaded.saturating_add(bytes);
    }

    /// Calculate how much VTR we should receive from this peer for our uploads.
    /// Returns the amount in satoshis.
    pub fn calculate_earned(&self) -> u64 {
        let gb_uploaded = self.bytes_uploaded as f64 / (1024.0 * 1024.0 * 1024.0);
        let vtr = gb_uploaded * VTR_PER_GB_SEEDED;
        (vtr * COIN as f64) as u64
    }

    /// Calculate how much VTR we owe this peer for their uploads to us.
    /// Returns the amount in satoshis.
    pub fn calculate_owed(&self) -> u64 {
        if !self.incentive_enabled {
            return 0;
        }
        let gb_downloaded = self.bytes_downloaded as f64 / (1024.0 * 1024.0 * 1024.0);
        let vtr = gb_downloaded * VTR_PER_GB_DOWNLOADED;
        (vtr * COIN as f64) as u64
    }

    /// Check if a settlement is due based on bytes transferred or time elapsed.
    pub fn needs_settlement(&self, current_timestamp: u64) -> bool {
        let time_elapsed = current_timestamp.saturating_sub(self.last_settlement);
        let bytes_total = self.bytes_uploaded.saturating_add(self.bytes_downloaded);
        time_elapsed >= PAYMENT_INTERVAL_SECS || bytes_total >= MIN_PAYMENT_BYTES
    }

    /// Settle the account: reset counters and record totals.
    /// Returns (earned_satoshis, owed_satoshis) for this settlement period.
    pub fn settle(&mut self, current_timestamp: u64) -> (u64, u64) {
        let earned = self.calculate_earned();
        let owed = self.calculate_owed();
        self.total_earned_satoshis = self.total_earned_satoshis.saturating_add(earned);
        self.total_paid_satoshis = self.total_paid_satoshis.saturating_add(owed);
        self.bytes_uploaded = 0;
        self.bytes_downloaded = 0;
        self.last_settlement = current_timestamp;
        (earned, owed)
    }
}

/// Session-level incentive summary for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncentiveSummary {
    /// Total VTR earned this session (in satoshis).
    pub total_earned_satoshis: u64,
    /// Total VTR paid this session (in satoshis).
    pub total_paid_satoshis: u64,
    /// Total bytes uploaded this session.
    pub total_bytes_uploaded: u64,
    /// Total bytes downloaded this session.
    pub total_bytes_downloaded: u64,
    /// Number of peers with incentive enabled.
    pub incentive_peer_count: usize,
}

impl IncentiveSummary {
    /// Format earned VTR as a human-readable string.
    pub fn earned_vtr_display(&self) -> String {
        format!("{:.6} VTR", self.total_earned_satoshis as f64 / COIN as f64)
    }

    /// Format paid VTR as a human-readable string.
    pub fn paid_vtr_display(&self) -> String {
        format!("{:.6} VTR", self.total_paid_satoshis as f64 / COIN as f64)
    }

    /// Format uploaded bytes as human-readable.
    pub fn uploaded_display(&self) -> String {
        format_bytes(self.total_bytes_uploaded)
    }

    /// Format downloaded bytes as human-readable.
    pub fn downloaded_display(&self) -> String {
        format_bytes(self.total_bytes_downloaded)
    }
}

/// Aggregate incentive accounts into a session summary.
pub fn aggregate_summary(accounts: &[PeerBandwidthAccount]) -> IncentiveSummary {
    let mut summary = IncentiveSummary {
        total_earned_satoshis: 0,
        total_paid_satoshis: 0,
        total_bytes_uploaded: 0,
        total_bytes_downloaded: 0,
        incentive_peer_count: 0,
    };
    for account in accounts {
        summary.total_earned_satoshis = summary
            .total_earned_satoshis
            .saturating_add(account.total_earned_satoshis);
        summary.total_paid_satoshis = summary
            .total_paid_satoshis
            .saturating_add(account.total_paid_satoshis);
        summary.total_bytes_uploaded = summary
            .total_bytes_uploaded
            .saturating_add(account.bytes_uploaded);
        summary.total_bytes_downloaded = summary
            .total_bytes_downloaded
            .saturating_add(account.bytes_downloaded);
        if account.incentive_enabled {
            summary.incentive_peer_count += 1;
        }
    }
    summary
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

    #[test]
    fn test_earn_calculation() {
        let mut account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        // Simulate uploading 1 GB
        account.record_upload(1024 * 1024 * 1024);
        let earned = account.calculate_earned();
        // Should earn 1 VTR = 1,000,000 satoshis
        assert_eq!(earned, COIN);
    }

    #[test]
    fn test_owed_calculation() {
        let mut account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        // Simulate downloading 2 GB
        account.record_download(2 * 1024 * 1024 * 1024);
        let owed = account.calculate_owed();
        // Should owe 1 VTR (2 GB × 0.5 VTR/GB)
        assert_eq!(owed, COIN);
    }

    #[test]
    fn test_settlement_resets_counters() {
        let mut account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        account.record_upload(1024 * 1024 * 1024);
        account.record_download(1024 * 1024 * 1024);
        let (earned, owed) = account.settle(1_700_000_000);
        assert!(earned > 0);
        assert!(owed > 0);
        assert_eq!(account.bytes_uploaded, 0);
        assert_eq!(account.bytes_downloaded, 0);
        assert_eq!(account.total_earned_satoshis, earned);
        assert_eq!(account.total_paid_satoshis, owed);
    }

    #[test]
    fn test_needs_settlement_by_time() {
        let account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        // last_settlement = 0, current = PAYMENT_INTERVAL_SECS
        assert!(account.needs_settlement(PAYMENT_INTERVAL_SECS));
    }

    #[test]
    fn test_needs_settlement_by_bytes() {
        let mut account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        account.record_upload(MIN_PAYMENT_BYTES);
        // Even with timestamp = 1 (no time elapsed), should settle due to bytes
        assert!(account.needs_settlement(1));
    }

    #[test]
    fn test_incentive_disabled_owes_nothing() {
        let mut account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        account.incentive_enabled = false;
        account.record_download(10 * 1024 * 1024 * 1024); // 10 GB
        assert_eq!(account.calculate_owed(), 0);
    }

    #[test]
    fn test_summary_display() {
        let summary = IncentiveSummary {
            total_earned_satoshis: 1_500_000,
            total_paid_satoshis: 500_000,
            total_bytes_uploaded: 2 * 1024 * 1024 * 1024,
            total_bytes_downloaded: 512 * 1024 * 1024,
            incentive_peer_count: 5,
        };
        assert_eq!(summary.earned_vtr_display(), "1.500000 VTR");
        assert_eq!(summary.paid_vtr_display(), "0.500000 VTR");
        assert_eq!(summary.uploaded_display(), "2.00 GB");
        assert_eq!(summary.downloaded_display(), "512.00 MB");
    }
}
