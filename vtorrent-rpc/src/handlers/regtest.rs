//! Regtest-only endpoints: faucet and debug helpers.

use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

pub async fn faucet(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FaucetRequest>,
) -> RpcResult<Json<FaucetResponse>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Faucet is only available in regtest mode".into(),
        ));
    }
    if req.address.trim().is_empty() {
        return Err(RpcError::BadRequest(
            "Address is required — provide a valid VTR address".into(),
        ));
    }

    // Per-address cooldown: 10 seconds between claims from the same address.
    // Entries older than the window are pruned on every call — without this
    // the map grows by one entry per unique address for the node's lifetime.
    const FAUCET_COOLDOWN_SECS: u64 = 10;
    {
        let mut cooldowns = state.faucet_cooldowns.write().await;
        cooldowns.retain(|_, last| last.elapsed().as_secs() < FAUCET_COOLDOWN_SECS);
        if let Some(last) = cooldowns.get(&req.address) {
            let elapsed = last.elapsed().as_secs();
            if elapsed < FAUCET_COOLDOWN_SECS {
                return Err(RpcError::BadRequest(format!(
                    "Faucet cooldown: wait {} more seconds before claiming again",
                    FAUCET_COOLDOWN_SECS - elapsed,
                )));
            }
        }
    }

    let amount = req
        .amount_satoshis
        .unwrap_or(100 * vtorrent_node::consensus::COIN);
    if amount == 0 {
        return Err(RpcError::BadRequest(
            "Amount must be non-zero — provide a positive satoshi amount".into(),
        ));
    }
    // Cap per-request mints: without this a single request can mint the
    // entire remaining supply headroom.
    const FAUCET_MAX_PER_REQUEST: u64 = 1_000 * vtorrent_node::consensus::COIN;
    if amount > FAUCET_MAX_PER_REQUEST {
        return Err(RpcError::BadRequest(format!(
            "Amount {} sats exceeds the per-request faucet cap of {} sats",
            amount, FAUCET_MAX_PER_REQUEST
        )));
    }

    // Truncate by chars: byte-slicing arbitrary UTF-8 text can split a
    // multi-byte character and panic the handler task.
    let address_display: String = req.address.chars().take(64).collect();

    let (txid, height, block) = {
        let mut chain = state.chain.lock().await;
        let txid = chain.mint_to_address(&req.address, amount).map_err(|e| {
            RpcError::Internal(format!(
                "Failed to mint {} sats to {}: {}",
                amount, address_display, e
            ))
        })?;
        let height = chain.best_height();
        let block = chain
            .get_block_at_height(height)
            .cloned()
            .ok_or_else(|| RpcError::Internal(format!("Minted block at height {} not found immediately after mint — chain state inconsistent", height)))?;
        (txid, height, block)
    };

    // Stamp the cooldown only after the mint succeeded — a failed mint
    // (invalid address, supply cap) must not burn the caller's window.
    state
        .faucet_cooldowns
        .write()
        .await
        .insert(req.address.clone(), std::time::Instant::now());

    // Announce the minted block to peers so a multi-node regtest network stays
    // in sync (the faucet mints directly into the chain, bypassing the node's
    // normal block-production path).
    if let Some(sender) = &state.block_submit {
        let _ = sender.try_send(block);
    }

    Ok(Json(FaucetResponse {
        address: req.address,
        amount_satoshis: amount,
        txid: hex::encode(txid),
        block_height: height as u64,
    }))
}

/// GET /api/v1/debug/order/:id/preimage
///
/// Returns the secret preimage for an order (regtest only). In a real swap the
/// taker learns the preimage when the maker claims BTC on-chain; this endpoint
/// exists purely to exercise the VTR claim leg in local regtest testing.
pub async fn debug_order_preimage(
    State(state): State<Arc<AppState>>,
    Path(order_id): Path<String>,
) -> RpcResult<Json<Value>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Preimage debug endpoint is only available in regtest mode".into(),
        ));
    }
    let order_book = state.order_book.read().await;
    let order = order_book.get_order(&order_id).ok_or_else(|| {
        RpcError::NotFound(format!(
            "Order {} not found — check the order_id and ensure the order exists",
            order_id
        ))
    })?;
    let preimage = order.preimage.ok_or_else(|| {
        RpcError::BadRequest(format!(
            "Order {} has no preimage — only maker-created orders have preimages",
            order_id
        ))
    })?;
    Ok(Json(json!({
        "order_id": order_id,
        "preimage": hex::encode(preimage),
    })))
}

