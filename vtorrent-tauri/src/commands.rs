/// vTorrent 2.0 Tauri IPC commands.
///
/// Each function decorated with `#[tauri::command]` becomes callable from
/// the React frontend via:
///   `import { invoke } from '@tauri-apps/api/core'`
///   `await invoke('command_name', { arg1, arg2 })`
///
/// All private key material is handled exclusively in Rust.
/// JavaScript only receives addresses, balances, and status flags.
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

use vtorrent_migrate::extractor::extract_wallet;
use vtorrent_wallet::wallet::Wallet;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

// ─── Request / Response types ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub keys_found: usize,
    pub addresses: Vec<String>,
    pub had_encryption: bool,
    pub had_2fa: bool,
    pub claimable_balance: u64,
}

#[derive(Debug, Serialize)]
pub struct WalletInfo {
    pub is_unlocked: bool,
    pub has_2fa: bool,
    pub address_count: usize,
    pub default_address: Option<String>,
    pub wallet_version: u32,
}

#[derive(Debug, Serialize)]
pub struct AddressInfo {
    pub address: String,
    pub label: String,
    pub balance: u64,
    pub is_legacy_import: bool,
}

#[derive(Debug, Serialize)]
pub struct Enable2FAResult {
    pub uri: String,
    pub secret: String,
    pub qr_data: String,
}

// ─── Wallet lifecycle commands ───────────────────────────────────────────────

/// Create a new wallet encrypted with the given passphrase.
///
/// Called from: `CreateWalletPage.tsx` → `invoke('create_wallet', { passphrase })`
#[tauri::command]
pub fn create_wallet(
    state: State<AppState>,
    passphrase: String,
    wallet_path: String,
) -> Result<WalletInfo> {
    if passphrase.len() < 8 {
        return Err(TauriError::InvalidInput(
            "Passphrase must be at least 8 characters".into(),
        ));
    }

    let path = std::path::PathBuf::from(&wallet_path);
    let wallet = Wallet::create(&passphrase).map_err(TauriError::from)?;

    // Save the encrypted wallet to disk
    wallet.save(&path).map_err(TauriError::from)?;

    let default_address = wallet.default_address().map(|a| a.to_string());
    let address_count = wallet.address_count();
    let has_2fa = wallet.has_2fa();

    *state.wallet.lock().unwrap() = Some(wallet);
    *state.wallet_path.lock().unwrap() = Some(path);

    Ok(WalletInfo {
        is_unlocked: true,
        has_2fa,
        address_count,
        default_address,
        wallet_version: 2,
    })
}

/// Load and unlock an existing wallet.dat file.
///
/// Called from: `WelcomePage.tsx` → `invoke('open_wallet', { walletPath, passphrase, otpCode })`
#[tauri::command]
pub fn open_wallet(
    state: State<AppState>,
    wallet_path: String,
    passphrase: String,
    otp_code: Option<String>,
) -> Result<WalletInfo> {
    let path = std::path::PathBuf::from(&wallet_path);

    let wallet = Wallet::load(&path, &passphrase, otp_code.as_deref()).map_err(TauriError::from)?;

    let default_address = wallet.default_address().map(|a| a.to_string());
    let address_count = wallet.address_count();
    let has_2fa = wallet.has_2fa();

    *state.wallet.lock().unwrap() = Some(wallet);
    *state.wallet_path.lock().unwrap() = Some(path);

    Ok(WalletInfo {
        is_unlocked: true,
        has_2fa,
        address_count,
        default_address,
        wallet_version: 2,
    })
}

/// Lock the wallet (clear keys from memory).
///
/// Called from: `Layout.tsx` → `invoke('lock_wallet')`
#[tauri::command]
pub fn lock_wallet(state: State<AppState>) -> Result<()> {
    *state.wallet.lock().unwrap() = None;
    Ok(())
}

/// Get the current wallet status without unlocking.
///
/// Called from: `App.tsx` on startup → `invoke('get_wallet_info')`
#[tauri::command]
pub fn get_wallet_info(state: State<AppState>) -> Result<WalletInfo> {
    let guard = state.wallet.lock().unwrap();
    match &*guard {
        Some(wallet) => Ok(WalletInfo {
            is_unlocked: true,
            has_2fa: wallet.has_2fa(),
            address_count: wallet.address_count(),
            default_address: wallet.default_address().map(|a| a.to_string()),
            wallet_version: 2,
        }),
        None => Ok(WalletInfo {
            is_unlocked: false,
            has_2fa: false,
            address_count: 0,
            default_address: None,
            wallet_version: 0,
        }),
    }
}

// ─── Legacy wallet import commands ──────────────────────────────────────────

/// Parse and extract keys from a legacy vTorrent wallet.dat file.
///
/// This is the core of the wallet migration feature. The wallet.dat bytes
/// are passed as a base64 string from the frontend (since Tauri IPC uses JSON).
///
/// Called from: `ImportWizardPage.tsx` → `invoke('import_legacy_wallet', { walletDatBase64, passphrase })`
///
/// Security guarantees:
/// - Private keys are extracted in Rust and NEVER sent to JavaScript
/// - Only addresses and metadata are returned to the frontend
/// - The extracted keys are immediately imported into the new wallet
#[tauri::command]
pub fn import_legacy_wallet(
    state: State<AppState>,
    wallet_dat_base64: String,
    passphrase: Option<String>,
    new_wallet_passphrase: String,
    new_wallet_path: String,
) -> Result<ImportResult> {
    // Decode the base64 wallet.dat bytes
    let wallet_bytes = B64
        .decode(&wallet_dat_base64)
        .map_err(|e| TauriError::InvalidInput(format!("Invalid base64: {}", e)))?;

    // The new wallet must be encrypted with a real passphrase, never a
    // hardcoded fallback.
    if new_wallet_passphrase.is_empty() {
        return Err(TauriError::InvalidInput(
            "A new wallet passphrase is required".into(),
        ));
    }

    // Extract keys from the legacy wallet.dat using the vtorrent-migrate crate
    let extraction =
        extract_wallet(&wallet_bytes, passphrase.as_deref()).map_err(TauriError::from)?;

    if extraction.keys.is_empty() {
        return Err(TauriError::Migration("No keys found in wallet.dat".into()));
    }

    // Create a new wallet and import the extracted keys
    let path = std::path::PathBuf::from(&new_wallet_path);
    let mut new_wallet = Wallet::create(&new_wallet_passphrase).map_err(TauriError::from)?;

    // Import each extracted key into the new wallet
    let mut addresses = Vec::new();
    for key in &extraction.keys {
        let addr = new_wallet
            .import_wif(&key.wif, Some(&key.legacy_address))
            .map_err(TauriError::from)?;
        addresses.push(addr.to_string());
    }

    // Save the new wallet to disk
    new_wallet.save(&path).map_err(TauriError::from)?;

    // Look up claimable balances from the snapshot
    let claimable_balance = lookup_snapshot_balances(&addresses);

    // Store the wallet in app state
    *state.wallet.lock().unwrap() = Some(new_wallet);
    *state.wallet_path.lock().unwrap() = Some(path);

    Ok(ImportResult {
        keys_found: extraction.keys.len(),
        addresses,
        had_encryption: extraction.was_encrypted,
        had_2fa: extraction.had_2fa,
        claimable_balance,
    })
}

