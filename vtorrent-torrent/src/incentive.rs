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
#[allow(dead_code)]
pub const MIN_PAYMENT_BYTES: u64 = 10 * 1024 * 1024; // Minimum 10 MB before payment is triggered
pub const PAYMENT_INTERVAL_SECS: u64 = 300; // Payment settled every 5 minutes
pub const COIN: u64 = 100_000_000; // 1 VTR = 100,000,000 satoshis

/// Monotonic-enough per-account epoch. A restart creates a new epoch so a
/// peer's cumulative receipt cannot be compared against a different session.
fn account_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
        .max(1)
}

/// Tracks bandwidth exchanged with a single peer for incentive accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerBandwidthAccount {
    /// The peer's VTR address (for payment).
    pub peer_address: String,
    /// Bytes we have uploaded to this peer since last settlement.
    pub bytes_uploaded: u64,
    /// Bytes we have downloaded from this peer since last settlement.
    pub bytes_downloaded: u64,
    /// Cumulative bytes uploaded to this peer since the account was created.
    #[serde(default)]
    pub total_bytes_uploaded: u64,
    /// Cumulative bytes downloaded from this peer since the account was created.
    #[serde(default)]
    pub total_bytes_downloaded: u64,
    /// Our cumulative downloaded bytes at the last settlement.
    #[serde(default)]
    pub settled_our_downloaded: u64,
    /// The peer's cumulative uploaded bytes at the last settlement.
    #[serde(default)]
    pub settled_peer_uploaded: u64,
    /// Total VTR earned from this peer (in satoshis).
    pub total_earned_satoshis: u64,
    /// Total VTR paid to this peer (in satoshis).
    pub total_paid_satoshis: u64,
    /// Unix timestamp of last settlement.
    pub last_settlement: u64,
    /// Unix timestamp of last bandwidth activity (upload or download).
    /// Used for eviction: least-recently-active accounts are evicted first,
    /// never active-settled ones (which would delete pending earnings).
    pub last_active: u64,
    /// Whether this peer is participating in the incentive scheme.
    pub incentive_enabled: bool,
    /// Latest verified receipt from this peer for the current settlement.
    #[serde(default)]
    pub peer_receipt: Option<vtorrent_core::receipt::BandwidthReceipt>,
    /// Highest settlement id seen from this peer, for replay rejection.
    #[serde(default)]
    pub last_receipt_id: u64,
    /// This side's cumulative-counter epoch. A restart gets a new value.
    #[serde(default)]
    pub receipt_epoch: u64,
    /// The peer's epoch last seen in a receipt. Kept separate from
    /// [`Self::receipt_epoch`] so adopting a peer's epoch never changes the
    /// epoch this side signs. `None` means no peer receipt has been seen; it
    /// is distinct from a legitimate epoch 0.
    #[serde(default)]
    pub peer_epoch: Option<u64>,
    /// Agreed satoshis settled but not yet confirmed paid on-chain.
    #[serde(default)]
    pub unpaid_satoshis: u64,
    /// A payment for `unpaid_satoshis` is currently in flight.
    #[serde(default)]
    pub payment_in_flight: bool,
    /// A payment transaction that was broadcast but is not yet confirmed.
    ///
    /// The obligation stays in `unpaid_satoshis` until the transaction is
    /// observed in a block. If it disappears from both the chain and the
    /// mempool, the account becomes payable again.
    #[serde(default)]
    pub pending_payment: Option<PendingPayment>,
}

/// A broadcast incentive payment awaiting on-chain confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPayment {
    pub txid: [u8; 32],
    pub satoshis: u64,
}

impl PeerBandwidthAccount {
    pub fn new(peer_address: String) -> Self {
        PeerBandwidthAccount {
            peer_address,
            bytes_uploaded: 0,
            bytes_downloaded: 0,
            total_bytes_uploaded: 0,
            total_bytes_downloaded: 0,
            settled_our_downloaded: 0,
            settled_peer_uploaded: 0,
            total_earned_satoshis: 0,
            total_paid_satoshis: 0,
            last_settlement: 0,
            last_active: 0,
            incentive_enabled: true,
            peer_receipt: None,
            last_receipt_id: 0,
            receipt_epoch: account_epoch(),
            peer_epoch: None,
            unpaid_satoshis: 0,
            payment_in_flight: false,
            pending_payment: None,
        }
    }

