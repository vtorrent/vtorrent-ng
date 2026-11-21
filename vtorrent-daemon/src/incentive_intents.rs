//! Durable payment intents for incentive payments.
//!
//! The payment worker writes the signed transaction and its txid here before
//! admission/broadcast. A crash can therefore never leave an in-flight account
//! whose payment was broadcast but whose txid was lost: startup either sees the
//! confirmed transaction, rebroadcasts the stored raw transaction, or determines
//! that no intent exists and safely releases the reservation.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentIntent {
    pub account_key: String,
    pub info_hash: [u8; 20],
    pub receipt_epoch: u64,
    pub peer_address: String,
    pub amount_satoshis: u64,
    pub txid: [u8; 32],
    /// JSON-serialized signed `Transaction`.
    pub raw: Vec<u8>,
    pub created_at: u64,
}

impl PaymentIntent {
    pub fn transaction(&self) -> Result<vtorrent_node::block::Transaction, String> {
        serde_json::from_slice(&self.raw).map_err(|error| error.to_string())
    }
}

pub struct IntentStore {
    path: PathBuf,
    state: Mutex<HashMap<String, PaymentIntent>>,
}

impl IntentStore {
    pub fn open(path: &Path) -> Result<Self> {
        let records = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Vec<PaymentIntent>>(&bytes)
                .with_context(|| format!("parse payment intents {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read payment intents {}", path.display()))
            }
        };
        let state = records
            .into_iter()
            .map(|intent| (intent.account_key.clone(), intent))
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(state),
        })
    }

    pub fn list(&self) -> Vec<PaymentIntent> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn put(&self, intent: PaymentIntent) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.insert(intent.account_key.clone(), intent);
        self.save(&state)
    }

    pub fn remove(&self, account_key: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.remove(account_key).is_some() {
            self.save(&state)?;
        }
        Ok(())
    }

    fn save(&self, state: &HashMap<String, PaymentIntent>) -> Result<()> {
        let records: Vec<&PaymentIntent> = state.values().collect();
        let bytes = serde_json::to_vec(&records).context("serialize payment intents")?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &self.path)?;
        if let Some(parent) = self.path.parent() {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(key: &str, txid: u8) -> PaymentIntent {
        PaymentIntent {
            account_key: key.into(),
            info_hash: [1u8; 20],
            receipt_epoch: 7,
            peer_address: "VPeer".into(),
            amount_satoshis: 1_000,
            txid: [txid; 32],
            raw: vec![1, 2, 3],
            created_at: 1,
        }
    }

    #[test]
    fn roundtrips_and_removes_intents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("intents.json");
        let store = IntentStore::open(&path).unwrap();
        store.put(intent("a", 1)).unwrap();
        store.put(intent("b", 2)).unwrap();
        let reopened = IntentStore::open(&path).unwrap();
        assert_eq!(reopened.list().len(), 2);
        reopened.remove("a").unwrap();
        let reopened = IntentStore::open(&path).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.list()[0].account_key, "b");
    }
}