/// The embedded UTXO snapshot binary (2.4 MB, 59,375 entries, sorted for binary search).
///
/// Format: 4-byte magic "VTRS" + 4-byte version + 4-byte count +
///         N * (34-byte null-padded address + 8-byte u64 satoshi balance)
static UTXO_SNAPSHOT: &[u8] = include_bytes!("../utxo_snapshot.bin");

const SNAPSHOT_MAGIC: &[u8; 4] = b"VTRS";
const SNAPSHOT_HEADER_SIZE: usize = 12;
const ADDR_LEN: usize = 34;
const ENTRY_SIZE: usize = ADDR_LEN + 8; // 42 bytes per entry

/// Look up balances in the embedded UTXO snapshot for a list of legacy addresses.
///
/// Uses binary search on the sorted snapshot for O(log n) lookup per address.
fn lookup_snapshot_balances(addresses: &[String]) -> u64 {
    if UTXO_SNAPSHOT.len() < SNAPSHOT_HEADER_SIZE {
        return 0;
    }
    if &UTXO_SNAPSHOT[0..4] != SNAPSHOT_MAGIC {
        return 0;
    }

    let entry_count = u32::from_le_bytes([
        UTXO_SNAPSHOT[8],
        UTXO_SNAPSHOT[9],
        UTXO_SNAPSHOT[10],
        UTXO_SNAPSHOT[11],
    ]) as usize;

    let entries_start = SNAPSHOT_HEADER_SIZE;
    let entries_end = entries_start + entry_count * ENTRY_SIZE;
    if UTXO_SNAPSHOT.len() < entries_end {
        return 0;
    }
    let entries = &UTXO_SNAPSHOT[entries_start..entries_end];

    let mut total = 0u64;
    for address in addresses {
        let addr_bytes = address.as_bytes();
        if addr_bytes.len() > ADDR_LEN {
            continue;
        }

        let mut lo = 0usize;
        let mut hi = entry_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry_offset = mid * ENTRY_SIZE;
            let entry_addr = &entries[entry_offset..entry_offset + ADDR_LEN];

            let stored_len = entry_addr.iter().position(|&b| b == 0).unwrap_or(ADDR_LEN);
            let stored_addr = &entry_addr[..stored_len];

            match stored_addr.cmp(addr_bytes) {
                std::cmp::Ordering::Equal => {
                    let bal_offset = entry_offset + ADDR_LEN;
                    let bal = u64::from_le_bytes([
                        entries[bal_offset],
                        entries[bal_offset + 1],
                        entries[bal_offset + 2],
                        entries[bal_offset + 3],
                        entries[bal_offset + 4],
                        entries[bal_offset + 5],
                        entries[bal_offset + 6],
                        entries[bal_offset + 7],
                    ]);
                    total = total.saturating_add(bal);
                    break;
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
    }
    total
}

// ─── Address management commands ─────────────────────────────────────────────

/// Generate a new receiving address in the current wallet.
///
/// Called from: `DashboardPage.tsx` → `invoke('generate_address', { label })`
#[tauri::command]
pub fn generate_address(state: State<AppState>, label: Option<String>) -> Result<AddressInfo> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    let address = wallet
        .generate_key(label.as_deref())
        .map_err(TauriError::from)?;

    // A newly generated address has no UTXOs yet — balance is always 0.
    Ok(AddressInfo {
        address: address.to_string(),
        label: label.unwrap_or_else(|| "New Address".into()),
        balance: 0,
        is_legacy_import: false,
    })
}

/// Get all addresses in the current wallet.
///
/// When the node is running, balances are queried live from the chain.
/// Otherwise falls back to the wallet's stored balances (always 0 for
/// addresses that haven't been synced yet).
///
/// Called from: `DashboardPage.tsx` → `invoke('get_addresses')`
#[tauri::command]
pub async fn get_addresses(state: tauri::State<'_, AppState>) -> Result<Vec<AddressInfo>> {
    // Read wallet addresses (synchronous lock).
    let wallet_addrs: Vec<(String, String, u64, bool)> = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        wallet.list_addresses()
    };

    // If the node is running, query the chain for live balances.
    let chain_guard = state.node.lock().await;
    let chain = chain_guard.as_ref().map(|h| h.rpc_state.chain.clone());
    drop(chain_guard);

    let balances: Vec<u64> = if let Some(chain) = chain {
        let chain = chain.lock().await;
        wallet_addrs
            .iter()
            .map(|(addr, _, _, _)| {
                chain
                    .get_utxos_for_address(addr)
                    .iter()
                    .map(|u| u.value)
                    .sum()
            })
            .collect()
    } else {
        wallet_addrs.iter().map(|(_, _, bal, _)| *bal).collect()
    };

    Ok(wallet_addrs
        .into_iter()
        .zip(balances)
        .map(
            |((address, label, _, is_legacy_import), balance)| AddressInfo {
                address,
                label,
                balance,
                is_legacy_import,
            },
        )
        .collect())
}

// ─── 2FA commands ─────────────────────────────────────────────────────────────

/// Enable TOTP 2FA on the current wallet.
///
/// Returns the TOTP URI and base32 secret for QR code display.
/// Called from: `SecurityCenterPage.tsx` → `invoke('enable_2fa')`
#[tauri::command]
pub fn enable_2fa(state: State<AppState>) -> Result<Enable2FAResult> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    let config = wallet.enable_2fa().map_err(TauriError::from)?;

    Ok(Enable2FAResult {
        uri: config.to_uri("vTorrent-Wallet"),
        secret: config.secret_base32(),
        qr_data: config.to_uri("vTorrent-Wallet"),
    })
}

/// Verify a TOTP code against the wallet's 2FA secret.
///
/// Called from: `SecurityCenterPage.tsx` → `invoke('verify_2fa', { code })`
#[tauri::command]
pub fn verify_2fa(state: State<AppState>, code: String) -> Result<bool> {
    let guard = state.wallet.lock().unwrap();
    let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;

    let valid = wallet.verify_2fa(&code).map_err(TauriError::from)?;
    Ok(valid)
}

/// Disable 2FA after verifying the current OTP code.
///
/// Called from: `SecurityCenterPage.tsx` → `invoke('disable_2fa', { code })`
#[tauri::command]
pub fn disable_2fa(state: State<AppState>, code: String) -> Result<()> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    if !wallet.verify_2fa(&code).map_err(TauriError::from)? {
        return Err(TauriError::TwoFAFailed);
    }

    wallet.disable_2fa().map_err(TauriError::from)?;
    Ok(())
}

// ─── Node lifecycle + query commands ─────────────────────────────────────────

/// Response type for node info queries.
#[derive(Debug, Serialize)]
pub struct NodeInfoResult {
    pub running: bool,
    pub version: String,
    pub network: String,
    pub block_height: u64,
    pub best_hash: String,
    pub connections: usize,
    pub syncing: bool,
    pub sync_percent: f64,
    pub mempool_size: usize,
    pub uptime_secs: u64,
}

/// Response type for a single transaction.
#[derive(Debug, Serialize)]
pub struct TxResult {
    pub txid: String,
    pub block_height: u32,
    pub confirmations: u32,
    pub timestamp: u32,
    pub tx_type: String,
    pub amount_satoshis: u64,
    pub fee_satoshis: u64,
    pub display: String,
}

/// Response type for a single torrent session.
#[derive(Debug, Serialize)]
pub struct TorrentResult {
    pub id: String,
    pub name: String,
    pub info_hash: String,
    pub state: String,
    pub progress: f64,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub peer_count: usize,
    pub vtr_earned_satoshis: u64,
    pub vtr_paid_satoshis: u64,
}