    /// Record bytes uploaded to this peer.
    pub fn record_upload(&mut self, bytes: u64) {
        self.bytes_uploaded = self.bytes_uploaded.saturating_add(bytes);
        self.total_bytes_uploaded = self.total_bytes_uploaded.saturating_add(bytes);
    }

    /// Record bytes downloaded from this peer.
    pub fn record_download(&mut self, bytes: u64) {
        self.bytes_downloaded = self.bytes_downloaded.saturating_add(bytes);
        self.total_bytes_downloaded = self.total_bytes_downloaded.saturating_add(bytes);
    }

    /// Update the last-activity timestamp.
    pub fn touch(&mut self, timestamp: u64) {
        self.last_active = timestamp;
    }

    /// Calculate how much VTR we should receive from this peer for our uploads.
    /// Returns the amount in satoshis.
    ///
    /// Uses integer arithmetic (u128 intermediate) to avoid floating-point
    /// precision loss on large byte counts.
    pub fn calculate_earned(&self) -> u64 {
        if !self.incentive_enabled {
            return 0;
        }
        const GB: u128 = 1024 * 1024 * 1024;
        // 1 VTR per GB = COIN satoshis per GB.
        ((self.bytes_uploaded as u128 * COIN as u128) / GB) as u64
    }

    /// Calculate how much VTR we owe this peer for their uploads to us.
    /// Returns the amount in satoshis.
    pub fn calculate_owed(&self) -> u64 {
        if !self.incentive_enabled {
            return 0;
        }
        calculate_owed_for_bytes(self.bytes_downloaded)
    }

    /// Check if a settlement is due based on bytes transferred or time elapsed.
    pub fn needs_settlement(&self, current_timestamp: u64) -> bool {
        let time_elapsed = current_timestamp.saturating_sub(self.last_settlement);
        let bytes_total = self.bytes_uploaded.saturating_add(self.bytes_downloaded);
        time_elapsed >= PAYMENT_INTERVAL_SECS || bytes_total >= MIN_PAYMENT_BYTES
    }

    /// Settle the account: reset counters and move the agreed amount into the
    /// unpaid balance.
    ///
    /// `total_paid_satoshis` is only advanced by [`Self::mark_paid`] after the
    /// payment transaction has been broadcast, so a failed or skipped payment
    /// is retried instead of being reported as paid.
    ///
    /// Returns (earned_satoshis, owed_satoshis) for this settlement period.
    pub fn settle(&mut self, current_timestamp: u64) -> (u64, u64) {
        let owed = self.calculate_owed();
        let earned = self.settle_agreed(current_timestamp, owed, self.bytes_downloaded);
        (earned, owed)
    }

    /// Settle using an externally agreed amount and byte count.
    ///
    /// `agreed_bytes` advances the cumulative baselines by exactly the bytes
    /// that were paid for, so a peer whose receipt lags behind our counter is
    /// not paid twice and the unpaid remainder is reconsidered next time.
    ///
    /// Returns the earned amount for this period.
    pub fn settle_agreed(
        &mut self,
        current_timestamp: u64,
        agreed_satoshis: u64,
        agreed_bytes: u64,
    ) -> u64 {
        let earned = self.calculate_earned();
        let remaining = self
            .total_bytes_downloaded
            .saturating_sub(self.settled_our_downloaded);
        let agreed_bytes = agreed_bytes.min(remaining);
        self.total_earned_satoshis = self.total_earned_satoshis.saturating_add(earned);
        self.unpaid_satoshis = self.unpaid_satoshis.saturating_add(agreed_satoshis);
        self.settled_our_downloaded = self
            .settled_our_downloaded
            .saturating_add(agreed_bytes)
            .min(self.total_bytes_downloaded);
        self.settled_peer_uploaded = self.settled_peer_uploaded.saturating_add(agreed_bytes);
        self.bytes_uploaded = 0;
        self.bytes_downloaded = 0;
        self.last_settlement = current_timestamp;
        self.peer_receipt = None;
        earned
    }

    /// Confirm that a payment was broadcast. Returns the remaining unpaid amount.
    pub fn mark_paid(&mut self, satoshis: u64) -> u64 {
        let paid = satoshis.min(self.unpaid_satoshis);
        self.unpaid_satoshis = self.unpaid_satoshis.saturating_sub(paid);
        self.total_paid_satoshis = self.total_paid_satoshis.saturating_add(paid);
        self.payment_in_flight = false;
        self.pending_payment = None;
        self.unpaid_satoshis
    }

