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

    let wallet = Wallet::load(&path, &passphrase).map_err(TauriError::from)?;

    // Verify 2FA if the wallet has it enabled
    if wallet.has_2fa() {
        let code = otp_code.ok_or(TauriError::TwoFAFailed)?;
        if !wallet.verify_2fa(&code).map_err(TauriError::from)? {
            return Err(TauriError::TwoFAFailed);
        }
    }

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

    Ok(AddressInfo {
        address: address.to_string(),
        label: label.unwrap_or_else(|| "New Address".into()),
        balance: 0,
        is_legacy_import: false,
    })
}

/// Get all addresses in the current wallet.
///
/// Called from: `DashboardPage.tsx` → `invoke('get_addresses')`
#[tauri::command]
pub fn get_addresses(state: State<AppState>) -> Result<Vec<AddressInfo>> {
    let guard = state.wallet.lock().unwrap();
    let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;

    let addresses = wallet
        .list_addresses()
        .iter()
        .map(|(addr, label, balance, is_import)| AddressInfo {
            address: addr.to_string(),
            label: label.clone(),
            balance: *balance,
            is_legacy_import: *is_import,
        })
        .collect();

    Ok(addresses)
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
    pub block_height: u64,
    pub best_hash: String,
    pub peer_count: usize,
    pub syncing: bool,
    pub mempool_count: usize,
    pub network: String,
}

/// Response type for a single transaction.
#[derive(Debug, Serialize)]
pub struct TxResult {
    pub txid: String,
    pub block_height: u32,
    pub confirmations: u32,
    pub timestamp: u32,
    pub direction: String,
    pub amount_satoshis: u64,
    pub fee_satoshis: u64,
}

/// Response type for a single torrent session.
#[derive(Debug, Serialize)]
pub struct TorrentResult {
    pub id: String,
    pub name: String,
    pub info_hash: String,
    pub state: String,
    pub progress: f64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub peers: usize,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub earnings_satoshis: u64,
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
    pub vtr_amount: u64,
    pub target_asset: String,
    pub target_amount: u64,
    pub status: String,
    pub expiry: u32,
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
    let rpc_state = RpcAppState::new_with_shared(node.chain_arc(), node.mempool_arc());

    // Wire the node's event sender to the RPC broadcaster.
    let (event_tx, _rx) = vtorrent_node::events::channel(1024);
    node.set_event_sender(event_tx);