#[derive(Debug, Serialize)]
pub struct AddTorrentResult {
    pub session_id: String,
    pub info_hash: String,
    pub name: String,
}

/// Response type for a single DEX order.
#[derive(Debug, Serialize)]
pub struct DexOrderResult {
    pub id: String,
    pub maker_address: String,
    pub offer_amount_satoshis: u64,
    pub offer_asset: String,
    pub request_amount_satoshis: u64,
    pub request_asset: String,
    pub rate: f64,
    pub status: String,
    pub created_at: u32,
    pub expires_at: u32,
}

/// Start the embedded vTorrent node in a background task.
///
/// Called from: `App.tsx` on startup → `invoke('start_node')`
///
/// Spawns the node as a tokio background task and stores the shared RPC
/// AppState in `AppState::node` so subsequent commands can query it.
#[tauri::command]
pub async fn start_node(state: tauri::State<'_, AppState>) -> Result<NodeInfoResult> {
    use crate::state::NodeHandle;
    use vtorrent_node::node::{Node, NodeConfig};
    use vtorrent_rpc::state::AppState as RpcAppState;

    // If node is already running, just return current info.
    {
        let guard = state.node.lock().await;
        if guard.is_some() {
            drop(guard);
            return get_node_info(state).await;
        }
    }

    // Create the node with default config.
    let config = NodeConfig::default();
    let mut node = Node::new(config).map_err(|e| TauriError::NodeError(e.to_string()))?;

    // Create the shared RPC AppState using the node's chain and mempool Arcs.
    let mut rpc_state = RpcAppState::new_with_shared(node.chain_arc(), node.mempool_arc());

    // Initialize the Bitcoin SPV wallet from the HD mnemonic, if available.
    let btc_seed = {
        let wallet_guard = state.wallet.lock().map_err(|_| TauriError::WalletLocked)?;
        wallet_guard
            .as_ref()
            .and_then(|w| w.mnemonic())
            .and_then(|m| vtorrent_wallet::hd::Mnemonic::from_phrase(m).ok())
            .and_then(|m| m.to_seed().ok())
    };
    if let Some(seed) = btc_seed {
        // Persist UTXOs to the app data directory so they survive restarts.
        let utxo_path = {
            let wp = state
                .wallet_path
                .lock()
                .map_err(|_| TauriError::WalletLocked)?;
            wp.as_ref()
                .map(|p| {
                    p.parent()
                        .unwrap_or(std::path::Path::new("."))
                        .join("btc_utxos.json")
                })
                .unwrap_or_else(|| std::env::temp_dir().join("vtorrent_btc_utxos.json"))
        };
        match vtorrent_btc::wallet::BtcWallet::with_persistence(
            seed,
            bitcoin::Network::Bitcoin,
            utxo_path.clone(),
        ) {
            Ok(mut wallet) => {
                // Advance the derivation index to skip any already-used addresses.
                wallet.set_last_scanned_height(wallet.best_height());
                *rpc_state.btc_wallet.write().await = Some(wallet);
                tracing::info!(
                    "BTC wallet loaded with UTXO persistence: {}",
                    utxo_path.display()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to load BTC UTXOs from {}: {}, using fresh wallet",
                    utxo_path.display(),
                    e
                );
                *rpc_state.btc_wallet.write().await =
                    Some(vtorrent_btc::wallet::BtcWallet::new(seed));
            }
        }
    }

    // Wire the node's event sender to the RPC broadcaster, and track staking
    // counters (blocks_staked / last_stake_time) from node events.
    let (event_tx, mut node_rx) = vtorrent_node::events::channel(1024);
    node.set_event_sender(event_tx);

    // Capture the staking control channel so RPC state can enable/disable
    // staking at runtime (before `node` is moved into the event loop task).
    rpc_state.staking_control = Some(node.staking_control());

    // Store the NodeHandle before spawning.
    let handle = NodeHandle {
        rpc_state: rpc_state.clone(),
        start_time: std::time::Instant::now(),
    };
    *state.node.lock().await = Some(handle);

    // Bridge node events → staking counters.
    {
        let blocks_staked = Arc::clone(&rpc_state.blocks_staked);
        let last_stake_time = Arc::clone(&rpc_state.last_stake_time);
        let rewards_earned = Arc::clone(&rpc_state.rewards_earned_sats);
        tokio::spawn(async move {
            loop {
                match node_rx.recv().await {
                    Ok(event) => {
                        if let vtorrent_node::events::NodeEvent::StakingReward {
                            reward_sats, ..
                        } = &*event
                        {
                            *blocks_staked.write().await += 1;
                            *last_stake_time.write().await = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                as u32;
                            let current = *rewards_earned.read().await;
                            *rewards_earned.write().await = current.saturating_add(*reward_sats);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // Spawn the node event loop as a background task.
    tokio::spawn(async move {
        if let Err(e) = node.start().await {
            tracing::error!("Node stopped with error: {}", e);
        }
    });

    tracing::info!("vTorrent node started in background");
    get_node_info(state).await
}

/// Get current node status.
///
/// Called from: `DashboardPage.tsx`, `Layout.tsx` → `invoke('get_node_info')`
#[tauri::command]
pub async fn get_node_info(state: tauri::State<'_, AppState>) -> Result<NodeInfoResult> {
    let guard = state.node.lock().await;
    match &*guard {
        None => Ok(NodeInfoResult {
            running: false,
            version: env!("CARGO_PKG_VERSION").into(),
            network: "vtorrent-mainnet".into(),
            block_height: 0,
            best_hash: String::new(),
            connections: 0,
            syncing: false,
            sync_percent: 0.0,
            mempool_size: 0,
            uptime_secs: 0,
        }),
        Some(handle) => {
            let rpc = &handle.rpc_state;
            let chain = rpc.chain.lock().await;
            let mempool = rpc.mempool.lock().await;
            let peer_count = *rpc.peer_count.read().await;
            let syncing = *rpc.syncing.read().await;
            let uptime = handle.start_time.elapsed().as_secs();
            let height = chain.best_height();
            let blocks = height as f64;
            let total = blocks.max(1.0);
            let sync_pct = if syncing {
                (blocks / total) * 100.0
            } else {
                100.0
            };
            Ok(NodeInfoResult {
                running: true,
                version: env!("CARGO_PKG_VERSION").into(),
                block_height: height as u64,
                best_hash: chain.best_hash().map(hex::encode).unwrap_or_default(),
                connections: peer_count,
                syncing,
                sync_percent: sync_pct,
                mempool_size: mempool.size(),
                uptime_secs: uptime,
                network: rpc.network.clone(),
            })
        }
    }
}

/// Get recent wallet transactions.
///
/// Called from: `DashboardPage.tsx` → `invoke('get_transactions', { limit })`
#[tauri::command]
pub async fn get_transactions(
    state: tauri::State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<TxResult>> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let chain = handle.rpc_state.chain.lock().await;
    let limit = limit.unwrap_or(50);
    let txs = chain.get_recent_transactions(limit);
    let best = chain.best_height();
    Ok(txs
        .into_iter()
        .map(|(txid, height, ts, dir, amount, fee)| {
            let display = format!(
                "{} {} VTR",
                if dir == "receive" { "Received" } else { "Sent" },
                amount as f64 / 100_000_000.0
            );
            TxResult {
                txid,
                block_height: height,
                confirmations: best.saturating_sub(height),
                timestamp: ts,
                tx_type: dir,
                amount_satoshis: amount,
                fee_satoshis: fee,
                display,
            }
        })
        .collect())
}

/// Get active torrent sessions.
///
/// Called from: `TorrentPage.tsx` → `invoke('get_torrent_sessions')`
#[tauri::command]
pub async fn get_torrent_sessions(state: tauri::State<'_, AppState>) -> Result<Vec<TorrentResult>> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let sessions = handle.rpc_state.torrent_sessions.read().await;
    Ok(sessions
        .list_sessions()
        .into_iter()
        .map(|s| TorrentResult {
            id: s.id.clone(),
            name: s.metainfo.name.clone(),
            info_hash: hex::encode(s.metainfo.info_hash),
            state: format!("{:?}", s.state),
            progress: s.progress(),
            size_bytes: s.metainfo.total_size,
            downloaded_bytes: s.bytes_downloaded,
            uploaded_bytes: s.bytes_uploaded,
            download_speed: s.download_speed,
            upload_speed: s.upload_speed,
            peer_count: s.peers.len(),
            vtr_earned_satoshis: s
                .incentive_accounts
                .values()
                .map(|a| a.total_earned_satoshis)
                .sum(),
            vtr_paid_satoshis: 0,
        })
        .collect())
}

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
        let bytes = B64
            .decode(&source)
            .map_err(|e| TauriError::Torrent(e.to_string()))?;
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

/// Get the DEX order book.
///
/// Called from: `TradePage.tsx` → `invoke('get_dex_orders')`
#[tauri::command]
pub async fn get_dex_orders(state: tauri::State<'_, AppState>) -> Result<Vec<DexOrderResult>> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let order_book = handle.rpc_state.order_book.read().await;
    Ok(order_book
        .list_open_orders()
        .into_iter()
        .map(|o| DexOrderResult {
            id: hex::encode(o.order_id),
            maker_address: o.maker_address.clone(),
            offer_amount_satoshis: o.vtr_amount,
            offer_asset: "VTR".into(),
            request_amount_satoshis: o.target_amount,
            request_asset: o.target_asset.clone(),
            rate: if o.target_amount > 0 {
                o.vtr_amount as f64 / o.target_amount as f64
            } else {
                0.0
            },
            status: format!("{:?}", o.status),
            created_at: 0,
            expires_at: o.expiry,
        })
        .collect())
}

/// Place a DEX order.
///
/// Called from: `TradePage.tsx` → `invoke('place_dex_order', { makerAddress, makerBtcAddress, vtrAmount, targetAsset, targetAmount })`
#[tauri::command]
pub async fn place_dex_order(
    state: tauri::State<'_, AppState>,
    maker_address: String,
    maker_btc_address: Option<String>,
    vtr_amount: u64,
    target_asset: String,
    target_amount: u64,
) -> Result<DexOrderResult> {
    use vtorrent_node::atomic_swap::SwapOrder;

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let mut order_book = handle.rpc_state.order_book.write().await;

    // Default locktime: 24 hours.
    let mut order = SwapOrder::new(
        maker_address,
        vtr_amount,
        target_asset,
        target_amount,
        86400,
    );
    order.maker_btc_address = maker_btc_address;
    let result = DexOrderResult {
        id: hex::encode(order.order_id),
        maker_address: order.maker_address.clone(),
        offer_amount_satoshis: order.vtr_amount,
        offer_asset: "VTR".into(),
        request_amount_satoshis: order.target_amount,
        request_asset: order.target_asset.clone(),
        rate: if order.target_amount > 0 {
            order.vtr_amount as f64 / order.target_amount as f64
        } else {
            0.0
        },
        status: format!("{:?}", order.status),
        created_at: 0,
        expires_at: order.expiry,
    };
    order_book.add_order(order);
    Ok(result)
}

/// Cancel a DEX order.
///
/// Called from: `TradePage.tsx` → `invoke('cancel_dex_order', { orderId })`
#[tauri::command]
pub async fn cancel_dex_order(state: tauri::State<'_, AppState>, order_id: String) -> Result<bool> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let mut order_book = handle.rpc_state.order_book.write().await;
    Ok(order_book.cancel_order(&order_id))
}

// ─── Swap lifecycle commands ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SwapActionResult {
    pub order_id: String,
    pub txid: String,
    pub status: String,
}

/// Match an order as the taker, funding the maker's VTR HTLC.
#[tauri::command]
pub async fn match_dex_order(
    state: tauri::State<'_, AppState>,
    order_id: String,
    taker_address: String,
    _passphrase: String,
    _otp_code: Option<String>,
) -> Result<vtorrent_rpc::models::MatchOrderResponse> {
    use vtorrent_node::atomic_swap::{AtomicSwap, Htlc, MIN_HTLC_LOCKTIME};
    use vtorrent_wallet::tx_builder::sign_custom_transaction;

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    if !rpc.is_wallet_unlocked().await {
        return Err(TauriError::WalletLocked);
    }
    if taker_address.trim().is_empty() {
        return Err(TauriError::InvalidInput("Taker address is required".into()));
    }
    vtorrent_wallet::tx_builder::p2pkh_script_pubkey(&taker_address)
        .map_err(|e| TauriError::InvalidInput(format!("Invalid taker address: {}", e)))?;

    let wif = rpc
        .wallet_wif
        .read()
        .await
        .clone()
        .ok_or(TauriError::WalletLocked)?;
    let wallet_address = rpc
        .wallet_change_address
        .read()
        .await
        .clone()
        .ok_or_else(|| TauriError::Internal("Change address not set".into()))?;

    let order = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .filter(|o| matches!(o.status, vtorrent_node::atomic_swap::OrderStatus::Open))
            .cloned()
            .ok_or_else(|| {
                TauriError::NotFound(format!("Order {} not found or not open", order_id))
            })?
    };
    if order.maker_address != wallet_address {
        return Err(TauriError::Unauthorized(
            "Only the maker's imported wallet may fund this order".into(),
        ));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    let remaining_locktime = order.expiry.saturating_sub(now);
    if remaining_locktime < MIN_HTLC_LOCKTIME {
        return Err(TauriError::InvalidInput(
            "DEX order is too close to expiry to fund safely".into(),
        ));
    }
    let (preimage, hash_lock) = match (order.preimage, order.hash_lock) {
        (Some(p), Some(h)) => (p, h),
        _ => {
            let swap = AtomicSwap::new();
            (swap.preimage, swap.hash_lock)
        }
    };
    let htlc = Htlc::new(
        hash_lock,
        taker_address.clone(),
        order.maker_address.clone(),
        remaining_locktime,
        order.vtr_amount,
    )
    .map_err(|e| TauriError::InvalidInput(format!("Unable to construct HTLC: {}", e)))?;

    const FUNDING_FEE_SATOSHIS: u64 = 10_000;
    let funding_utxo = {
        let chain = rpc.chain.lock().await;
        chain
            .get_utxos_for_address(&wallet_address)
            .into_iter()
            .filter(|utxo| utxo.value >= order.vtr_amount.saturating_add(FUNDING_FEE_SATOSHIS))
            .max_by_key(|utxo| utxo.value)
            .ok_or_else(|| {
                TauriError::InvalidInput("No single wallet UTXO can fund this HTLC".into())
            })?
    };
    let unsigned_funding = htlc
        .build_funding_tx(
            funding_utxo.txid,
            funding_utxo.vout,
            funding_utxo.value,
            FUNDING_FEE_SATOSHIS,
        )
        .map_err(|e| TauriError::InvalidInput(format!("Unable to build HTLC funding tx: {}", e)))?;
    let funding_tx =
        sign_custom_transaction(unsigned_funding, std::slice::from_ref(&funding_utxo), &wif)
            .map_err(|e| {
                TauriError::InvalidInput(format!("Unable to sign HTLC funding tx: {}", e))
            })?;
    let funding_txid = funding_tx.txid();

    let reserved = rpc.order_book.write().await.begin_funding(&order_id);
    if reserved.is_none() {
        return Err(TauriError::NotFound(format!(
            "Order {} is no longer open",
            order_id
        )));
    }
    let admission = {
        let mut mempool = rpc.mempool.lock().await;
        mempool.add_transaction_with_fee(funding_tx.clone(), FUNDING_FEE_SATOSHIS)
    };
    if let Err(e) = admission {
        rpc.order_book.write().await.release_funding(&order_id);
        return Err(TauriError::InvalidInput(format!(
            "Mempool rejected HTLC funding transaction: {}",
            e
        )));
    }
    let matched = rpc
        .order_book
        .write()
        .await
        .fund_and_match_order(&order_id, taker_address, preimage, hash_lock, funding_txid)
        .ok_or_else(|| TauriError::Internal("Funding reservation disappeared".into()))?;

    if let Some(sender) = &rpc.tx_submit {
        let _ = sender.try_send(funding_tx);
    }

    Ok(vtorrent_rpc::models::MatchOrderResponse {
        order_id: hex::encode(matched.order.order_id),
        maker_address: matched.order.maker_address,
        vtr_amount: matched.order.vtr_amount,
        target_asset: matched.order.target_asset,
        target_amount: matched.order.target_amount,
        hash_lock: hex::encode(matched.hash_lock),
        expiry: matched.order.expiry,
        funding_txid: hex::encode(funding_txid),
    })
}

/// Fund the BTC side of the HTLC as the taker.
#[tauri::command]
pub async fn btc_fund(
    state: tauri::State<'_, AppState>,
    order_id: String,
    btc_refund_address: String,
) -> Result<SwapActionResult> {
    use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    let order = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .cloned()
            .ok_or_else(|| TauriError::NotFound(format!("Order {} not found", order_id)))?
    };
    let hash_lock = order
        .hash_lock
        .ok_or_else(|| TauriError::InvalidInput("Order has no hash lock".into()))?;
    let maker_btc_address = order
        .maker_btc_address
        .clone()
        .ok_or_else(|| TauriError::InvalidInput("Order has no maker BTC address".into()))?;
    let btc_amount = order.target_amount;
    if btc_amount == 0 {
        return Err(TauriError::InvalidInput(
            "Order target amount is zero".into(),
        ));
    }

    let htlc = vtorrent_btc::htlc::BtcHtlc::new(
        hash_lock,
        maker_btc_address.clone(),
        btc_refund_address.clone(),
        vtorrent_btc::htlc::DEFAULT_HTLC_LOCKTIME,
        btc_amount,
    )
    .map_err(|e| TauriError::InvalidInput(format!("Unable to construct BTC HTLC: {}", e)))?;

    const FUNDING_FEE_SATOSHIS: u64 = 1_000;
    let (funding_utxo, funder_wif, change_address) = {
        let btc = rpc.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| TauriError::InvalidInput("BTC wallet not initialized".into()))?;
        let utxos = w.list_utxos();
        let selected = utxos
            .iter()
            .filter(|u| u.value >= btc_amount + FUNDING_FEE_SATOSHIS)
            .max_by_key(|u| u.value)
            .cloned()
            .ok_or_else(|| TauriError::InvalidInput("Insufficient BTC funds".into()))?;
        let wif = w
            .derive_wif(0)
            .map_err(|e| TauriError::Internal(e.to_string()))?;
        let change = w
            .current_address()
            .map_err(|e| TauriError::Internal(e.to_string()))?;
        (selected, wif, change)
    };

    let funding_txid_bytes: [u8; 32] = {
        use bitcoin::hashes::Hash;
        funding_utxo
            .txid
            .parse::<bitcoin::Txid>()
            .map(|t| t.to_byte_array())
            .map_err(|e| TauriError::InvalidInput(format!("Invalid UTXO txid: {}", e)))?
    };
    let unsigned = htlc
        .build_funding_tx(
            funding_txid_bytes,
            funding_utxo.vout,
            funding_utxo.value,
            FUNDING_FEE_SATOSHIS,
            &change_address,
        )
        .map_err(|e| TauriError::InvalidInput(format!("Unable to build BTC funding tx: {}", e)))?;
    let signed = htlc
        .sign_funding_tx(unsigned, funding_utxo.value, &funder_wif)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to sign BTC funding tx: {}", e)))?;
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let btc_funding_txid = {
        use bitcoin::hashes::Hash;
        signed.compute_txid().to_byte_array()
    };
    vtorrent_btc::sync::broadcast_tx(&raw)
        .await
        .map_err(|e| TauriError::Internal(format!("BTC broadcast failed: {}", e)))?;

    let mut swaps = rpc.swaps.write().await;
    let swap = swaps
        .entry(order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.btc_funding_txid = Some(btc_funding_txid);
    swap.maker_btc_address = Some(maker_btc_address);
    swap.taker_btc_refund_address = Some(btc_refund_address);
    swap.btc_amount = btc_amount;
    swap.btc_expiry = htlc.expiry;
    swap.status = SwapStatus::BtcFunded;

    Ok(SwapActionResult {
        order_id,
        txid: hex::encode(btc_funding_txid),
        status: "BtcFunded".to_string(),
    })
}

/// Claim VTR by revealing the preimage (taker).
#[tauri::command]
pub async fn vtr_claim(
    state: tauri::State<'_, AppState>,
    order_id: String,
    preimage: String,
    taker_wif: String,
) -> Result<SwapActionResult> {
    use vtorrent_node::atomic_swap::{Htlc, SwapState, SwapStatus};
    use vtorrent_wallet::tx_builder::sign_input_over_subscript;

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    let preimage_bytes = {
        let bytes = hex::decode(&preimage)
            .map_err(|_| TauriError::InvalidInput("Invalid preimage hex".into()))?;
        if bytes.len() != 32 {
            return Err(TauriError::InvalidInput("Preimage must be 32 bytes".into()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    };
    if taker_wif.is_empty() {
        return Err(TauriError::InvalidInput("Taker WIF is required".into()));
    }

    let order = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .cloned()
            .ok_or_else(|| TauriError::NotFound(format!("Order {} not found", order_id)))?
    };
    let hash_lock = order
        .hash_lock
        .ok_or_else(|| TauriError::InvalidInput("Order has no hash lock".into()))?;
    let funding_txid = order
        .funding_txid
        .ok_or_else(|| TauriError::InvalidInput("Order has no VTR funding txid".into()))?;
    let taker_address = order
        .taker_address
        .clone()
        .ok_or_else(|| TauriError::InvalidInput("Order has no taker address".into()))?;

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(preimage_bytes);
    let digest = hasher.finalize();
    if digest.as_slice() != hash_lock {
        return Err(TauriError::InvalidInput(
            "Preimage does not match hash lock".into(),
        ));
    }

    let htlc = Htlc::with_expiry(
        hash_lock,
        taker_address.clone(),
        order.maker_address.clone(),
        order.expiry,
        order.vtr_amount,
    )
    .map_err(|e| TauriError::InvalidInput(format!("Unable to reconstruct HTLC: {}", e)))?;

    const CLAIM_FEE_SATOSHIS: u64 = 10_000;
    let unsigned = htlc
        .build_claim_tx_unsigned(funding_txid, &preimage_bytes, CLAIM_FEE_SATOSHIS)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to build VTR claim tx: {}", e)))?;

    let htlc_script = htlc
        .build_script()
        .map_err(|e| TauriError::InvalidInput(format!("Invalid HTLC addresses: {}", e)))?;
    let (sig, pubkey) = sign_input_over_subscript(&unsigned, 0, &htlc_script, &taker_wif)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to sign VTR claim tx: {}", e)))?;

    let mut script_sig = Vec::new();
    script_sig.push(sig.len() as u8);
    script_sig.extend_from_slice(&sig);
    script_sig.push(pubkey.len() as u8);
    script_sig.extend_from_slice(&pubkey);
    script_sig.push(0x20);
    script_sig.extend_from_slice(&preimage_bytes);
    script_sig.push(0x51); // OP_1

    let mut claim_tx = unsigned;
    claim_tx.inputs[0].script_sig = script_sig;
    let claim_txid = claim_tx.txid();

    {
        let mut mempool = rpc.mempool.lock().await;
        mempool
            .add_transaction_with_fee(claim_tx.clone(), CLAIM_FEE_SATOSHIS)
            .map_err(|e| {
                TauriError::InvalidInput(format!("Mempool rejected VTR claim tx: {}", e))
            })?;
    }
    if let Some(sender) = &rpc.tx_submit {
        let _ = sender.try_send(claim_tx);
    }

    let mut swaps = rpc.swaps.write().await;
    let swap = swaps
        .entry(order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.preimage = Some(preimage_bytes);
    swap.status = SwapStatus::Claimed;

    Ok(SwapActionResult {
        order_id,
        txid: hex::encode(claim_txid),
        status: "Claimed".to_string(),
    })
}

/// Claim BTC using the revealed preimage (maker).
#[tauri::command]
pub async fn btc_claim(
    state: tauri::State<'_, AppState>,
    order_id: String,
) -> Result<SwapActionResult> {
    use vtorrent_node::atomic_swap::SwapStatus;

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    let (preimage, btc_funding_txid, maker_btc_address, btc_amount, btc_expiry, refund_address) = {
        let swaps = rpc.swaps.read().await;
        let swap = swaps
            .get(&order_id)
            .ok_or_else(|| TauriError::NotFound(format!("Swap {} not found", order_id)))?;
        let preimage = match swap.preimage {
            Some(p) => p,
            None => {
                let order_book = rpc.order_book.read().await;
                order_book
                    .get_order(&order_id)
                    .and_then(|o| o.preimage)
                    .ok_or_else(|| TauriError::InvalidInput("Preimage not available".into()))?
            }
        };
        let btc_funding_txid = swap
            .btc_funding_txid
            .ok_or_else(|| TauriError::InvalidInput("BTC funding txid not recorded".into()))?;
        let maker_btc_address = swap
            .maker_btc_address
            .clone()
            .ok_or_else(|| TauriError::InvalidInput("Maker BTC address not recorded".into()))?;
        let refund_address = swap.taker_btc_refund_address.clone().ok_or_else(|| {
            TauriError::InvalidInput("Taker BTC refund address not recorded".into())
        })?;
        (
            preimage,
            btc_funding_txid,
            maker_btc_address,
            swap.btc_amount,
            swap.btc_expiry,
            refund_address,
        )
    };

    let htlc = vtorrent_btc::htlc::BtcHtlc {
        hash_lock: {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(preimage);
            let d = h.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&d);
            out
        },
        recipient: maker_btc_address.clone(),
        refund_address,
        expiry: btc_expiry,
        amount: btc_amount,
        network: *rpc.btc_network.read().await,
    };
    const CLAIM_FEE_SATOSHIS: u64 = 1_000;
    let unsigned = htlc
        .build_claim_tx(btc_funding_txid, &preimage, CLAIM_FEE_SATOSHIS)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to build BTC claim tx: {}", e)))?;
    let maker_wif = {
        let btc = rpc.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| TauriError::InvalidInput("BTC wallet not initialized".into()))?;
        w.derive_wif(0)
            .map_err(|e| TauriError::Internal(e.to_string()))?
    };
    let signed = htlc
        .sign_claim_tx(unsigned, &preimage, &maker_wif)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to sign BTC claim tx: {}", e)))?;
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let txid = {
        use bitcoin::hashes::Hash;
        signed.compute_txid().to_byte_array()
    };
    vtorrent_btc::sync::broadcast_tx(&raw)
        .await
        .map_err(|e| TauriError::Internal(format!("BTC broadcast failed: {}", e)))?;

    {
        let mut swaps = rpc.swaps.write().await;
        if let Some(swap) = swaps.get_mut(&order_id) {
            swap.status = SwapStatus::Claimed;
        }
    }

    Ok(SwapActionResult {
        order_id,
        txid: hex::encode(txid),
        status: "Claimed".to_string(),
    })
}

/// Refund either side after expiry.
#[tauri::command]
pub async fn swap_refund(
    state: tauri::State<'_, AppState>,
    order_id: String,
) -> Result<SwapActionResult> {
    use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    let order = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .cloned()
            .ok_or_else(|| TauriError::NotFound(format!("Order {} not found", order_id)))?
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    if now < order.expiry {
        return Err(TauriError::InvalidInput("Swap has not expired yet".into()));
    }

    let btc_refund_txid = {
        let swaps = rpc.swaps.read().await;
        let swap = swaps.get(&order_id);
        match swap {
            Some(s) if s.btc_funding_txid.is_some() && s.btc_expiry > 0 => {
                let funding_txid = s.btc_funding_txid.unwrap();
                let refund_address = s.taker_btc_refund_address.clone().ok_or_else(|| {
                    TauriError::InvalidInput("Taker BTC refund address not recorded".into())
                })?;
                let htlc = vtorrent_btc::htlc::BtcHtlc {
                    hash_lock: s.hash_lock,
                    recipient: s.maker_btc_address.clone().unwrap_or_default(),
                    refund_address,
                    expiry: s.btc_expiry,
                    amount: s.btc_amount,
                    network: *rpc.btc_network.read().await,
                };
                const REFUND_FEE_SATOSHIS: u64 = 1_000;
                let unsigned = htlc
                    .build_refund_tx(funding_txid, REFUND_FEE_SATOSHIS)
                    .map_err(|e| {
                        TauriError::InvalidInput(format!("Unable to build BTC refund tx: {}", e))
                    })?;
                let refund_wif = {
                    let btc = rpc.btc_wallet.read().await;
                    let w = btc.as_ref().ok_or_else(|| {
                        TauriError::InvalidInput("BTC wallet not initialized".into())
                    })?;
                    w.derive_wif(0)
                        .map_err(|e| TauriError::Internal(e.to_string()))?
                };
                let signed = htlc.sign_refund_tx(unsigned, &refund_wif).map_err(|e| {
                    TauriError::InvalidInput(format!("Unable to sign BTC refund tx: {}", e))
                })?;
                let raw = bitcoin::consensus::encode::serialize(&signed);
                let txid = {
                    use bitcoin::hashes::Hash;
                    signed.compute_txid().to_byte_array()
                };
                vtorrent_btc::sync::broadcast_tx(&raw)
                    .await
                    .map_err(|e| TauriError::Internal(format!("BTC broadcast failed: {}", e)))?;
                Some(txid)
            }
            _ => None,
        }
    };

    let mut swaps = rpc.swaps.write().await;
    let swap = swaps
        .entry(order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, order.hash_lock.unwrap_or([0u8; 32])));
    swap.status = SwapStatus::Refunded;

    Ok(SwapActionResult {
        order_id: order_id.clone(),
        txid: btc_refund_txid
            .map(hex::encode)
            .unwrap_or_else(|| hex::encode(order.order_id)),
        status: "Refunded".to_string(),
    })
}

/// Send VTR to an address.
///
/// Called from: `DashboardPage.tsx` → `invoke('send_vtr', { toAddress, amountSatoshis })`
///
/// The wallet must already be open (loaded into memory). No passphrase is
/// required here because the WIF is stored in plaintext in the decrypted
/// WalletData while the wallet is unlocked.
#[tauri::command]
pub async fn send_vtr(
    state: tauri::State<'_, AppState>,
    to_address: String,
    amount_satoshis: u64,
) -> Result<String> {
    use vtorrent_wallet::tx_builder::TxBuilder;

    // Get the WIF and from-address from the unlocked wallet.
    let (wif, from_address) = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        let wif = wallet
            .get_default_wif()
            .ok_or(TauriError::WalletNotInitialized)?
            .to_string();
        let from = wallet
            .default_address()
            .ok_or(TauriError::WalletNotInitialized)?
            .to_string();
        (wif, from)
    };

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    // Collect UTXOs for the sender address.
    let utxos = {
        let chain = handle.rpc_state.chain.lock().await;
        chain.get_utxos_for_address(&from_address)
    };

    // Build and sign the transaction using the mempool's recommended fee rate.
    let fee_rate = {
        let mempool = handle.rpc_state.mempool.lock().await;
        mempool.recommended_fee_rate().max(1)
    };
    let tx = TxBuilder::new()
        .recipient(&to_address, amount_satoshis)
        .fee_rate(fee_rate)
        .change_address(&from_address)
        .sign_with_wif(&wif)
        .build(&utxos)
        .map_err(|e| TauriError::NodeError(format!("Transaction build failed: {}", e)))?;

    let txid = hex::encode(tx.txid());

    // Submit to mempool.
    {
        let mut mempool = handle.rpc_state.mempool.lock().await;
        mempool
            .add_transaction(tx)
            .map_err(|e| TauriError::NodeError(format!("Mempool rejected tx: {}", e)))?;
    }

    tracing::info!(
        "Sent {} satoshis to {} (txid: {})",
        amount_satoshis,
        to_address,
        txid
    );
    Ok(txid)
}

// ─── Staking IPC ─────────────────────────────────────────────────────────────

/// Return type for staking status queries.
#[derive(Debug, serde::Serialize)]
pub struct StakingStatusResult {
    pub enabled: bool,
    pub staking_address: Option<String>,
    pub eligible_utxos: usize,
    pub total_staking_satoshis: u64,
    pub expected_reward_per_day: u64,
    pub last_stake_time: Option<u64>,
    pub blocks_staked: u64,
}

/// Start PoS staking with the given wallet address.
///
/// Called from: `StakingPage.tsx` → `invoke('start_staking', { address })`
#[tauri::command]
pub async fn start_staking(
    state: tauri::State<'_, AppState>,
    address: String,
) -> Result<StakingStatusResult> {
    if address.is_empty() {
        return Err(TauriError::NodeError("Staking address is required".into()));
    }
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    if !handle.rpc_state.is_wallet_unlocked().await {
        return Err(TauriError::WalletLocked);
    }
    let wif = handle.rpc_state.wallet_wif.read().await.clone();

    // Drive the node's actual staking engine via the runtime control channel.
    if let Some(tx) = &handle.rpc_state.staking_control {
        let _ = tx
            .send(vtorrent_node::staking::StakingCommand::Start {
                address: address.clone(),
                wif,
            })
            .await;
    }
    *handle.rpc_state.staking_enabled.write().await = true;
    *handle.rpc_state.staking_address.write().await = Some(address.clone());

    tracing::info!("Staking started for address: {}", address);

    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    let chain = handle.rpc_state.chain.lock().await;
    let staking_utxos = chain.get_utxos_for_address(&address);
    let total_staking: u64 = staking_utxos.iter().map(|u| u.value).sum();
    let eligible_count = staking_utxos.len();
    // Expected daily reward = (total_staked * annual_rate) / 365
    let expected_reward_per_day = if total_staking > 0 {
        ((total_staking as f64 * vtorrent_node::consensus::POS_ANNUAL_RATE) / 365.0) as u64
    } else {
        0
    };
    Ok(StakingStatusResult {
        enabled: true,
        staking_address: Some(address),
        eligible_utxos: eligible_count,
        total_staking_satoshis: total_staking,
        expected_reward_per_day,
        last_stake_time: None,
        blocks_staked,
    })
}

/// Stop PoS staking.
///
/// Called from: `StakingPage.tsx` → `invoke('stop_staking')`
#[tauri::command]
pub async fn stop_staking(state: tauri::State<'_, AppState>) -> Result<StakingStatusResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    if let Some(tx) = &handle.rpc_state.staking_control {
        let _ = tx.send(vtorrent_node::staking::StakingCommand::Stop).await;
    }
    *handle.rpc_state.staking_enabled.write().await = false;
    *handle.rpc_state.staking_address.write().await = None;

    tracing::info!("Staking stopped");

    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    Ok(StakingStatusResult {
        enabled: false,
        staking_address: None,
        eligible_utxos: 0,
        total_staking_satoshis: 0,
        expected_reward_per_day: 0,
        last_stake_time: None,
        blocks_staked,
    })
}

/// Get current staking status.
///
/// Called from: `StakingPage.tsx` → `invoke('get_staking_status')`
#[tauri::command]
pub async fn get_staking_status(state: tauri::State<'_, AppState>) -> Result<StakingStatusResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    let enabled = *handle.rpc_state.staking_enabled.read().await;
    let staking_address = handle.rpc_state.staking_address.read().await.clone();
    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;

    let (eligible_utxos, total_staking_satoshis, expected_reward_per_day) = if enabled {
        if let Some(ref addr) = staking_address {
            let chain = handle.rpc_state.chain.lock().await;
            let staking_utxos = chain.get_utxos_for_address(addr);
            let total: u64 = staking_utxos.iter().map(|u| u.value).sum();
            let count = staking_utxos.len();
            let daily = if total > 0 {
                ((total as f64 * vtorrent_node::consensus::POS_ANNUAL_RATE) / 365.0) as u64
            } else {
                0
            };
            (count, total, daily)
        } else {
            (0, 0, 0)
        }
    } else {
        (0, 0, 0)
    };

    Ok(StakingStatusResult {
        enabled,
        staking_address,
        eligible_utxos,
        total_staking_satoshis,
        expected_reward_per_day,
        last_stake_time: None,
        blocks_staked,
    })
}

// ─── Bitcoin wallet commands ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct BtcStatus {
    pub initialized: bool,
    pub balance_satoshis: u64,
    pub address: Option<String>,
    pub best_height: u32,
    pub synced: bool,
}

/// Get the BTC wallet status.
#[tauri::command]
pub async fn get_btc_status(state: State<'_, AppState>) -> Result<BtcStatus> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let btc = handle.rpc_state.btc_wallet.read().await;
    match btc.as_ref() {
        None => Ok(BtcStatus {
            initialized: false,
            balance_satoshis: 0,
            address: None,
            best_height: 0,
            synced: false,
        }),
        Some(wallet) => Ok(BtcStatus {
            initialized: true,
            balance_satoshis: wallet.balance(),
            address: wallet.current_address().ok(),
            best_height: wallet.best_height(),
            synced: wallet.synced(),
        }),
    }
}

/// Get the current BTC receiving address.
#[tauri::command]
pub async fn get_btc_address(state: State<'_, AppState>) -> Result<String> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let btc = handle.rpc_state.btc_wallet.read().await;
    match btc.as_ref() {
        None => Err(TauriError::NodeError("BTC wallet not initialized".into())),
        Some(wallet) => wallet
            .current_address()
            .map_err(|e| TauriError::NodeError(e.to_string())),
    }
}

/// Build, sign, and broadcast a BTC transaction.
#[tauri::command]
pub async fn send_btc(
    state: State<'_, AppState>,
    to_address: String,
    amount_satoshis: u64,
) -> Result<String> {
    if to_address.trim().is_empty() {
        return Err(TauriError::InvalidInput(
            "Recipient address is required".into(),
        ));
    }
    if amount_satoshis == 0 {
        return Err(TauriError::InvalidInput("Amount must be non-zero".into()));
    }

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    // Build and sign the transaction, removing spent UTXOs from the wallet.
    // The selected UTXOs are kept for rollback if broadcasting fails.
    let (txid_hex, raw, spent_utxos) = {
        let mut btc = handle.rpc_state.btc_wallet.write().await;
        let wallet = btc
            .as_mut()
            .ok_or_else(|| TauriError::NodeError("BTC wallet not initialized".into()))?;
        wallet
            .send_to(&to_address, amount_satoshis, 1_000)
            .map_err(|e| TauriError::NodeError(e.to_string()))?
    };

    // Broadcast to the Bitcoin network. On failure, restore the spent UTXOs
    // so the wallet does not forget outputs for a tx that never landed.
    let broadcast_result = {
        let network = *handle.rpc_state.btc_network.read().await;
        let peer = *handle.rpc_state.btc_peer.read().await;
        if let Some(addr) = peer {
            vtorrent_btc::sync::broadcast_tx_to(&raw, network, &[addr]).await
        } else {
            vtorrent_btc::sync::broadcast_tx(&raw).await
        }
    };
    if let Err(e) = broadcast_result {
        if let Some(wallet) = handle.rpc_state.btc_wallet.write().await.as_mut() {
            wallet.restore_utxos(&spent_utxos);
        }
        return Err(TauriError::NodeError(format!(
            "BTC broadcast failed: {}",
            e
        )));
    }

    tracing::info!("BTC tx broadcast: {}", txid_hex);
    Ok(txid_hex)
}

// ─── Legacy claim commands ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ClaimCheckResult {
    pub address: String,
    pub claimable_satoshis: u64,
    pub display: String,
    pub already_claimed: bool,
}

/// Check if a legacy address has claimable balance.
#[tauri::command]
pub async fn check_legacy_claim(
    state: State<'_, AppState>,
    legacy_address: String,
) -> Result<ClaimCheckResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let chain = handle.rpc_state.chain.lock().await;
    let claimable = vtorrent_node::genesis::get_legacy_balance(&legacy_address);
    let already_claimed = chain.is_claimed(&legacy_address);
    Ok(ClaimCheckResult {
        address: legacy_address,
        claimable_satoshis: claimable,
        display: format!("{:.6} VTR", claimable as f64 / 100_000_000.0),
        already_claimed,
    })
}