    /// Record a broadcast payment without clearing the debt.
    ///
    /// Mempool admission is not settlement: the transaction can be evicted,
    /// lost on restart, or invalidated by a reorg. The obligation is cleared
    /// only by [`Self::payment_confirmed`].
    pub fn record_payment_broadcast(&mut self, txid: [u8; 32], satoshis: u64) {
        self.payment_in_flight = false;
        self.pending_payment = Some(PendingPayment {
            txid,
            satoshis: satoshis.min(self.unpaid_satoshis),
        });
    }

    /// The pending payment was observed in a confirmed block.
    pub fn payment_confirmed(&mut self) {
        if let Some(pending) = self.pending_payment.take() {
            self.mark_paid(pending.satoshis);
        }
        self.payment_in_flight = false;
    }

    /// The pending payment vanished from both the chain and the mempool.
    pub fn payment_missing(&mut self) {
        self.pending_payment = None;
        self.payment_in_flight = false;
    }

    /// Clear the in-flight marker after a failed payment so it is retried.
    pub fn payment_failed(&mut self) {
        self.payment_in_flight = false;
    }

    /// Store a verified peer receipt. Returns false for a replayed id.
    pub fn record_receipt(&mut self, receipt: vtorrent_core::receipt::BandwidthReceipt) -> bool {
        if self
            .peer_epoch
            .is_some_and(|current| receipt.epoch < current)
        {
            return false;
        }
        if self.peer_epoch != Some(receipt.epoch) {
            // The peer's epoch changed (normally a restart), so its cumulative
            // counter restarted. Settle the previous epoch's agreement first,
            // and leave our own unsettled downloads pending. Anchor the new
            // counter so its next delta covers those downloads instead of
            // discarding them. This changes only the peer's epoch, never the
            // epoch we sign.
            if self.peer_epoch.is_some() {
                if let Some(agreement) = self.agreed_owed() {
                    self.settle_agreed(receipt.window_end, agreement.satoshis, agreement.bytes);
                }
            }
            self.peer_epoch = Some(receipt.epoch);
            // Anchor the peer's cumulative counter so its next delta covers
            // our unsettled downloads, even when the counter also includes
            // bytes from before this connection or from a previous session.
            let unsettled = self
                .total_bytes_downloaded
                .saturating_sub(self.settled_our_downloaded);
            self.settled_peer_uploaded = receipt.uploaded_by_me.saturating_sub(unsettled);
            self.last_receipt_id = receipt.settlement_id;
            self.peer_receipt = Some(receipt);
            return true;
        }
        if receipt.settlement_id <= self.last_receipt_id {
            return false;
        }
        self.last_receipt_id = receipt.settlement_id;
        self.peer_receipt = Some(receipt);
        true
    }

    /// Amount payable, or `None` without a matching receipt.
    ///
    /// Counters are cumulative, so a missed or clock-skewed window does not
    /// permanently dispute an honest claim: the next receipt covers the whole
    /// unpaid delta.
    pub fn agreed_owed(&self) -> Option<AgreedOwed> {
        let receipt = self.peer_receipt.as_ref()?;
        if receipt.uploaded_by_me < self.settled_peer_uploaded {
            return None;
        }
        let our_bytes = self
            .total_bytes_downloaded
            .saturating_sub(self.settled_our_downloaded);
        let their_bytes = receipt
            .uploaded_by_me
            .saturating_sub(self.settled_peer_uploaded);
        let mine_satoshis = calculate_owed_for_bytes(our_bytes);
        let theirs_satoshis = calculate_owed_for_bytes(their_bytes);
        match agree_on_satoshis(mine_satoshis, theirs_satoshis) {
            ReceiptAgreement::Agreed { satoshis } => {
                let bytes = if mine_satoshis <= theirs_satoshis {
                    our_bytes
                } else {
                    their_bytes
                };
                Some(AgreedOwed { satoshis, bytes })
            }
            ReceiptAgreement::Disputed { .. } => None,
        }
    }
}

/// A receipt-verified amount and the byte delta it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgreedOwed {
    pub satoshis: u64,
    pub bytes: u64,
}

