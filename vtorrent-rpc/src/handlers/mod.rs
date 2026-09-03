use axum::{extract::State, Json};

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;
use std::sync::Arc;
use vtorrent_core::time::now_secs;

pub mod blockchain;
pub mod btc;
pub mod dex;
pub mod prelude;
pub mod regtest;
pub mod staking;
pub mod swap;
pub mod torrent;
pub mod wallet;

pub use blockchain::*;
pub use btc::*;
pub use dex::*;
pub use regtest::*;
pub use staking::*;
pub use swap::*;
pub use torrent::*;
pub use wallet::*;

/// Current time in seconds, honoring the regtest mock clock if set.
pub(crate) async fn now_secs_mock(state: &AppState) -> u64 {
    if let Some(t) = *state.mock_time.read().await {
        t
    } else {
        now_secs()
    }
}

/// Broadcast a raw BTC transaction to the configured network/peer.
pub(crate) async fn broadcast_btc(state: &AppState, raw: &[u8]) -> RpcResult<[u8; 32]> {
    let network = *state.btc_network.read().await;
    let peer = state.btc_peer.read().await.clone();
    if let Some(host) = peer {
        // Resolve per broadcast: container peers may change IPs across restarts.
        let addr = tokio::net::lookup_host(&host)
            .await
            .ok()
            .and_then(|mut it| it.next())
            .ok_or_else(|| {
                RpcError::Internal(format!(
                    "BTC peer {} DNS resolution failed — check btc_peer config",
                    host
                ))
            })?;
        vtorrent_btc::sync::broadcast_tx_to(raw, network, &[addr])
            .await
            .map_err(|e| RpcError::Internal(format!("BTC broadcast to {} failed: {}", host, e)))
    } else {
        vtorrent_btc::sync::broadcast_tx(raw).await.map_err(|e| {
            RpcError::Internal(format!("BTC broadcast (no peer configured) failed: {}", e))
        })
    }
}

pub(crate) fn parse_hash32(value: &str, field: &str) -> RpcResult<[u8; 32]> {
    let bytes = hex::decode(value).map_err(|_| {
        RpcError::BadRequest(format!(
            "Invalid {} hex: expected 64 hex characters, got \"{}\"",
            field,
            &value[..value.len().min(64)]
        ))
    })?;
    if bytes.len() != 32 {
        return Err(RpcError::BadRequest(format!(
            "{} must be exactly 32 bytes (64 hex chars), got {} bytes ({} hex chars)",
            field,
            bytes.len(),
            value.len()
        )));
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes);
    Ok(hash)
}

/// Encode a Bitcoin txid stored in internal (little-endian) byte order as the
/// display-order hex string used by Bitcoin Core and block explorers.
pub fn btc_txid_hex(bytes: &[u8; 32]) -> String {
    let mut display = *bytes;
    display.reverse();
    hex::encode(display)
}

/// Reject swap operations whose current lifecycle stage makes them invalid
/// (e.g. double-funding, claiming after refund, refunding after claim).
pub fn require_swap_stage(
    swap: Option<&vtorrent_node::atomic_swap::SwapState>,
    forbidden: &[vtorrent_node::atomic_swap::SwapStatus],
) -> RpcResult<()> {
    if let Some(swap) = swap {
        if forbidden.contains(&swap.status) {
            return Err(RpcError::BadRequest(format!(
                "Swap is in state {:?}; operation not allowed",
                swap.status
            )));
        }
    }
    Ok(())
}

pub(crate) fn block_response(
    hash: [u8; 32],
    height: u32,
    block: &vtorrent_node::block::Block,
) -> BlockResponse {
    BlockResponse {
        hash: hex::encode(hash),
        height: height as u64,
        version: block.header.version,
        prev_hash: hex::encode(block.header.prev_block_hash),
        merkle_root: hex::encode(block.header.merkle_root),
        timestamp: block.header.timestamp,
        bits: block.header.bits,
        nonce: block.header.nonce,
        tx_count: block.transactions.len(),
        size_bytes: bincode::serialized_size(block).unwrap_or(0) as usize,
    }
}

