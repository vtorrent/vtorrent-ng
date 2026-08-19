//! Incentive payment events emitted by the torrent engine.

use serde::{Deserialize, Serialize};

/// A payment that is due to a peer for bandwidth exchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentDue {
    /// The peer's VTR address.
    pub peer_address: String,
    /// Amount owed in satoshis.
    pub amount_satoshis: u64,
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
        });
        let payment = receiver.recv().await.unwrap();
        assert_eq!(payment.peer_address, "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT");
        assert_eq!(payment.amount_satoshis, 50_000_000);
    }
}
