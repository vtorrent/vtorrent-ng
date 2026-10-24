//! Persistent wallet activity journal.
//!
//! The chain and mempool are authoritative for confirmed and pending
//! transactions. This journal adds the transitions they cannot express:
//! broadcast, failed, and reorged.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Result, TauriError};
use crate::state::AppState;

const MAX_ENTRIES: usize = 500;
const DROP_AFTER_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub txid: String,
    pub status: String,
    pub tx_type: String,
    pub amount_satoshis: u64,
    pub timestamp: u64,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ActivityJournal {
    #[serde(default)]
    pub entries: Vec<JournalEntry>,
}

fn journal_path(state: &AppState) -> Option<PathBuf> {
    state
        .app_data_dir
        .lock()
        .unwrap()
        .clone()
        .map(|dir| dir.join("activity.json"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl ActivityJournal {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TauriError::Io(e.to_string()))?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| TauriError::Io(e.to_string()))?;
        let tmp = path.with_extension(format!("json.tmp-{:08x}", rand::random::<u32>()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|e| TauriError::Io(e.to_string()))?;
        file.write_all(&bytes)
            .map_err(|e| TauriError::Io(e.to_string()))?;
        file.sync_all().map_err(|e| TauriError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| TauriError::Io(e.to_string()))?;
        Ok(())
    }

    pub fn record_broadcast(&mut self, txid: &str, tx_type: &str, amount_satoshis: u64) {
        let now = unix_now();
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.txid == txid) {
            entry.status = "broadcast".to_string();
            entry.timestamp = now;
            entry.error = None;
        } else {
            self.entries.push(JournalEntry {
                txid: txid.to_string(),
                status: "broadcast".to_string(),
                tx_type: tx_type.to_string(),
                amount_satoshis,
                timestamp: now,
                error: None,
            });
        }
        self.prune();
    }

    pub fn record_failed(&mut self, error: &str) {
        let now = unix_now();
        self.entries.push(JournalEntry {
            txid: format!("failed-{now}-{:08x}", rand::random::<u32>()),
            status: "failed".to_string(),
            tx_type: "send".to_string(),
            amount_satoshis: 0,
            timestamp: now,
            error: Some(error.to_string()),
        });
        self.prune();
    }

    pub fn reconcile(&mut self, confirmed: &HashSet<String>, pending: &HashSet<String>, now: u64) {
        for entry in &mut self.entries {
            match entry.status.as_str() {
                "broadcast" => {
                    if confirmed.contains(&entry.txid) {
                        entry.status = "confirmed".to_string();
                        entry.error = None;
                    } else if pending.contains(&entry.txid) {
                        entry.status = "broadcast".to_string();
                    } else if now.saturating_sub(entry.timestamp) > DROP_AFTER_SECS {
                        entry.status = "failed".to_string();
                        entry.error = Some(
                            "Not observed in the mempool or chain after 30 minutes".to_string(),
                        );
                    }
                }
                "confirmed" => {
                    if !confirmed.contains(&entry.txid) {
                        entry.status = "reorged".to_string();
                        entry.error = Some(
                            "Confirmed transaction disappeared after a chain reorganization"
                                .to_string(),
                        );
                    }
                }
                _ => {}
            }
        }
        self.prune();
    }

    fn prune(&mut self) {
        if self.entries.len() > MAX_ENTRIES {
            self.entries.sort_by_key(|entry| entry.timestamp);
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(0..excess);
        }
    }
}

pub fn record_broadcast(state: &AppState, txid: &str, tx_type: &str, amount_satoshis: u64) {
    let Some(path) = journal_path(state) else {
        return;
    };
    let mut journal = ActivityJournal::load(&path);
    journal.record_broadcast(txid, tx_type, amount_satoshis);
    if let Err(error) = journal.save(&path) {
        tracing::warn!("Could not persist activity journal: {}", error);
    }
}

pub fn record_failed(state: &AppState, error: &str) {
    let Some(path) = journal_path(state) else {
        return;
    };
    let mut journal = ActivityJournal::load(&path);
    journal.record_failed(error);
    if let Err(save_error) = journal.save(&path) {
        tracing::warn!("Could not persist activity journal: {}", save_error);
    }
}

/// Merge journal-only states into the chain/mempool activity list.
pub fn merge(
    state: &AppState,
    entries: &mut Vec<vtorrent_node::activity::ActivityEntry>,
    now: u32,
) {
    let Some(path) = journal_path(state) else {
        return;
    };
    let mut journal = ActivityJournal::load(&path);
    let confirmed: HashSet<String> = entries
        .iter()
        .filter(|entry| entry.status == "confirmed")
        .map(|entry| entry.txid.clone())
        .collect();
    let pending: HashSet<String> = entries
        .iter()
        .filter(|entry| entry.status == "pending")
        .map(|entry| entry.txid.clone())
        .collect();
    journal.reconcile(&confirmed, &pending, now as u64);
    if let Err(error) = journal.save(&path) {
        tracing::warn!("Could not persist activity journal: {}", error);
    }

    for entry in journal.entries {
        if let Some(existing) = entries.iter_mut().find(|item| item.txid == entry.txid) {
            if existing.status == "pending" && entry.status == "broadcast" {
                existing.status = "broadcast".to_string();
            }
            continue;
        }
        entries.push(vtorrent_node::activity::ActivityEntry {
            txid: entry.txid,
            status: entry.status,
            tx_type: entry.tx_type,
            amount_satoshis: entry.amount_satoshis,
            timestamp: entry.timestamp as u32,
            block_height: 0,
            confirmations: 0,
            fee_satoshis: 0,
            error: entry.error,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_tracks_broadcast_confirmed_and_reorged() {
        let mut journal = ActivityJournal::default();
        journal.record_broadcast("tx1", "send", 100);
        assert_eq!(journal.entries[0].status, "broadcast");

        let confirmed = HashSet::from(["tx1".to_string()]);
        journal.reconcile(&confirmed, &HashSet::new(), unix_now());
        assert_eq!(journal.entries[0].status, "confirmed");

        journal.reconcile(&HashSet::new(), &HashSet::new(), unix_now());
        assert_eq!(journal.entries[0].status, "reorged");
        assert!(journal.entries[0].error.is_some());
    }

    #[test]
    fn journal_records_failed_sends() {
        let mut journal = ActivityJournal::default();
        journal.record_failed("mempool rejected");
        assert_eq!(journal.entries.len(), 1);
        assert_eq!(journal.entries[0].status, "failed");
        assert_eq!(
            journal.entries[0].error.as_deref(),
            Some("mempool rejected")
        );
    }
}
