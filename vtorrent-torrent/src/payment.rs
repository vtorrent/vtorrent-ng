//! Incentive payment events emitted by the torrent engine.

use serde::{Deserialize, Serialize};

/// A payment that is due to a peer for bandwidth exchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentDue {
    /// The peer's VTR address.
    pub peer_address: String,
    /// Amount owed in satoshis.
    pub amount_satoshis: u64,
    /// Stable key for the originating incentive account.
    #[serde(default)]
    pub account_key: String,
    /// Torrent and receipt epoch, used by the payment-intent journal.
    #[serde(default)]
    pub info_hash: [u8; 20],
    #[serde(default)]
    pub receipt_epoch: u64,
}

/// Result of a payment attempt, keyed back to its incentive account.
#[derive(Debug, Clone)]
pub struct PaymentResult {
    pub account_key: String,
    pub amount_satoshis: u64,
    pub success: bool,
    /// The broadcast transaction, present only on success.
    pub txid: Option<[u8; 32]>,
}

/// Amount to put in one payment, bounded by the per-payment cap and the
/// remaining process-wide budget.
pub fn capped_payment_amount(unpaid: u64, per_payment_cap: u64, remaining_budget: u64) -> u64 {
    unpaid.min(per_payment_cap).min(remaining_budget)
}

/// Whether a payment at `tx_height` is buried deeply enough to settle.
///
/// Six confirmations keeps a shallow reorg from clearing the debt and then
/// removing the transaction.
pub fn payment_is_final(best_height: u32, tx_height: u32) -> bool {
    best_height.saturating_sub(tx_height).saturating_add(1) >= 6
}

/// A channel for emitting payment events to the daemon.
#[derive(Clone)]
pub struct PaymentSender {
    tx: tokio::sync::mpsc::UnboundedSender<PaymentDue>,
}

impl PaymentSender {
    /// Create a new payment channel, returning (sender, receiver).
    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<PaymentDue>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Emit a payment event (non-blocking; drops if the receiver is gone).
    pub fn emit(&self, payment: PaymentDue) {
        let _ = self.tx.send(payment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_payment_channel_roundtrip() {
        let (sender, mut receiver) = PaymentSender::channel();
        sender.emit(PaymentDue {
            peer_address: "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            amount_satoshis: 50_000_000,
            account_key: "session\u{0}peer".to_string(),
            info_hash: [1u8; 20],
            receipt_epoch: 7,
        });
        let payment = receiver.recv().await.unwrap();
        assert_eq!(payment.peer_address, "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT");
        assert_eq!(payment.amount_satoshis, 50_000_000);
    }

    #[test]
    fn payment_caps_the_single_payment_and_the_remaining_budget() {
        assert_eq!(capped_payment_amount(500, 100, 1_000), 100);
        assert_eq!(capped_payment_amount(500, 1_000, 40), 40);
        assert_eq!(capped_payment_amount(500, 1_000, 0), 0);
    }

    #[test]
    fn payment_settles_only_after_six_confirmations() {
        assert!(!payment_is_final(100, 100));
        assert!(!payment_is_final(104, 100));
        assert!(payment_is_final(105, 100));
        assert!(payment_is_final(106, 100));
    }
}