#[derive(Debug, Serialize)]
pub struct ClaimSubmitResult {
    pub txid: String,
    pub claimed_satoshis: u64,
    pub recipient_address: String,
}

/// Submit a legacy claim transaction.
#[tauri::command]
pub async fn submit_legacy_claim(
    state: State<'_, AppState>,
    wif_private_key: String,
    recipient_address: String,
) -> Result<ClaimSubmitResult> {
    use vtorrent_core::keys::PrivateKey;
    use vtorrent_node::block::{Transaction, TxOutput, TxType};
    use vtorrent_node::genesis::get_legacy_balance;
    use vtorrent_wallet::tx_builder::{p2pkh_script_pubkey, pubkey_to_vtorrent_address};

    if wif_private_key.is_empty() {
        return Err(TauriError::InvalidInput(
            "WIF private key is required".into(),
        ));
    }
    if recipient_address.is_empty() {
        return Err(TauriError::InvalidInput(
            "Recipient address is required".into(),
        ));
    }

    let key = PrivateKey::from_wif(&wif_private_key)
        .map_err(|e| TauriError::InvalidInput(format!("Invalid WIF key: {}", e)))?;
    let secp = secp256k1::Secp256k1::new();
    let secret_key = secp256k1::SecretKey::from_slice(key.as_bytes())
        .map_err(|e| TauriError::InvalidInput(format!("Invalid key bytes: {}", e)))?;
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let derived_address = pubkey_to_vtorrent_address(&pubkey.serialize())
        .map_err(|e| TauriError::NodeError(e.to_string()))?;

    let claimable = get_legacy_balance(&derived_address);
    if claimable == 0 {
        return Err(TauriError::InvalidInput(format!(
            "No claimable balance for address {}",
            derived_address
        )));
    }

    let txid;
    {
        let guard = state.node.lock().await;
        let handle = guard
            .as_ref()
            .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

        {
            let chain = handle.rpc_state.chain.lock().await;
            if chain.is_claimed(&derived_address) {
                return Err(TauriError::InvalidInput(format!(
                    "Address {} has already been claimed",
                    derived_address
                )));
            }
        }

        let script_pubkey = p2pkh_script_pubkey(&recipient_address)
            .map_err(|e| TauriError::InvalidInput(format!("Invalid recipient address: {}", e)))?;

        let msg_hash = vtorrent_node::consensus::claim_message_hash(&derived_address);
        let msg = secp256k1::Message::from_digest(msg_hash);
        let rec_sig = secp.sign_ecdsa_recoverable(&msg, &secret_key);
        let (rec_id, sig64) = rec_sig.serialize_compact();
        let mut sig_bytes = vec![27 + rec_id.to_i32() as u8 + 4];
        sig_bytes.extend_from_slice(&sig64);

        let tx = Transaction {
            version: 1,
            tx_type: TxType::LegacyClaim,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: claimable,
                script_pubkey,
            }],
            lock_time: 0,
            claim_address: Some(derived_address.clone()),
            claim_signature: Some(sig_bytes),
        };

        txid = hex::encode(tx.txid());

        let mut mempool = handle.rpc_state.mempool.lock().await;
        mempool
            .add_transaction(tx)
            .map_err(|e| TauriError::NodeError(format!("Failed to submit claim: {}", e)))?;
    }

    tracing::info!(
        "Legacy claim submitted: {} VTR from {} to {}",
        claimable as f64 / 100_000_000.0,
        derived_address,
        recipient_address
    );

    Ok(ClaimSubmitResult {
        txid,
        claimed_satoshis: claimable,
        recipient_address,
    })
}
