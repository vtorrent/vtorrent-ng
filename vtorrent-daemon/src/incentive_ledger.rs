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
            key: format!("{}:{}", hex::encode(info_hash), peer_address),
            peer_address: peer_address.to_string(),
            unpaid_satoshis: account.unpaid_satoshis,
            pending_txid,
            pending_satoshis,
            payment_in_flight: false,
        });
    }

    pub fn debt_mut(&mut self, id: u64) -> Option<&mut IncentiveDebt> {
        self.debts.iter_mut().find(|debt| debt.id == id)
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
}
