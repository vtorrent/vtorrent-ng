//! Append-only JSONL journal for incentive accounts.
//!
//! Each record carries the full `PeerBandwidthAccount`, so a crash can restore
//! receipt-backed counters, the peer epoch, unpaid debt, and a pending txid.
//! The last record per `(info_hash, peer)` wins. A torn final line is discarded;
//! any other corruption fails startup instead of silently dropping debt.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use vtorrent_torrent::incentive::{IncentiveJournal, PeerBandwidthAccount};

const VERSION: u8 = 1;
const MAX_LINES: usize = 4_096;
const MAX_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalRecord {
    version: u8,
    #[serde(default)]
    info_hash: Option<String>,
    #[serde(default)]
    peer_address: Option<String>,
    #[serde(default)]
    receipt_epoch: u64,
    #[serde(default)]
    account: Option<PeerBandwidthAccount>,
    #[serde(default)]
    deleted: bool,
    #[serde(default)]
    emitted_total: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct JournalAccount {
    pub info_hash: [u8; 20],
    pub peer_address: String,
    pub receipt_epoch: u64,
    pub account: PeerBandwidthAccount,
}

struct State {
    path: PathBuf,
    file: std::fs::File,
    records: HashMap<String, JournalAccount>,
    emitted_total: u64,
    lines: usize,
    poisoned: bool,
}

pub struct JsonlJournal {
    state: Mutex<State>,
}

fn key(info_hash: &[u8; 20], peer: &str, receipt_epoch: u64) -> String {
    format!("{}:{}:{}", hex::encode(info_hash), peer, receipt_epoch)
}

impl JsonlJournal {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut records: HashMap<String, JournalAccount> = HashMap::new();
        let mut emitted_total = 0u64;
        let mut lines = 0usize;

        if path.exists() {
            let bytes = std::fs::read(path)?;
            let mut valid_end = 0usize;
            let mut offset = 0usize;
            for raw in bytes.split(|byte| *byte == b'\n') {
                let line_end = offset + raw.len();
                let has_newline = line_end < bytes.len();
                offset = line_end + usize::from(has_newline);
                if raw.is_empty() {
                    valid_end = offset;
                    continue;
                }
                match serde_json::from_slice::<JournalRecord>(raw) {
                    Ok(record) => {
                        if record.version != VERSION {
                            anyhow::bail!(
                                "incentive journal {} has unsupported version {}",
                                path.display(),
                                record.version
                            );
                        }
                        if let Some(total) = record.emitted_total {
                            emitted_total = total;
                        }
                        if let (Some(info), Some(peer)) =
                            (record.info_hash.as_deref(), record.peer_address.as_deref())
                        {
                            let Ok(info_bytes) = hex::decode(info) else {
                                anyhow::bail!("incentive journal has an invalid info hash");
                            };
                            let Ok(info_hash) = <[u8; 20]>::try_from(info_bytes) else {
                                anyhow::bail!("incentive journal has an invalid info hash length");
                            };
                            let receipt_epoch = record.receipt_epoch;
                            let entry = key(&info_hash, peer, receipt_epoch);
                            if record.deleted {
                                records.remove(&entry);
                            } else if let Some(account) = record.account {
                                records.insert(
                                    entry,
                                    JournalAccount {
                                        info_hash,
                                        peer_address: peer.to_string(),
                                        receipt_epoch,
                                        account,
                                    },
                                );
                            }
                        }
                        lines += 1;
                        valid_end = offset;
                    }
                    Err(error) => {
                        // A crash during append can leave a partial last line.
                        // Any other invalid line is corruption: fail closed.
                        if has_newline {
                            return Err(error).map_err(|error| {
                                anyhow::anyhow!(
                                    "corrupt incentive journal {}: {error}",
                                    path.display()
                                )
                            });
                        }
                        break;
                    }
                }
            }
            if valid_end < bytes.len() {
                let file = std::fs::OpenOptions::new().write(true).open(path)?;
                file.set_len(valid_end as u64)?;
                file.sync_all()?;
            }
        }

        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        // Make the directory entry durable across a power loss.
        if let Some(parent) = path.parent() {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(Self {
            state: Mutex::new(State {
                path: path.to_path_buf(),
                file,
                records,
                emitted_total,
                lines,
                poisoned: false,
            }),
        })
    }

    pub fn accounts(&self) -> Vec<JournalAccount> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .records
            .values()
            .cloned()
            .collect()
    }

    pub fn emitted_total(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .emitted_total
    }

    fn append(state: &mut State, record: &JournalRecord) -> Result<(), String> {
        if state.poisoned {
            return Err("incentive journal is poisoned; restart required".into());
        }
        let mut bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        if let Err(error) = state.file.write_all(&bytes) {
            state.poisoned = true;
            return Err(error.to_string());
        }
        if let Err(error) = state.file.sync_data() {
            state.poisoned = true;
            return Err(error.to_string());
        }
        state.lines += 1;
        Ok(())
    }

    fn maybe_compact(state: &mut State) -> Result<(), String> {
        if state.lines > MAX_LINES
            || state.file.metadata().map(|m| m.len()).unwrap_or(0) > MAX_BYTES
        {
            if let Err(error) = Self::compact(state) {
                state.poisoned = true;
                return Err(error);
            }
        }
        Ok(())
    }

    fn compact(state: &mut State) -> Result<(), String> {
        let temporary = state.path.with_extension("jsonl.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        let budget = JournalRecord {
            version: VERSION,
            info_hash: None,
            peer_address: None,
            receipt_epoch: 0,
            account: None,
            deleted: false,
            emitted_total: Some(state.emitted_total),
        };
        let mut bytes = serde_json::to_vec(&budget).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        for account in state.records.values() {
            let record = JournalRecord {
                version: VERSION,
                info_hash: Some(hex::encode(account.info_hash)),
                peer_address: Some(account.peer_address.clone()),
                receipt_epoch: account.receipt_epoch,
                account: Some(account.account.clone()),
                deleted: false,
                emitted_total: None,
            };
            let mut bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
            bytes.push(b'\n');
            file.write_all(&bytes).map_err(|error| error.to_string())?;
        }
        file.sync_all().map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, &state.path).map_err(|error| error.to_string())?;
        if let Some(parent) = state.path.parent() {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        state.file = std::fs::OpenOptions::new()
            .append(true)
            .open(&state.path)
            .map_err(|error| error.to_string())?;
        state.lines = state.records.len() + 1;
        Ok(())
    }
}

