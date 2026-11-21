//! Persistent incentive-debt ledger.
//!
//! Live torrent sessions are not persisted, so a daemon restart would
//! otherwise lose unpaid peer bandwidth debts. The payment task snapshots
//! outstanding accounts at shutdown and reloads them here on the next start.
//! A payment transaction stays pending until six confirmations; a transaction
//! that disappears is never re-paid, because the original can still be mined.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use vtorrent_torrent::incentive::PeerBandwidthAccount;

use crate::incentive_journal::JournalAccount;

/// One outstanding debt, independent of the live torrent session that produced
/// it. `id` is stable for the lifetime of the ledger entry so an asynchronous
/// payment result can find it after other entries are removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncentiveDebt {
    pub id: u64,
    pub key: String,
    pub peer_address: String,
    pub unpaid_satoshis: u64,
    pub pending_txid: Option<[u8; 32]>,
    pub pending_satoshis: u64,
    #[serde(default)]
    pub payment_in_flight: bool,
}

impl IncentiveDebt {
    /// Apply a confirmed payment to the debt. A per-payment cap can cover only
    /// part of it; the remainder stays payable.
    pub fn confirm_payment(&mut self, paid_satoshis: u64) {
        self.unpaid_satoshis = self.unpaid_satoshis.saturating_sub(paid_satoshis);
        self.pending_txid = None;
        self.pending_satoshis = 0;
        self.payment_in_flight = false;
    }