/// Session-level incentive summary for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncentiveSummary {
    /// Total VTR earned this session (in satoshis).
    pub total_earned_satoshis: u64,
    /// Total VTR paid this session (in satoshis).
    pub total_paid_satoshis: u64,
    /// Estimated VTR earned since the last settlement (in satoshis).
    pub pending_earned_satoshis: u64,
    /// Estimated VTR owed since the last settlement (in satoshis).
    pub pending_owed_satoshis: u64,
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
/// Build a signed-ready bandwidth receipt for one settlement window from this
/// account's own accounting. The caller fills `signer` with its own VTR address
/// and signs with `vtorrent_core::receipt::BandwidthReceipt::sign`.
///
/// **SECURITY (replay protection):** When the receipt exchange is wired into
/// settlement, `settlement_id` must be enforced as **single-use** — i.e. the
/// receiving peer must reject (or ignore) a receipt whose `settlement_id` has
/// already been seen for this torrent. Without this, a peer can replay the
/// same receipt across multiple settlement windows to inflate its earnings.
/// Track used settlement IDs in a `HashSet<u64>` per-peer per-torrent.
pub fn build_receipt(
    account: &PeerBandwidthAccount,
    info_hash: [u8; 20],
    window_start: u64,
    window_end: u64,
    signer: &str,
    settlement_id: u64,
) -> vtorrent_core::receipt::BandwidthReceipt {
    vtorrent_core::receipt::BandwidthReceipt {
        info_hash,
        window_start,
        window_end,
        uploaded_by_me: account.total_bytes_uploaded,
        downloaded_by_me: account.total_bytes_downloaded,
        peer: account.peer_address.clone(),
        signer: signer.to_string(),
        settlement_id,
        epoch: account.receipt_epoch,
    }
}

/// Default tolerance for bilateral receipt agreement, in parts per 10,000
/// (500 = 5%). Lost packets and retransmits mean the two sides rarely agree
/// exactly; a payment is made on the **agreed** (minimum) figure when the two
/// receipts are within this tolerance, and withheld otherwise.
pub const RECEIPT_TOLERANCE_BPS: u64 = 500;

/// Outcome of comparing two signed receipts for the same window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptAgreement {
    /// Both sides agree within tolerance; pay this many satoshis (the minimum
    /// of the two claims, so neither side can inflate the payment).
    Agreed { satoshis: u64 },
    /// The claims differ by more than the tolerance — withhold payment.
    Disputed { mine: u64, theirs: u64 },
}

/// Satoshis owed for downloading `bytes` from a peer (0.5 VTR/GB).
pub fn calculate_owed_for_bytes(bytes: u64) -> u64 {
    const GB: u128 = 1024 * 1024 * 1024;
    ((bytes as u128 * (COIN as u128 / 2)) / GB) as u64
}

/// Satoshis earned for uploading `bytes` to a peer (1 VTR/GB).
pub fn calculate_earned_for_bytes(bytes: u64) -> u64 {
    const GB: u128 = 1024 * 1024 * 1024;
    ((bytes as u128 * COIN as u128) / GB) as u64
}

fn agree_on_satoshis(mine_satoshis: u64, theirs: u64) -> ReceiptAgreement {
    let hi = mine_satoshis.max(theirs);
    let lo = mine_satoshis.min(theirs);
    let diff = hi - lo;
    if diff.saturating_mul(10_000) <= hi.saturating_mul(RECEIPT_TOLERANCE_BPS) {
        ReceiptAgreement::Agreed { satoshis: lo }
    } else {
        ReceiptAgreement::Disputed {
            mine: mine_satoshis,
            theirs,
        }
    }
}

/// Compare what we earned from a peer against what the peer's signed receipt
/// claims we uploaded, and decide whether to pay.
///
/// `mine_satoshis` is what our own accounting says we are owed for uploads to
/// the peer; `their_receipt_uploaded` is the bytes the peer's signed receipt says
/// it downloaded from us. Both are converted at the same 1 VTR/GB rate.
pub fn agree_on_upload(mine_satoshis: u64, their_receipt_uploaded: u64) -> ReceiptAgreement {
    agree_on_satoshis(
        mine_satoshis,
        calculate_earned_for_bytes(their_receipt_uploaded),
    )
}