pub(crate) fn transaction_lookup_response(
    txid: [u8; 32],
    tx: &vtorrent_node::block::Transaction,
    block_hash: Option<[u8; 32]>,
    block_height: Option<u32>,
) -> TransactionLookupResponse {
    TransactionLookupResponse {
        txid: hex::encode(txid),
        block_hash: block_hash.map(hex::encode),
        block_height,
        version: tx.version,
        tx_type: tx.type_str().to_string(),
        inputs: tx
            .inputs
            .iter()
            .map(|input| TransactionInputResponse {
                prev_txid: hex::encode(input.prev_txid),
                prev_vout: input.prev_vout,
                script_sig: hex::encode(&input.script_sig),
                sequence: input.sequence,
            })
            .collect(),
        outputs: tx
            .outputs
            .iter()
            .map(|output| TransactionOutputResponse {
                value_satoshis: output.value,
                script_pubkey: hex::encode(&output.script_pubkey),
            })
            .collect(),
        lock_time: tx.lock_time,
        claim_address: tx.claim_address.clone(),
    }
}

pub fn validate_p2pkh(addr: &str) -> RpcResult<()> {
    vtorrent_core::address::validate_p2pkh(addr)
        .map(|_| ())
        .map_err(|e| {
            RpcError::BadRequest(format!(
                "Invalid address: must be a VTR address with prefix V, got \"{}\": {}",
                &addr[..addr.len().min(80)],
                e
            ))
        })
}

/// Verify the hot wallet passphrase (and TOTP code if 2FA is enabled) and
/// return the decrypted WIF. Fails if no wallet has been imported or the
/// credentials are incorrect.
pub(crate) async fn verify_wallet_auth(
    state: &AppState,
    passphrase: &str,
    otp_code: Option<&str>,
) -> RpcResult<String> {
    let encrypted = state.wallet_encrypted.read().await.clone().ok_or_else(|| {
        RpcError::BadRequest("No wallet imported. Call POST /api/v1/wallet/import first. (hint: import a WIF key before unlocking)".into())
    })?;

    let plaintext =
        vtorrent_wallet::encryption::decrypt_wallet(&encrypted, passphrase).map_err(|_| {
            RpcError::Unauthorized("Incorrect passphrase — wallet decryption failed".into())
        })?;
    let wif = String::from_utf8(plaintext).map_err(|_| {
        RpcError::Internal(
            "Wallet decryption produced invalid UTF-8 data — key may be corrupted".into(),
        )
    })?;

    if let Some(secret) = state.wallet_totp_secret.read().await.as_ref() {
        let code = otp_code.filter(|c| !c.is_empty()).ok_or_else(|| {
            RpcError::Unauthorized(
                "TOTP code required — 2FA is enabled on this wallet, provide otp_code".into(),
            )
        })?;
        secret.verify_or_error(code).map_err(|_| {
            RpcError::Unauthorized("Invalid TOTP code — check your authenticator app".into())
        })?;
    }

    Ok(wif)
}