impl IncentiveJournal for JsonlJournal {
    fn upsert(
        &self,
        info_hash: [u8; 20],
        peer_address: &str,
        receipt_epoch: u64,
        account: &PeerBandwidthAccount,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let entry = key(&info_hash, peer_address, receipt_epoch);
        if let Some(stored) = state.records.get(&entry) {
            if stored.account.state_version > account.state_version {
                return Ok(());
            }
        }
        let record = JournalRecord {
            version: VERSION,
            info_hash: Some(hex::encode(info_hash)),
            peer_address: Some(peer_address.to_string()),
            receipt_epoch,
            account: Some(account.clone()),
            deleted: false,
            emitted_total: None,
        };
        Self::append(&mut state, &record)?;
        state.records.insert(
            entry,
            JournalAccount {
                info_hash,
                peer_address: peer_address.to_string(),
                receipt_epoch,
                account: account.clone(),
            },
        );
        Self::maybe_compact(&mut state)?;
        Ok(())
    }

    fn remove(
        &self,
        info_hash: [u8; 20],
        peer_address: &str,
        receipt_epoch: u64,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let record = JournalRecord {
            version: VERSION,
            info_hash: Some(hex::encode(info_hash)),
            peer_address: Some(peer_address.to_string()),
            receipt_epoch,
            account: None,
            deleted: true,
            emitted_total: None,
        };
        Self::append(&mut state, &record)?;
        state
            .records
            .remove(&key(&info_hash, peer_address, receipt_epoch));
        Self::maybe_compact(&mut state)?;
        Ok(())
    }