/// Compare what we owe a peer for their uploads against the bytes their signed
/// receipt says they uploaded. The download price is half the seeding price.
pub fn agree_on_download(mine_satoshis: u64, their_receipt_uploaded: u64) -> ReceiptAgreement {
    agree_on_satoshis(
        mine_satoshis,
        calculate_owed_for_bytes(their_receipt_uploaded),
    )
}

pub fn aggregate_summary(accounts: &[PeerBandwidthAccount]) -> IncentiveSummary {
    let mut summary = IncentiveSummary {
        total_earned_satoshis: 0,
        total_paid_satoshis: 0,
        pending_earned_satoshis: 0,
        pending_owed_satoshis: 0,
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
        summary.pending_earned_satoshis = summary
            .pending_earned_satoshis
            .saturating_add(account.calculate_earned());
        summary.pending_owed_satoshis = summary
            .pending_owed_satoshis
            .saturating_add(account.calculate_owed());
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
        // Should earn 1 VTR = 100,000,000 satoshis
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
        assert_eq!(account.unpaid_satoshis, owed);
        assert_eq!(account.total_paid_satoshis, 0);
        assert_eq!(account.mark_paid(owed), 0);
        assert_eq!(account.total_paid_satoshis, owed);
    }

    #[test]
    fn test_needs_settlement_by_time() {
        let account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        // last_settlement = 0, current = PAYMENT_INTERVAL_SECS
        assert!(account.needs_settlement(PAYMENT_INTERVAL_SECS));
    }

    #[test]
    fn failed_payment_stays_unpaid_and_is_retried() {
        let mut account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        account.record_download(2 * 1024 * 1024 * 1024);
        account.settle_agreed(1_700_000_000, COIN, 2 * 1024 * 1024 * 1024);
        assert_eq!(account.unpaid_satoshis, COIN);
        account.payment_failed();
        assert_eq!(account.unpaid_satoshis, COIN);
        assert_eq!(account.total_paid_satoshis, 0);
        assert_eq!(account.mark_paid(COIN), 0);
        assert_eq!(account.total_paid_satoshis, COIN);
    }

    #[test]
    fn broadcast_payment_keeps_the_debt_until_confirmed() {
        let mut account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        account.record_download(2 * 1024 * 1024 * 1024);
        account.settle_agreed(1_700_000_000, COIN, 2 * 1024 * 1024 * 1024);
        assert_eq!(account.unpaid_satoshis, COIN);

        account.record_payment_broadcast([9u8; 32], COIN);
        assert_eq!(
            account.unpaid_satoshis, COIN,
            "mempool admission must not clear the debt"
        );
        assert!(account.pending_payment.is_some());

        account.payment_missing();
        assert!(account.pending_payment.is_none());
        assert_eq!(
            account.unpaid_satoshis, COIN,
            "a vanished transaction must be payable again"
        );

        account.record_payment_broadcast([9u8; 32], COIN);
        account.payment_confirmed();
        assert_eq!(account.unpaid_satoshis, 0);
        assert_eq!(account.total_paid_satoshis, COIN);
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
    fn test_incentive_disabled_earns_nothing() {
        let mut account = PeerBandwidthAccount::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        account.incentive_enabled = false;
        account.record_upload(10 * 1024 * 1024 * 1024); // 10 GB
        assert_eq!(account.calculate_earned(), 0);
    }

    #[test]
    fn test_summary_display() {
        let summary = IncentiveSummary {
            total_earned_satoshis: 150_000_000,
            total_paid_satoshis: 50_000_000,
            pending_earned_satoshis: 0,
            pending_owed_satoshis: 0,
            total_bytes_uploaded: 2 * 1024 * 1024 * 1024,
            total_bytes_downloaded: 512 * 1024 * 1024,
            incentive_peer_count: 5,
        };
        assert_eq!(summary.earned_vtr_display(), "1.500000 VTR");
        assert_eq!(summary.paid_vtr_display(), "0.500000 VTR");
        assert_eq!(summary.uploaded_display(), "2.00 GB");
        assert_eq!(summary.downloaded_display(), "512.00 MB");
    }
    #[test]
    fn test_receipt_agreement_within_tolerance_pays_minimum() {
        const GB: u64 = 1024 * 1024 * 1024;
        // We claim 1 VTR (COIN sat); peer's receipt says 0.97 VTR (3% less).
        let mine = COIN;
        let their_bytes = GB * 97 / 100;
        match agree_on_upload(mine, their_bytes) {
            ReceiptAgreement::Agreed { satoshis } => {
                assert!(satoshis <= mine, "never pay more than we claim");
                assert!(satoshis > 0);
            }
            other => panic!("expected agreement, got {other:?}"),
        }
    }

    #[test]
    fn test_receipt_agreement_beyond_tolerance_disputes() {
        const GB: u64 = 1024 * 1024 * 1024;
        // We claim 1 VTR; peer's receipt claims 2 VTR (100% more) -> dispute.
        let mine = COIN;
        let their_bytes = GB * 2;
        assert!(matches!(
            agree_on_upload(mine, their_bytes),
            ReceiptAgreement::Disputed { .. }
        ));
    }
    #[test]
    fn test_build_receipt_from_account() {
        let mut account = PeerBandwidthAccount::new("VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k".into());
        account.record_upload(3 * 1024 * 1024 * 1024);
        account.record_download(1024 * 1024 * 1024);
        let r = build_receipt(
            &account,
            [9u8; 20],
            100,
            400,
            "VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6",
            7,
        );
        assert_eq!(r.uploaded_by_me, 3 * 1024 * 1024 * 1024);
        assert_eq!(r.downloaded_by_me, 1024 * 1024 * 1024);
        assert_eq!(r.peer, account.peer_address);
        assert_eq!(r.settlement_id, 7);
        assert_eq!(r.info_hash, [9u8; 20]);
    }

    #[test]
    fn receipt_is_required_for_agreed_payment_and_cannot_be_replayed() {
        use vtorrent_core::receipt::BandwidthReceipt;

        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.record_download(1024 * 1024 * 1024);
        assert_eq!(account.agreed_owed(), None);
        // Simulate a peer epoch already established by an earlier receipt.
        account.peer_epoch = Some(account.receipt_epoch);

        let receipt = BandwidthReceipt {
            info_hash: [1u8; 20],
            window_start: 0,
            window_end: 300,
            uploaded_by_me: 1024 * 1024 * 1024,
            downloaded_by_me: 0,
            peer: "VUs".into(),
            signer: "VPeer".into(),
            settlement_id: 7,
            epoch: account.receipt_epoch,
        };
        assert!(account.record_receipt(receipt.clone()));
        assert_eq!(
            account.agreed_owed().map(|agreement| agreement.satoshis),
            Some(COIN / 2)
        );
        assert!(!account.record_receipt(receipt));
        let _ = account.settle(300);
        assert_eq!(account.agreed_owed(), None);
    }

    #[test]
    fn first_receipt_pays_for_acknowledged_downloads() {
        const GB: u64 = 1024 * 1024 * 1024;
        use vtorrent_core::receipt::BandwidthReceipt;

        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.record_download(GB);
        let mut receipt = BandwidthReceipt {
            info_hash: [1u8; 20],
            window_start: 0,
            window_end: 300,
            uploaded_by_me: GB,
            downloaded_by_me: 0,
            peer: "VUs".into(),
            signer: "VPeer".into(),
            settlement_id: 1,
            epoch: account.receipt_epoch,
        };
        assert!(account.record_receipt(receipt.clone()));
        // The first receipt already acknowledges the bytes downloaded before
        // it; the payment is capped by our own count.
        assert_eq!(account.agreed_owed().map(|a| a.satoshis), Some(COIN / 2));
        let baseline = account.agreed_owed().unwrap();
        account.settle_agreed(300, baseline.satoshis, baseline.bytes);

        account.record_download(GB);
        receipt.settlement_id = 2;
        receipt.uploaded_by_me = 2 * GB;
        assert!(account.record_receipt(receipt));
        assert_eq!(
            account.agreed_owed().map(|agreement| agreement.satoshis),
            Some(COIN / 2)
        );
    }

    #[test]
    fn first_receipt_with_prior_session_bytes_does_not_dispute() {
        const GB: u64 = 1024 * 1024 * 1024;
        use vtorrent_core::receipt::BandwidthReceipt;

        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.record_download(GB);
        let receipt = BandwidthReceipt {
            info_hash: [1u8; 20],
            window_start: 0,
            window_end: 300,
            uploaded_by_me: 10 * GB,
            downloaded_by_me: 0,
            peer: "VUs".into(),
            signer: "VPeer".into(),
            settlement_id: 1,
            epoch: account.receipt_epoch,
        };
        assert!(account.record_receipt(receipt));
        assert_eq!(
            account.agreed_owed().map(|agreement| agreement.satoshis),
            Some(COIN / 2),
            "a cumulative counter from a prior session must not block payment"
        );
    }

    #[test]
    fn receipt_epoch_change_resets_baselines_without_double_paying() {
        const GB: u64 = 1024 * 1024 * 1024;
        use vtorrent_core::receipt::BandwidthReceipt;

        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.peer_epoch = Some(account.receipt_epoch);
        account.record_download(2 * GB);
        let mut receipt = BandwidthReceipt {
            info_hash: [1u8; 20],
            window_start: 0,
            window_end: 300,
            uploaded_by_me: 2 * GB,
            downloaded_by_me: 0,
            peer: "VUs".into(),
            signer: "VPeer".into(),
            settlement_id: 1,
            epoch: account.receipt_epoch,
        };
        assert!(account.record_receipt(receipt.clone()));
        let first = account.agreed_owed().unwrap();
        assert_eq!(first.satoshis, COIN);
        account.settle_agreed(300, first.satoshis, first.bytes);

        // The peer restarted: its cumulative counter starts over.
        receipt.epoch = account.receipt_epoch.wrapping_add(1);
        receipt.settlement_id = 2;
        receipt.uploaded_by_me = 0;
        assert!(account.record_receipt(receipt.clone()));
        assert_eq!(
            account.agreed_owed().map(|agreement| agreement.satoshis),
            Some(0)
        );
        let baseline = account.agreed_owed().unwrap();
        account.settle_agreed(600, baseline.satoshis, baseline.bytes);

        account.record_download(2 * GB);
        receipt.settlement_id = 3;
        receipt.uploaded_by_me = 2 * GB;
        assert!(account.record_receipt(receipt));
        assert_eq!(
            account.agreed_owed().map(|agreement| agreement.satoshis),
            Some(COIN)
        );
    }

    #[test]
    fn first_receipt_does_not_discard_unsettled_downloads() {
        const GB: u64 = 1024 * 1024 * 1024;
        use vtorrent_core::receipt::BandwidthReceipt;

        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.record_download(GB);
        let receipt = BandwidthReceipt {
            info_hash: [1u8; 20],
            window_start: 0,
            window_end: 300,
            uploaded_by_me: GB,
            downloaded_by_me: 0,
            peer: "VUs".into(),
            signer: "VPeer".into(),
            settlement_id: 1,
            epoch: account.receipt_epoch,
        };
        assert!(account.record_receipt(receipt.clone()));
        // The first receipt covers the earlier download instead of discarding
        // it while anchoring the peer's epoch.
        assert_eq!(
            account.agreed_owed().map(|agreement| agreement.satoshis),
            Some(COIN / 2),
            "bytes downloaded before the first receipt must remain payable"
        );
    }

    #[test]
    fn epoch_change_settles_the_previous_receipt_before_rebasing() {
        const GB: u64 = 1024 * 1024 * 1024;
        use vtorrent_core::receipt::BandwidthReceipt;

        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.peer_epoch = Some(account.receipt_epoch);
        account.record_download(2 * GB);
        let mut receipt = BandwidthReceipt {
            info_hash: [1u8; 20],
            window_start: 0,
            window_end: 300,
            uploaded_by_me: 2 * GB,
            downloaded_by_me: 0,
            peer: "VUs".into(),
            signer: "VPeer".into(),
            settlement_id: 1,
            epoch: account.receipt_epoch,
        };
        assert!(account.record_receipt(receipt.clone()));
        assert_eq!(account.agreed_owed().map(|a| a.satoshis), Some(COIN));

        // A new epoch arrives before the first agreement was settled. The old
        // claim must move into the unpaid balance, not disappear.
        receipt.epoch = account.receipt_epoch.wrapping_add(1);
        receipt.settlement_id = 2;
        receipt.uploaded_by_me = 0;
        assert!(account.record_receipt(receipt));
        assert_eq!(account.unpaid_satoshis, COIN);
        assert_eq!(account.total_paid_satoshis, 0);
    }
}