    // Store the NodeHandle before spawning.
    let handle = NodeHandle {
        rpc_state: rpc_state.clone(),
    };
    *state.node.lock().await = Some(handle);

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
            block_height: 0,
            best_hash: String::new(),
            peer_count: 0,
            syncing: false,
            mempool_count: 0,
            network: "vtorrent-mainnet".into(),
        }),
        Some(handle) => {
            let rpc = &handle.rpc_state;
            let chain = rpc.chain.lock().await;
            let mempool = rpc.mempool.lock().await;
            let peer_count = *rpc.peer_count.read().await;
            let syncing = *rpc.syncing.read().await;
            Ok(NodeInfoResult {
                running: true,
                block_height: chain.best_height() as u64,
                best_hash: chain.best_hash().map(hex::encode).unwrap_or_default(),
                peer_count,
                syncing,
                mempool_count: mempool.size(),
                network: "vtorrent-mainnet".into(),
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
        .map(|(txid, height, ts, dir, fee)| TxResult {
            txid,
            block_height: height,
            confirmations: best.saturating_sub(height),
            timestamp: ts,
            direction: dir,
            amount_satoshis: 0, // populated by UTXO diff in a future pass
            fee_satoshis: fee,
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
            download_speed: s.download_speed,
            upload_speed: s.upload_speed,
            peers: s.peers.len(),
            size_bytes: s.metainfo.total_size,
            downloaded_bytes: s.bytes_downloaded,
            uploaded_bytes: s.bytes_uploaded,
            earnings_satoshis: s
                .incentive_accounts
                .values()
                .map(|a| a.total_earned_satoshis)
                .sum(),
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
            vtr_amount: o.vtr_amount,
            target_asset: o.target_asset.clone(),
            target_amount: o.target_amount,
            status: format!("{:?}", o.status),
            expiry: o.expiry,
        })
        .collect())
}

/// Place a DEX order.
///
/// Called from: `TradePage.tsx` → `invoke('place_dex_order', { makerAddress, vtrAmount, targetAsset, targetAmount })`
#[tauri::command]
pub async fn place_dex_order(
    state: tauri::State<'_, AppState>,
    maker_address: String,
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
    let order = SwapOrder::new(
        maker_address,
        vtr_amount,
        target_asset,
        target_amount,
        86400,
    );
    let result = DexOrderResult {
        id: hex::encode(order.order_id),
        maker_address: order.maker_address.clone(),
        vtr_amount: order.vtr_amount,
        target_asset: order.target_asset.clone(),
        target_amount: order.target_amount,
        status: format!("{:?}", order.status),
        expiry: order.expiry,
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

    // Build and sign the transaction.
    let tx = TxBuilder::new()
        .recipient(&to_address, amount_satoshis)
        .fee_rate(10)
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
    pub address: Option<String>,
    pub blocks_staked: u64,
    /// Estimated APY based on current network parameters (percentage).
    pub estimated_apy: f64,
    /// Total rewards earned this session (satoshis).
    pub rewards_earned_sats: u64,
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

    // Update the staking engine with the new address and enable it.
    {
        let mut engine = handle.rpc_state.staking.write().await;
        *engine = vtorrent_node::staking::StakingEngine::new(address.clone());
    }
    *handle.rpc_state.staking_enabled.write().await = true;
    *handle.rpc_state.staking_address.write().await = Some(address.clone());

    tracing::info!("Staking started for address: {}", address);

    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    Ok(StakingStatusResult {
        enabled: true,
        address: Some(address),
        blocks_staked,
        estimated_apy: 5.0,
        rewards_earned_sats: blocks_staked.saturating_mul(100_000_000) / 100,
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

    *handle.rpc_state.staking_enabled.write().await = false;
    *handle.rpc_state.staking_address.write().await = None;

    tracing::info!("Staking stopped");

    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    Ok(StakingStatusResult {
        enabled: false,
        address: None,
        blocks_staked,
        estimated_apy: 0.0,
        rewards_earned_sats: 0,
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
    let address = handle.rpc_state.staking_address.read().await.clone();
    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;

    Ok(StakingStatusResult {
        enabled,
        address,
        blocks_staked,
        estimated_apy: if enabled { 5.0 } else { 0.0 },
        rewards_earned_sats: blocks_staked.saturating_mul(100_000_000) / 100,
    })
}

// ─── Bitcoin wallet commands ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct BtcStatus {
    pub initialized: bool,
    pub balance_satoshis: u64,
    pub address: Option<String>,
    pub best_height: u32,
}

/// Get the BTC wallet status.
#[tauri::command]
pub fn get_btc_status(state: State<AppState>) -> Result<BtcStatus> {
    let wallet = state.wallet.lock().map_err(|_| TauriError::WalletLocked)?;
    let wallet = wallet.as_ref().ok_or(TauriError::WalletNotInitialized)?;
    if !wallet.has_hd() {
        return Ok(BtcStatus {
            initialized: false,
            balance_satoshis: 0,
            address: None,
            best_height: 0,
        });
    }
    let mnemonic = wallet.mnemonic().ok_or(TauriError::WalletNotInitialized)?;
    let seed = vtorrent_wallet::hd::Mnemonic::from_phrase(mnemonic)
        .map_err(TauriError::from)?
        .to_seed();
    let mut btc = vtorrent_btc::wallet::BtcWallet::new(seed);
    let address = btc
        .next_address()
        .map_err(|e| TauriError::Wallet(e.to_string()))?;
    Ok(BtcStatus {
        initialized: true,
        balance_satoshis: btc.balance(),
        address: Some(address),
        best_height: btc.best_height(),
    })
}