pub async fn get_spv_status(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<SpvStatusResponse>> {
    let chain = state.spv_chain.read().await;
    let best_hash = chain.best_hash().map(hex::encode).unwrap_or_default();
    Ok(Json(SpvStatusResponse {
        header_count: chain.len(),
        best_height: chain.best_height(),
        best_hash,
    }))
}

/// POST /api/v1/spv/headers - submit a batch of block headers to the SPV chain.
pub async fn add_spv_headers(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SpvAddHeadersRequest>,
) -> RpcResult<Json<SpvAddHeadersResponse>> {
    use vtorrent_spv::SpvHeader;

    let mut headers: Vec<SpvHeader> = Vec::with_capacity(req.headers.len());
    for h in req.headers {
        let ph = parse_hash32(&h.prev_hash, "prev_hash")?;
        let mr = parse_hash32(&h.merkle_root, "merkle_root")?;

        headers.push(SpvHeader {
            version: h.version,
            prev_hash: ph,
            merkle_root: mr,
            utxo_root: h
                .utxo_root
                .as_deref()
                .map(|s| parse_hash32(s, "utxo_root"))
                .transpose()?
                .unwrap_or([0u8; 32]),
            timestamp: h.timestamp,
            bits: h.bits,
            nonce: h.nonce,
            stake_modifier: h.stake_modifier.unwrap_or(0),
            height: h.height,
        });
    }

    let added = {
        let mut chain = state.spv_chain.write().await;
        chain.add_headers(headers).map_err(|e| {
            RpcError::BadRequest(format!("SPV header chain rejected batch of headers: {}", e))
        })?
    };

    let chain = state.spv_chain.read().await;
    let best_hash = chain.best_hash().map(hex::encode).unwrap_or_default();

    tracing::info!(
        "SPV: added {} headers, best height now {}",
        added,
        chain.best_height()
    );

    Ok(Json(SpvAddHeadersResponse {
        added,
        best_height: chain.best_height(),
        best_hash,
    }))
}

/// GET /api/v1/spv/proof/:hash — retrieve a stored StakeProof for a PoS header (if available).
pub async fn spv_get_proof(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(hash_hex): axum::extract::Path<String>,
) -> RpcResult<Json<SpvProofResponse>> {
    let hash = parse_hash32(&hash_hex, "hash")?;
    let chain = state.spv_chain.read().await;
    let header = chain.get_header(&hash).cloned();
    drop(chain);
    if header.is_none() {
        return Err(RpcError::NotFound(format!("header {} not found", hash_hex)));
    }
    let proof = state
        .stake_proofs
        .read()
        .await
        .get(&hash)
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| RpcError::Internal(format!("serialize stake proof: {}", e)))?;
    Ok(Json(SpvProofResponse {
        block_hash: hash_hex,
        proof,
    }))
}

// ─── Peers ────────────────────────────────────────────────────────────────────

/// GET /api/v1/peers
///
/// Returns the list of currently connected P2P peers with their metadata.
/// The list is updated live by the daemon event bridge on `PeerConnected` /
/// `PeerDisconnected` events.
pub async fn get_peers(State(state): State<Arc<AppState>>) -> RpcResult<Json<PeersResponse>> {
    let peer_list = state.peer_list.read().await;
    let peers: Vec<PeerInfoResponse> = peer_list
        .iter()
        .map(|p| PeerInfoResponse {
            addr: p.addr.clone(),
            user_agent: p.user_agent.clone(),
            services: p.services,
            best_height: p.best_height,
        })
        .collect();
    let count = peers.len();
    Ok(Json(PeersResponse { count, peers }))
}

// ─── Bitcoin wallet ────────────────────────────────────────────────────────────

#[cfg(test)]
mod relay_floor_lockstep {
    #[test]
    fn wallet_minimum_matches_mempool_relay_policy() {
        assert_eq!(
            vtorrent_node::mempool::MIN_RELAY_FEE,
            vtorrent_wallet::tx_builder::MIN_ABSOLUTE_FEE_SATS,
            "wallet builder floor drifted from mempool relay policy"
        );
    }

    #[cfg(test)]
    mod swap_guard_tests {
        use crate::handlers::require_swap_stage;
        use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

        fn state_at(status: SwapStatus) -> SwapState {
            let mut s = SwapState::new([7u8; 32], [9u8; 32]);
            s.status = status;
            s
        }

        #[test]
        fn absent_state_is_allowed() {
            assert!(require_swap_stage(None, &[SwapStatus::Refunded]).is_ok());
        }

        #[test]
        fn terminal_states_block_everything() {
            for status in [SwapStatus::Claimed, SwapStatus::Refunded] {
                let s = state_at(status);
                assert!(
                    require_swap_stage(Some(&s), &[SwapStatus::Claimed, SwapStatus::Refunded])
                        .is_err()
                );
            }
        }

        #[test]
        fn funded_states_allow_claims() {
            for status in [
                SwapStatus::Funding,
                SwapStatus::VtrFunded,
                SwapStatus::BtcFunded,
            ] {
                let s = state_at(status);
                assert!(require_swap_stage(Some(&s), &[SwapStatus::Refunded]).is_ok());
            }
        }
    }
}