    pub fn has_outstanding(&self) -> bool {
        self.unpaid_satoshis > 0 || self.pending_txid.is_some() || self.payment_in_flight
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IncentiveLedger {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub emitted_total: u64,
    #[serde(default)]
    pub debts: Vec<IncentiveDebt>,
}

impl IncentiveLedger {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse incentive ledger {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => {
                Err(error).with_context(|| format!("read incentive ledger {}", path.display()))
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create incentive ledger dir {}", parent.display()))?;
        }
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(self).context("serialize incentive ledger")?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("open {}", temporary.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("rename {} to {}", temporary.display(), path.display()))?;
        Ok(())
    }

    /// Add an outstanding live account, assigning it a stable ledger id.
    pub fn add_account(
        &mut self,
        info_hash: &[u8; 20],
        peer_address: &str,
        account: &PeerBandwidthAccount,
    ) {
        if account.unpaid_satoshis == 0 && account.pending_payment.is_none() {
            return;
        }
        let (pending_txid, pending_satoshis) = account
            .pending_payment
            .map(|payment| (Some(payment.txid), payment.satoshis))
            .unwrap_or((None, 0));
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.debts.push(IncentiveDebt {
            id,
            key: format!(
                "{}:{}:{}",
                hex::encode(info_hash),
                peer_address,
                account.receipt_epoch
            ),
            peer_address: peer_address.to_string(),
            unpaid_satoshis: account.unpaid_satoshis,
            pending_txid,
            pending_satoshis,
            payment_in_flight: account.payment_in_flight,
        });
    }

    pub fn debt_mut(&mut self, id: u64) -> Option<&mut IncentiveDebt> {
        self.debts.iter_mut().find(|debt| debt.id == id)
    }

    /// Merge replayed journal accounts into the payment queue.
    ///
    /// The journal is authoritative for its exact `(info_hash, peer, receipt
    /// epoch)` key. Receipt-backed but unsettled bytes are converted into debt
    /// at `now`; a different epoch is a different account and both remain.
    pub fn merge_journal(&mut self, accounts: Vec<JournalAccount>, now: u64) {
        let mut by_key: std::collections::HashMap<String, JournalAccount> = accounts
            .into_iter()
            .map(|record| {
                let key = format!(
                    "{}:{}:{}",
                    hex::encode(record.info_hash),
                    record.peer_address,
                    record.receipt_epoch
                );
                (key, record)
            })
            .collect();
        // A ledger debt is the payment-progress record for its key. Never
        // replace it with the pre-payment journal snapshot: preserve the
        // pending txid, in-flight marker, and remaining unpaid amount.
        for debt in &mut self.debts {
            if let Some(record) = by_key.remove(&debt.key) {
                if debt.pending_txid.is_none() {
                    if let Some(pending) = record.account.pending_payment {
                        debt.pending_txid = Some(pending.txid);
                        debt.pending_satoshis = pending.satoshis;
                    }
                }
            }
        }
        for record in by_key.into_values() {
            let mut account = record.account;
            account.receipt_epoch = record.receipt_epoch;
            if let Some(agreement) = account.agreed_owed() {
                account.settle_agreed(now, agreement.satoshis, agreement.bytes);
            }
            self.add_account(&record.info_hash, &record.peer_address, &account);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_debts_and_the_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("incentive-ledger.json");
        let mut ledger = IncentiveLedger {
            next_id: 0,
            emitted_total: 42,
            debts: Vec::new(),
        };
        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.unpaid_satoshis = 1_000;
        ledger.add_account(&[7u8; 20], "VPeer", &account);
        assert_eq!(ledger.debts.len(), 1);
        assert_eq!(ledger.debts[0].id, 0);
        ledger.save(&path).unwrap();
        let loaded = IncentiveLedger::load(&path).unwrap();
        assert_eq!(loaded.emitted_total, 42);
        assert_eq!(loaded.debts.len(), 1);
        assert_eq!(loaded.debts[0].unpaid_satoshis, 1_000);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn outstanding_pending_payment_is_recorded() {
        let mut ledger = IncentiveLedger::default();
        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.unpaid_satoshis = 900;
        account.record_payment_broadcast([9u8; 32], 900);
        ledger.add_account(&[1u8; 20], "VPeer", &account);
        assert_eq!(ledger.debts[0].pending_txid, Some([9u8; 32]));
        assert_eq!(ledger.debts[0].pending_satoshis, 900);
    }

    #[test]
    fn a_partial_payment_leaves_the_remainder_payable() {
        let mut debt = IncentiveDebt {
            id: 1,
            key: "key".into(),
            peer_address: "VPeer".into(),
            unpaid_satoshis: 1_000,
            pending_txid: Some([9u8; 32]),
            pending_satoshis: 400,
            payment_in_flight: true,
        };
        debt.confirm_payment(400);
        assert_eq!(debt.unpaid_satoshis, 600);
        assert!(debt.has_outstanding());
        assert!(debt.pending_txid.is_none());
        debt.confirm_payment(600);
        assert!(!debt.has_outstanding());
    }

    fn receipt_account() -> PeerBandwidthAccount {
        use vtorrent_core::receipt::BandwidthReceipt;

        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.record_download(1024 * 1024 * 1024);
        account.peer_epoch = Some(account.receipt_epoch);
        let receipt = BandwidthReceipt {
            info_hash: [1u8; 20],
            window_start: 0,
            window_end: 300,
            uploaded_by_me: 1024 * 1024 * 1024,
            downloaded_by_me: 0,
            peer: "VUs".into(),
            signer: "VPeer".into(),
            settlement_id: 1,
            epoch: account.receipt_epoch,
        };
        assert!(account.record_receipt(receipt));
        account
    }

    #[test]
    fn journal_merge_settles_receipt_backed_bytes_into_debt() {
        let account = receipt_account();
        let epoch = account.receipt_epoch;
        let journal = JournalAccount {
            info_hash: [1u8; 20],
            peer_address: "VPeer".into(),
            receipt_epoch: epoch,
            account,
        };
        let mut ledger = IncentiveLedger::default();
        ledger.merge_journal(vec![journal], 1_700_000_000);
        assert_eq!(ledger.debts.len(), 1);
        assert_eq!(
            ledger.debts[0].unpaid_satoshis,
            vtorrent_torrent::incentive::COIN / 2
        );
        assert_eq!(
            ledger.debts[0].key,
            format!("{}:VPeer:{}", hex::encode([1u8; 20]), epoch)
        );
    }

    #[test]
    fn a_different_receipt_epoch_is_a_separate_debt() {
        let mut ledger = IncentiveLedger::default();
        let old = IncentiveDebt {
            id: 1,
            key: format!("{}:VPeer:1", hex::encode([1u8; 20])),
            peer_address: "VPeer".into(),
            unpaid_satoshis: 100,
            pending_txid: None,
            pending_satoshis: 0,
            payment_in_flight: false,
        };
        ledger.debts.push(old);
        let mut account = receipt_account();
        account.receipt_epoch = 2;
        let journal = JournalAccount {
            info_hash: [1u8; 20],
            peer_address: "VPeer".into(),
            receipt_epoch: 2,
            account,
        };
        ledger.merge_journal(vec![journal], 1_700_000_000);
        assert_eq!(ledger.debts.len(), 2);
        assert!(ledger.debts.iter().any(|debt| debt.key.ends_with(":1")));
        assert!(ledger.debts.iter().any(|debt| debt.key.ends_with(":2")));
    }

    #[test]
    fn journal_merge_preserves_ledger_payment_progress() {
        let account = receipt_account();
        let epoch = account.receipt_epoch;
        let key = format!("{}:VPeer:{}", hex::encode([1u8; 20]), epoch);
        let mut ledger = IncentiveLedger::default();
        ledger.debts.push(IncentiveDebt {
            id: 7,
            key: key.clone(),
            peer_address: "VPeer".into(),
            unpaid_satoshis: 700,
            pending_txid: Some([9u8; 32]),
            pending_satoshis: 400,
            payment_in_flight: false,
        });
        let journal = JournalAccount {
            info_hash: [1u8; 20],
            peer_address: "VPeer".into(),
            receipt_epoch: epoch,
            account,
        };
        ledger.merge_journal(vec![journal], 1_700_000_000);
        assert_eq!(ledger.debts.len(), 1);
        assert_eq!(ledger.debts[0].id, 7);
        assert_eq!(ledger.debts[0].pending_txid, Some([9u8; 32]));
        assert_eq!(ledger.debts[0].pending_satoshis, 400);
        assert_eq!(ledger.debts[0].unpaid_satoshis, 700);
    }
}
