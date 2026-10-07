//! Wallet metadata: address book (contacts) and per-transaction notes.
//!
//! Stored in a 0600 sidecar next to the wallet file (`wallet_meta.json`), not in
//! the encrypted wallet — this avoids a wallet-format migration and keeps
//! non-key metadata separate. Notes are **local only** and never broadcast.
//! See `docs/wallet-organization-design.md`.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{RpcError, RpcResult};
use crate::state::AppState;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WalletMeta {
    #[serde(default)]
    pub contacts: Vec<Contact>,
    /// txid (hex) -> local note.
    #[serde(default)]
    pub notes: BTreeMap<String, String>,
}

/// The sidecar path: `wallet_meta.json` beside the wallet file. `None` when the
/// wallet is not persisted (ephemeral/test instances).
fn meta_path(state: &AppState) -> Option<PathBuf> {
    state
        .wallet_path
        .as_ref()
        .map(|p| p.with_file_name("wallet_meta.json"))
}

fn load_meta(path: &Path) -> WalletMeta {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_meta(path: &Path, meta: &WalletMeta) -> RpcResult<()> {
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|e| RpcError::Internal(format!("meta serialize failed: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &bytes))
            .map_err(|e| RpcError::Internal(format!("meta write failed: {e}")))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, &bytes)
            .map_err(|e| RpcError::Internal(format!("meta write failed: {e}")))?;
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| RpcError::Internal(format!("meta rename failed: {e}")))?;
    Ok(())
}

// ─── Contacts ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddContactRequest {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// GET /api/v1/wallet/contacts
pub async fn list_contacts(State(state): State<Arc<AppState>>) -> RpcResult<Json<Vec<Contact>>> {
    let Some(path) = meta_path(&state) else {
        return Ok(Json(Vec::new()));
    };
    Ok(Json(load_meta(&path).contacts))
}

/// POST /api/v1/wallet/contacts
pub async fn add_contact(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddContactRequest>,
) -> RpcResult<Json<Contact>> {
    if req.name.trim().is_empty() || req.address.trim().is_empty() {
        return Err(RpcError::BadRequest("name and address are required".into()));
    }
    // Validate the address so a typo can't be saved as a contact.
    vtorrent_core::address::validate_p2pkh(&req.address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;
    let Some(path) = meta_path(&state) else {
        return Err(RpcError::Internal("wallet is not persisted".into()));
    };
    let mut meta = load_meta(&path);
    let contact = Contact {
        name: req.name.trim().to_string(),
        address: req.address.trim().to_string(),
        note: req.note,
        created_at: vtorrent_core::time::now_secs(),
    };
    meta.contacts.push(contact.clone());
    save_meta(&path, &meta)?;
    Ok(Json(contact))
}

#[derive(Debug, Deserialize)]
pub struct DeleteContactRequest {
    pub address: String,
}

/// POST /api/v1/wallet/contacts/delete
pub async fn delete_contact(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DeleteContactRequest>,
) -> RpcResult<Json<serde_json::Value>> {
    let Some(path) = meta_path(&state) else {
        return Err(RpcError::Internal("wallet is not persisted".into()));
    };
    let mut meta = load_meta(&path);
    let before = meta.contacts.len();
    meta.contacts.retain(|c| c.address != req.address);
    let removed = before - meta.contacts.len();
    save_meta(&path, &meta)?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

// ─── Notes ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SetNoteRequest {
    pub txid: String,
    /// Empty string deletes the note.
    #[serde(default)]
    pub note: String,
}

/// POST /api/v1/wallet/notes
pub async fn set_note(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetNoteRequest>,
) -> RpcResult<Json<serde_json::Value>> {
    if req.txid.trim().is_empty() {
        return Err(RpcError::BadRequest("txid is required".into()));
    }
    let Some(path) = meta_path(&state) else {
        return Err(RpcError::Internal("wallet is not persisted".into()));
    };
    let mut meta = load_meta(&path);
    if req.note.trim().is_empty() {
        meta.notes.remove(req.txid.trim());
    } else {
        meta.notes
            .insert(req.txid.trim().to_string(), req.note.clone());
    }
    save_meta(&path, &meta)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// GET /api/v1/wallet/notes
pub async fn list_notes(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<BTreeMap<String, String>>> {
    let Some(path) = meta_path(&state) else {
        return Ok(Json(BTreeMap::new()));
    };
    Ok(Json(load_meta(&path).notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_roundtrip_and_note_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wallet_meta.json");
        let mut meta = WalletMeta::default();
        meta.contacts.push(Contact {
            name: "Alice".into(),
            address: "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k".into(),
            note: None,
            created_at: 1,
        });
        meta.notes.insert("abc".into(), "rent".into());
        save_meta(&path, &meta).unwrap();

        let loaded = load_meta(&path);
        assert_eq!(loaded.contacts.len(), 1);
        assert_eq!(loaded.notes.get("abc").map(String::as_str), Some("rent"));

        // Deleting a note.
        let mut m2 = load_meta(&path);
        m2.notes.remove("abc");
        save_meta(&path, &m2).unwrap();
        assert!(load_meta(&path).notes.is_empty());
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let meta = load_meta(&dir.path().join("nope.json"));
        assert!(meta.contacts.is_empty() && meta.notes.is_empty());
    }
}
