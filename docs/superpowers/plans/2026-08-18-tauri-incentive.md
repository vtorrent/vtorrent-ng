# Tauri Command Gap + Incentive Settlement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the desktop app's missing `add_torrent`/`remove_torrent` Tauri commands and wire a periodic incentive settlement loop.

**Architecture:** Add `add_torrent` and `remove_torrent` Tauri commands mirroring the RPC handlers (parse, create session, spawn engine; cancel + remove). Add a daemon task that periodically calls `settle()` on due `PeerBandwidthAccount`s.

**Tech Stack:** Rust (edition 2021), `tauri` 2, `tokio`, `tokio-util`.

**Spec:** `docs/superpowers/specs/2026-08-18-tauri-incentive-design.md`

---

## File Structure

**Modified:**
- `vtorrent-tauri/src/commands.rs` — add `add_torrent`, `remove_torrent`
- `vtorrent-tauri/src/main.rs` — register the commands
- `vtorrent-daemon/src/main.rs` — add the incentive settlement loop

---

## Task 1: Add `add_torrent` and `remove_torrent` Tauri commands

**Files:**
- Modify: `vtorrent-tauri/src/commands.rs`
- Modify: `vtorrent-tauri/src/main.rs`

- [ ] **Step 1: Add the commands**

In `vtorrent-tauri/src/commands.rs`, add after `get_torrent_sessions` (around line 603):

```rust
/// Add a torrent (magnet link or base64 .torrent file).
///
/// Called from: `TorrentPage.tsx` → `invoke('add_torrent', { source, sourceType, walletAddress })`
#[tauri::command]
pub async fn add_torrent(
    state: tauri::State<'_, AppState>,
    source: String,
    source_type: String,
    wallet_address: String,
) -> Result<AddTorrentResult> {
    use vtorrent_torrent::metainfo::{MagnetLink, Metainfo};
    use vtorrent_torrent::session::TorrentSession;

    let metainfo = if source_type == "magnet" {
        let magnet = MagnetLink::parse(&source).map_err(|e| TauriError::Torrent(e.to_string()))?;
        Metainfo::from_magnet_link(&magnet)
    } else {
        let bytes = B64.decode(&source).map_err(|e| TauriError::Torrent(e.to_string()))?;
        Metainfo::from_bytes(&bytes).map_err(|e| TauriError::Torrent(e.to_string()))?
    };

    let info_hash = hex::encode(metainfo.info_hash);
    let name = metainfo.name.clone();
    let session = TorrentSession::new(metainfo, wallet_address);

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let session_id = handle
        .rpc_state
        .torrent_sessions
        .write()
        .await
        .add_session(session);

    // Spawn the download engine for this session.
    let cancel = tokio_util::sync::CancellationToken::new();
    handle
        .rpc_state
        .torrent_cancels
        .write()
        .await
        .insert(session_id.clone(), cancel.clone());
    let sessions = Arc::clone(&handle.rpc_state.torrent_sessions);
    let download_dir = handle.rpc_state.download_dir.read().await.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        vtorrent_torrent::engine::run_engine(sid, sessions, download_dir, cancel).await;
    });

    Ok(AddTorrentResult {
        session_id,
        info_hash,
        name,
    })
}

/// Remove a torrent session.
///
/// Called from: `TorrentPage.tsx` → `invoke('remove_torrent', { id })`
#[tauri::command]
pub async fn remove_torrent(state: tauri::State<'_, AppState>, id: String) -> Result<()> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    if let Some(cancel) = handle.rpc_state.torrent_cancels.write().await.remove(&id) {
        cancel.cancel();
    }
    handle
        .rpc_state
        .torrent_sessions
        .write()
        .await
        .remove_session(&id)
        .ok_or_else(|| TauriError::Torrent(format!("Session {} not found", id)))?;
    Ok(())
}
```

- [ ] **Step 2: Add the `AddTorrentResult` type**

In `vtorrent-tauri/src/commands.rs`, add near the other result types (after `TorrentResult`, around line 440):

```rust
#[derive(Debug, Serialize)]
pub struct AddTorrentResult {
    pub session_id: String,
    pub info_hash: String,
    pub name: String,
}
```

- [ ] **Step 3: Add the `Arc` import**

In `vtorrent-tauri/src/commands.rs`, add `use std::sync::Arc;` near the top (after `use tauri::State;`):

```rust
use std::sync::Arc;
```

- [ ] **Step 4: Add the `Torrent` error variant**

In `vtorrent-tauri/src/error.rs`, add a variant to `TauriError` (after `NodeError`):

```rust
    #[error("Node error: {0}")]
    NodeError(String),

    #[error("Torrent error: {0}")]
    Torrent(String),
```

- [ ] **Step 5: Register the commands**

In `vtorrent-tauri/src/main.rs`, add after `commands::get_torrent_sessions,`:

```rust
            commands::add_torrent,
            commands::remove_torrent,
```

- [ ] **Step 6: Build and commit**

Run: `cargo build -p vtorrent-tauri 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-tauri/src/commands.rs vtorrent-tauri/src/main.rs vtorrent-tauri/src/error.rs
git commit -m "feat: add add_torrent and remove_torrent Tauri commands"
```

---

## Task 2: Add the incentive settlement loop

**Files:**
- Modify: `vtorrent-daemon/src/main.rs`

- [ ] **Step 1: Add the settlement task**

In `vtorrent-daemon/src/main.rs`, after the DEX maintenance task (around line 465), add:

```rust
    // Periodic torrent incentive settlement — runs every 5 minutes.
    let torrent_sessions_for_settlement = Arc::clone(&rpc_state.torrent_sessions);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
            vtorrent_torrent::incentive::PAYMENT_INTERVAL_SECS,
        ));
        loop {
            interval.tick().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut guard = torrent_sessions_for_settlement.write().await;
            let mut settled = 0;
            for session in guard.sessions_mut() {
                for account in session.incentive_accounts.values_mut() {
                    if account.needs_settlement(now) {
                        account.settle(now);
                        settled += 1;
                    }
                }
            }
            if settled > 0 {
                tracing::info!("Torrent incentive: settled {} peer accounts", settled);
            }
        }
    });
```

- [ ] **Step 2: Add `sessions_mut` to `SessionManager`**

The loop calls `guard.sessions_mut()`, which doesn't exist yet. In `vtorrent-torrent/src/session.rs`, add to `impl SessionManager` (after `list_sessions`):

```rust
    /// Iterate over all sessions mutably.
    pub fn sessions_mut(&mut self) -> impl Iterator<Item = &mut TorrentSession> {
        self.sessions.values_mut()
    }
```

- [ ] **Step 3: Build and commit**

Run: `cargo build -p vtorrent-daemon 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-daemon/src/main.rs vtorrent-torrent/src/session.rs
git commit -m "feat: add torrent incentive settlement loop"
```

---

## Final Verification

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace --all-features 2>&1 | rg "test result: FAILED|error\["`
Expected: no failures.

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | rg "warning:|error:"`
Expected: no output.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 3: Commit any remaining changes**

```bash
git add -A
git commit -m "chore: final verification of Tauri commands and incentive settlement"
```