/// POST /api/v1/debug/mocktime
///
/// Sets the regtest mock clock (regtest only). Time-dependent checks (e.g.
/// HTLC expiry) use this instead of the wall clock, enabling refund-path
/// testing without waiting for real time to pass. A `null` timestamp resets
/// to the wall clock.
pub async fn debug_mocktime(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Value>,
) -> RpcResult<Json<Value>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Mocktime is only available in regtest mode".into(),
        ));
    }
    let ts = req.get("timestamp").cloned();
    let new_time = match ts {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(n.as_u64().filter(|n| *n <= u32::MAX as u64).ok_or_else(
            || RpcError::BadRequest("timestamp must be an integer between 0 and u32::MAX".into()),
        )?),
        _ => {
            return Err(RpcError::BadRequest(
                "timestamp must be a number (unix seconds) or null to reset — got unexpected type"
                    .into(),
            ))
        }
    };
    *state.mock_time.write().await = new_time;
    Ok(Json(json!({ "mock_time": new_time })))
}

/// POST /api/v1/blockchain/mine
///
/// Test-only regtest miner: builds a block containing the current valid
/// mempool transactions and a coinbase mint. It exists so integration tests
/// can confirm mempool transactions without enabling staking. Compiled only
/// with the `test-util` feature.
#[cfg(feature = "test-util")]
pub async fn mine_block(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Value>,
) -> RpcResult<Json<Value>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Mining is only available in regtest mode".into(),
        ));
    }
    let address = req["address"].as_str().unwrap_or("").trim();
    if address.is_empty() {
        return Err(RpcError::BadRequest("Address is required".into()));
    }
    let amount = req["amount_satoshis"]
        .as_u64()
        .unwrap_or(vtorrent_node::consensus::COIN);
    if amount == 0 {
        return Err(RpcError::BadRequest("Amount must be non-zero".into()));
    }
    let script = vtorrent_core::address::Address::parse(address)
        .map_err(|e| RpcError::BadRequest(format!("Invalid address: {e}")))?
        .p2pkh_script_pubkey();

    let (block, height, included) = {
        let mut chain = state.chain.lock().await;
        let mut mempool = state.mempool.lock().await;
        if chain.total_supply().saturating_add(amount) > vtorrent_node::consensus::MAX_SUPPLY {
            return Err(RpcError::BadRequest(
                "Mining would exceed maximum supply".into(),
            ));
        }
        let height = chain.best_height() + 1;
        let prev_hash = chain
            .best_hash()
            .ok_or_else(|| RpcError::Internal("Chain has no tip".into()))?;
        let parent = chain
            .get_header_at_height(chain.best_height())
            .ok_or_else(|| RpcError::Internal("Parent header missing".into()))?;
        let timestamp = (vtorrent_core::time::now_secs().min(u64::from(u32::MAX)) as u32)
            .max(parent.timestamp.saturating_add(1));
        let coinbase = vtorrent_node::block::Transaction {
            version: 1,
            tx_type: vtorrent_node::block::TxType::Coinbase,
            inputs: vec![vtorrent_node::block::TxInput {
                prev_txid: [0; 32],
                prev_vout: u32::MAX,
                script_sig: vec![height as u8],
                sequence: u32::MAX,
            }],
            outputs: vec![vtorrent_node::block::TxOutput {
                value: amount,
                script_pubkey: script,
            }],
            lock_time: height,
            claim_address: None,
            claim_signature: None,
        };
        let txs = mempool.get_transactions();
        let included: Vec<[u8; 32]> = txs.iter().map(|tx| tx.txid()).collect();
        let mut transactions = vec![coinbase];
        transactions.extend(txs);
        let mut block = vtorrent_node::block::Block {
            header: vtorrent_node::block::BlockHeader {
                version: 1,
                prev_block_hash: prev_hash,
                merkle_root: [0; 32],
                utxo_root: [0; 32],
                timestamp,
                bits: vtorrent_node::genesis::GENESIS_BITS,
                nonce: height,
                stake_modifier: vtorrent_node::consensus::compute_stake_modifier(
                    parent.stake_modifier,
                    &prev_hash,
                ),
            },
            transactions,
        };
        block.header.merkle_root = block.compute_merkle_root();
        match chain.add_block(block.clone()) {
            Ok(vtorrent_node::chain::BlockAcceptance::MainChain { .. }) => {}
            Ok(_) => {
                return Err(RpcError::Internal(
                    "Test miner produced a non-main-chain block".into(),
                ))
            }
            Err(e) => return Err(RpcError::Internal(format!("Mining failed: {e}"))),
        }
        for txid in &included {
            mempool.remove_transaction(txid);
        }
        (block, height, included)
    };

    if let Some(sender) = &state.block_submit {
        let _ = sender.try_send(block);
    }
    Ok(Json(json!({
        "height": height,
        "included": included.iter().map(hex::encode).collect::<Vec<_>>(),
    })))
}