    fn set_emitted_total(&self, emitted_total: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let record = JournalRecord {
            version: VERSION,
            info_hash: None,
            peer_address: None,
            receipt_epoch: 0,
            account: None,
            deleted: false,
            emitted_total: Some(emitted_total),
        };
        Self::append(&mut state, &record)?;
        state.emitted_total = emitted_total;
        Self::maybe_compact(&mut state)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.file.sync_all().map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(unpaid: u64) -> PeerBandwidthAccount {
        let mut account = PeerBandwidthAccount::new("VPeer".into());
        account.unpaid_satoshis = unpaid;
        account
    }

    #[test]
    fn last_record_wins_and_deleted_records_remove() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let journal = JsonlJournal::open(&path).unwrap();
        journal
            .upsert([1u8; 20], "VPeer", 7, &account(100))
            .unwrap();
        journal
            .upsert([1u8; 20], "VPeer", 7, &account(200))
            .unwrap();
        journal.set_emitted_total(7).unwrap();
        let reopened = JsonlJournal::open(&path).unwrap();
        assert_eq!(reopened.accounts().len(), 1);
        assert_eq!(reopened.accounts()[0].account.unpaid_satoshis, 200);
        assert_eq!(reopened.emitted_total(), 7);
        reopened.remove([1u8; 20], "VPeer", 7).unwrap();
        let reopened = JsonlJournal::open(&path).unwrap();
        assert!(reopened.accounts().is_empty());
    }

    #[test]
    fn a_poisoned_journal_refuses_further_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let journal = JsonlJournal::open(&path).unwrap();
        {
            let mut state = journal.state.lock().unwrap();
            state.poisoned = true;
        }
        assert!(journal
            .upsert([1u8; 20], "VPeer", 7, &account(100))
            .is_err());
    }

    #[test]
    fn an_older_account_version_cannot_overwrite_a_newer_one() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let journal = JsonlJournal::open(&path).unwrap();
        let mut newer = account(100);
        newer.state_version = 2;
        journal.upsert([1u8; 20], "VPeer", 7, &newer).unwrap();
        let mut older = account(999);
        older.state_version = 1;
        journal.upsert([1u8; 20], "VPeer", 7, &older).unwrap();
        let reopened = JsonlJournal::open(&path).unwrap();
        assert_eq!(reopened.accounts()[0].account.unpaid_satoshis, 100);
        assert_eq!(reopened.accounts()[0].account.state_version, 2);
    }

    #[test]
    fn torn_final_line_is_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        {
            let journal = JsonlJournal::open(&path).unwrap();
            journal
                .upsert([1u8; 20], "VPeer", 7, &account(100))
                .unwrap();
        }
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"version\":1,\"info_hash\":\"01");
        std::fs::write(&path, bytes).unwrap();
        let journal = JsonlJournal::open(&path).unwrap();
        assert_eq!(journal.accounts().len(), 1);
        assert_eq!(journal.accounts()[0].account.unpaid_satoshis, 100);
    }

    #[test]
    fn the_record_that_triggers_compaction_is_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let journal = JsonlJournal::open(&path).unwrap();
        {
            let mut state = journal.state.lock().unwrap();
            state.lines = MAX_LINES;
        }
        journal
            .upsert([7u8; 20], "VPeer", 9, &account(500))
            .unwrap();
        let reopened = JsonlJournal::open(&path).unwrap();
        let records = reopened.accounts();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].account.unpaid_satoshis, 500);
        assert_eq!(records[0].receipt_epoch, 9);
    }

    #[test]
    fn corrupt_middle_line_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        {
            let journal = JsonlJournal::open(&path).unwrap();
            journal
                .upsert([1u8; 20], "VPeer", 7, &account(100))
                .unwrap();
        }
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"not-json\n");
        std::fs::write(&path, bytes).unwrap();
        assert!(JsonlJournal::open(&path).is_err());
    }
}
