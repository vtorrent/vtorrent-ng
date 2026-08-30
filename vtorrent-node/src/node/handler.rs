// Lock order: chain → mempool — always acquire Chain before Mempool to avoid deadlock.
//! Extracted message handlers for the major P2P message types.
//!
//! Each function handles a single message command. They are kept as free
//! functions so they can be unit-tested independently and to keep
//! `handle_message` in `mod.rs` focused on the rate-limiting preamble
//! and small message arms.

use std::collections::HashMap;
use std::net::SocketAddr;

use vtorrent_p2p::{
    ban_manager::Misbehaviour,
    compact::{derive_siphash_keys, short_txid, CompactBlockDecodeError, CompactBlockDecoder},
    message::{
        decode_for_peer, encode_for_peer, BlockTxnMsg, GetBlocksMsg, GetDataMsg, GetHeadersMsg,
        HeaderEntry, HeadersMsg, InvItem, InvMsg, InvType, NetMessage, PROTOCOL_VERSION,
    },
};

use crate::{
    block::{Block, BlockHeader, Transaction},
    chain::BlockAcceptance,
    consensus::compute_stake_modifier,
    error::{NodeError, Result},
    events::NodeEvent,
};

use super::Node;

/// Handle an `inv` message — announce inventory to the peer manager.
pub(crate) async fn handle_inv(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
    peer_version: u32,
) -> Result<()> {
    if let Ok(inv) = decode_for_peer::<InvMsg>(&msg.payload, peer_version) {
        // Bound the inventory announcement: an unbounded list makes us issue
        // one getdata per unknown item, amplifying a peer's bandwidth into
        // full block/tx fetches (DoS vector).
        const MAX_INV_ITEMS: usize = 1_000;
        if inv.items.len() > MAX_INV_ITEMS {
            tracing::debug!(
                "inv from {} with {} items — rejecting (max {})",
                peer_addr,
                inv.items.len(),
                MAX_INV_ITEMS
            );
            node.peer_manager
                .record_misbehaviour(peer_addr, Misbehaviour::OversizedMessage)
                .await;
            return Ok(());
        }
        let mut want = Vec::new();
        for item in &inv.items {
            match item.inv_type {
                InvType::Block => {
                    let chain = node.chain.lock().await;
                    if chain.get_block(&item.hash).is_none() {
                        want.push(item.clone());
                    }
                }
                InvType::Transaction => {
                    let mp = node.mempool.lock().await;
                    if mp.get_transaction(&item.hash).is_none() {
                        want.push(item.clone());
                    }
                }
                _ => {}
            }
        }
        if !want.is_empty() {
            let payload = encode_for_peer(
                &vtorrent_p2p::message::GetDataMsg { items: want },
                peer_version,
            );
            node.peer_manager
                .broadcast(NetMessage::new("getdata", payload))
                .await;
        }
    }
    Ok(())
}

/// Handle a `block` message — validate, accept, and relay.
pub(crate) async fn handle_block(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
    peer_version: u32,
) -> Result<()> {
    match node.deserialize_block(&msg.payload) {
        Ok(block) => {
            let hash = block.hash();
            let tx_count = block.transactions.len();
            let timestamp = block.header.timestamp;
            let size_bytes = serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
            let block_arc = std::sync::Arc::new(block);
            let mut chain = node.chain.lock().await;
            match chain.add_block((*block_arc).clone()) {
                Ok(acceptance) => {
                    let should_relay = match acceptance {
                        BlockAcceptance::MainChain {
                            height,
                            utxos_added,
                            utxos_removed,
                            claimed_addresses,
                        } => {
                            tracing::info!(
                                "Accepted block {} at height {}",
                                hex::encode(hash),
                                height
                            );
                            {
                                let confirmed: Vec<[u8; 32]> =
                                    block_arc.transactions.iter().map(|tx| tx.txid()).collect();
                                let mut mp = node.mempool.lock().await;
                                mp.handle_confirmed_block(&confirmed, &utxos_removed);
                            }
                            node.emit(NodeEvent::NewBlock {
                                height,
                                hash,
                                tx_count,
                                timestamp,
                                size_bytes,
                                block: block_arc.clone(),
                                utxos_added,
                                utxos_removed,
                                claimed_addresses,
                            });
                            for tx in block_arc.transactions.iter() {
                                node.emit(NodeEvent::TxConfirmed {
                                    txid: tx.txid(),
                                    block_height: height,
                                    block_hash: hash,
                                });
                            }
                            true
                        }
                        BlockAcceptance::Reorg {
                            old_tip,
                            new_tip,
                            depth,
                            rolled_back_txs,
                            rolled_back_blocks,
                            applied_fork_blocks,
                        } => {
                            tracing::warn!(
                                "Reorg depth {}: {} -> {}",
                                depth,
                                hex::encode(old_tip),
                                hex::encode(new_tip)
                            );
                            {
                                let chain = node.chain.lock().await;
                                let mut mp = node.mempool.lock().await;
                                for tx in rolled_back_txs {
                                    if let Some(fee) = chain.compute_tx_fee(&tx) {
                                        let _ = mp.add_transaction_with_fee(tx, fee);
                                    }
                                }
                            }
                            node.emit(NodeEvent::Reorg {
                                old_tip,
                                new_tip,
                                depth,
                                rolled_back_blocks,
                                applied_fork_blocks,
                            });
                            true
                        }
                        BlockAcceptance::Fork { fork_tip } => {
                            tracing::debug!("Fork block {} stored", hex::encode(fork_tip));
                            false
                        }
                        BlockAcceptance::Duplicate => false,
                    };
                    if should_relay {
                        let inv_msg = InvMsg {
                            items: vec![InvItem {
                                inv_type: InvType::Block,
                                hash,
                            }],
                        };
                        let payload = encode_for_peer(&inv_msg, peer_version);
                        drop(chain);
                        node.peer_manager
                            .broadcast_except(peer_addr, NetMessage::new("inv", payload))
                            .await;
                    }
                }
                Err(e) => {
                    tracing::warn!("Rejected block from {}: {}", peer_addr, e);
                    node.peer_manager
                        .ban_peer_with_duration(
                            peer_addr,
                            std::time::Duration::from_secs(3600),
                            format!("Invalid block: {}", e),
                        )
                        .await;
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to deserialize block from {}: {}", peer_addr, e);
            node.peer_manager
                .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                .await;
        }
    }
    Ok(())
}

/// Handle a `tx` message — validate fee and add to mempool.
pub(crate) async fn handle_tx(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
    peer_version: u32,
) -> Result<()> {
    match node.deserialize_tx(&msg.payload) {
        Ok(tx) => {
            let real_fee = {
                let chain = node.chain.lock().await;
                chain.compute_tx_fee(&tx)
            };
            let mut mp = node.mempool.lock().await;
            let result = match real_fee {
                Some(fee) => mp.add_transaction_with_fee(tx.clone(), fee),
                None => Err(NodeError::Chain("Inputs not found in UTXO set".into())),
            };
            match result {
                Ok(()) => {
                    let txid = tx.txid();
                    let fee_sats = real_fee.unwrap_or(0);
                    let size_bytes = tx.serialized_size();
                    tracing::debug!("Accepted tx {}", hex::encode(txid));
                    node.emit(NodeEvent::TxUnconfirmed {
                        txid,
                        fee_sats,
                        size_bytes,
                    });
                    let inv_msg = InvMsg {
                        items: vec![InvItem {
                            inv_type: InvType::Transaction,
                            hash: txid,
                        }],
                    };
                    let payload = encode_for_peer(&inv_msg, peer_version);
                    drop(mp);
                    node.peer_manager
                        .broadcast_except(peer_addr, NetMessage::new("inv", payload))
                        .await;
                }
                Err(NodeError::PolicyRejected(_)) => {
                    tracing::debug!("Rejected tx by policy");
                }
                Err(e) => {
                    tracing::debug!("Rejected tx: {}", e);
                    node.peer_manager
                        .record_misbehaviour(peer_addr, Misbehaviour::InvalidTransaction)
                        .await;
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to deserialize tx from {}: {}", peer_addr, e);
            node.peer_manager
                .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                .await;
        }
    }
    Ok(())
}

/// Handle a `getblocks` message — respond with inventory hashes.
pub(crate) async fn handle_getblocks(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
    peer_version: u32,
) -> Result<()> {
    if let Ok(req) = decode_for_peer::<GetBlocksMsg>(&msg.payload, peer_version) {
        const MAX_LOCATOR_HASHES: usize = 64;
        if req.block_locator_hashes.len() > MAX_LOCATOR_HASHES {
            tracing::debug!(
                "getblocks from {} with {} locator hashes — rejecting",
                peer_addr,
                req.block_locator_hashes.len()
            );
            node.peer_manager
                .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                .await;
        } else {
            let chain = node.chain.lock().await;
            let our_height = chain.best_height();

            let locator: std::collections::HashSet<[u8; 32]> =
                req.block_locator_hashes.iter().copied().collect();
            let mut start_height = 1u32;
            for h in (1..=our_height).rev() {
                if let Some(b) = chain.get_block_at_height(h) {
                    if locator.contains(&b.hash()) {
                        start_height = h + 1;
                        break;
                    }
                }
            }

            let mut items = Vec::new();
            for h in start_height..=our_height.min(start_height + 500) {
                if let Some(block) = chain.get_block_at_height(h) {
                    items.push(InvItem {
                        inv_type: InvType::Block,
                        hash: block.hash(),
                    });
                }
            }

            if !items.is_empty() {
                let inv_msg = InvMsg { items };
                let payload = encode_for_peer(&inv_msg, peer_version);
                drop(chain);
                node.peer_manager
                    .send_to(peer_addr, NetMessage::new("inv", payload))
                    .await;
            }
        }
    }
    Ok(())
}

/// Handle a `cmpctblock` message — reconstruct full block from compact representation.
pub(crate) async fn handle_cmpctblock(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
) -> Result<()> {
    use vtorrent_p2p::message::CmpctBlockMsg;

    if let Ok(cmpct) = serde_json::from_slice::<CmpctBlockMsg>(&msg.payload) {
        let mut header_bytes = Vec::with_capacity(80);
        header_bytes.extend_from_slice(&cmpct.version.to_le_bytes());
        header_bytes.extend_from_slice(&cmpct.prev_block_hash);
        header_bytes.extend_from_slice(&cmpct.merkle_root);
        header_bytes.extend_from_slice(&cmpct.timestamp.to_le_bytes());
        header_bytes.extend_from_slice(&cmpct.bits.to_le_bytes());
        header_bytes.extend_from_slice(&cmpct.nonce.to_le_bytes());
        let (k0, k1) = derive_siphash_keys(&header_bytes, cmpct.siphash_nonce);

        let mempool_map = {
            let mp = node.mempool.lock().await;
            let entries = mp.get_entries();
            let mut map = std::collections::HashMap::new();
            for entry in entries {
                let txid = entry.tx.txid();
                let sid = short_txid(&txid, k0, k1);
                if let Ok(bytes) = serde_json::to_vec(&entry.tx) {
                    map.insert(sid, bytes);
                }
            }
            map
        };

        match CompactBlockDecoder::decode(&cmpct, &mempool_map) {
            Ok(tx_bytes_list) => {
                let mut txs: Vec<Transaction> = Vec::new();
                let mut all_ok = true;
                for bytes in &tx_bytes_list {
                    match serde_json::from_slice::<Transaction>(bytes) {
                        Ok(tx) => txs.push(tx),
                        Err(e) => {
                            tracing::warn!(
                                "cmpctblock: failed to decode tx from {}: {}",
                                peer_addr,
                                e
                            );
                            all_ok = false;
                            break;
                        }
                    }
                }
                if all_ok {
                    let stake_modifier = {
                        let chain = node.chain.lock().await;
                        chain
                            .get_block(&cmpct.prev_block_hash)
                            .map(|p| {
                                compute_stake_modifier(
                                    p.header.stake_modifier,
                                    &cmpct.prev_block_hash,
                                )
                            })
                            .unwrap_or(0)
                    };
                    let block = Block {
                        header: BlockHeader {
                            version: cmpct.version,
                            prev_block_hash: cmpct.prev_block_hash,
                            merkle_root: cmpct.merkle_root,
                            timestamp: cmpct.timestamp,
                            bits: cmpct.bits,
                            nonce: cmpct.nonce,
                            stake_modifier,
                        },
                        transactions: txs,
                    };
                    let hash = block.hash();
                    let tx_count = block.transactions.len();
                    let timestamp = block.header.timestamp;
                    let size_bytes = serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
                    let block_arc = std::sync::Arc::new(block);
                    let mut chain = node.chain.lock().await;
                    match chain.add_block((*block_arc).clone()) {
                        Ok(acceptance) => {
                            if let BlockAcceptance::MainChain {
                                height,
                                utxos_added,
                                utxos_removed,
                                claimed_addresses,
                            } = acceptance
                            {
                                tracing::info!(
                                    "cmpctblock: accepted block {} at height {}",
                                    hex::encode(hash),
                                    height
                                );
                                {
                                    let confirmed: Vec<[u8; 32]> =
                                        block_arc.transactions.iter().map(|tx| tx.txid()).collect();
                                    let mut mp = node.mempool.lock().await;
                                    mp.handle_confirmed_block(&confirmed, &utxos_removed);
                                }
                                node.emit(NodeEvent::NewBlock {
                                    height,
                                    hash,
                                    tx_count,
                                    timestamp,
                                    size_bytes,
                                    block: block_arc.clone(),
                                    utxos_added,
                                    utxos_removed,
                                    claimed_addresses,
                                });
                                for tx in block_arc.transactions.iter() {
                                    node.emit(NodeEvent::TxConfirmed {
                                        txid: tx.txid(),
                                        block_height: height,
                                        block_hash: hash,
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("cmpctblock: rejected block from {}: {}", peer_addr, e);
                        }
                    }
                }
            }
            Err(CompactBlockDecodeError::MissingTransactions(missing_indexes)) => {
                tracing::debug!(
                    "cmpctblock: {} missing txs from {}, sending getblocktxn",
                    missing_indexes.len(),
                    peer_addr
                );
                let stake_modifier = {
                    let chain = node.chain.lock().await;
                    chain
                        .get_block(&cmpct.prev_block_hash)
                        .map(|p| {
                            compute_stake_modifier(p.header.stake_modifier, &cmpct.prev_block_hash)
                        })
                        .unwrap_or(0)
                };
                let probe_block_hash = {
                    let hdr = crate::block::BlockHeader {
                        version: cmpct.version,
                        prev_block_hash: cmpct.prev_block_hash,
                        merkle_root: cmpct.merkle_root,
                        timestamp: cmpct.timestamp,
                        bits: cmpct.bits,
                        nonce: cmpct.nonce,
                        stake_modifier,
                    };
                    hdr.hash()
                };
                let block_hash = probe_block_hash;
                let req = CompactBlockDecoder::build_getblocktxn(block_hash, missing_indexes);
                let payload = serde_json::to_vec(&req).unwrap_or_default();
                node.peer_manager
                    .send_to(peer_addr, NetMessage::new("getblocktxn", payload))
                    .await;
                if node.pending_compact_blocks.len() >= 16 {
                    if let Some(oldest) = node.pending_compact_blocks.keys().next().copied() {
                        node.pending_compact_blocks.remove(&oldest);
                    }
                }
                node.pending_compact_blocks.insert(block_hash, cmpct);
            }
            Err(CompactBlockDecodeError::TooManyTransactions) => {
                tracing::warn!(
                    "cmpctblock: rejecting block with too many transactions from {}",
                    peer_addr
                );
            }
            Err(CompactBlockDecodeError::DuplicateShortId) => {
                tracing::warn!(
                    "cmpctblock: duplicate short IDs from {} — protocol violation",
                    peer_addr
                );
                node.peer_manager
                    .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                    .await;
            }
            Err(CompactBlockDecodeError::InvalidPrefilledIndex) => {
                tracing::warn!("cmpctblock: invalid prefilled index from {}", peer_addr);
                let _ = node
                    .peer_manager
                    .record_misbehaviour(
                        peer_addr,
                        vtorrent_p2p::ban_manager::Misbehaviour::MalformedMessage,
                    )
                    .await;
            }
        }
    }
    Ok(())
}

/// Handle a `getblocktxn` message — serve requested transactions from a block.
pub(crate) async fn handle_getblocktxn(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
) -> Result<()> {
    if let Ok(req) = serde_json::from_slice::<BlockTxnReq>(&msg.payload) {
        let chain = node.chain.lock().await;
        let mut found_txs: Option<Vec<Vec<u8>>> = None;
        if let Some(block) = chain.get_block(&req.block_hash) {
            let mut txs = Vec::new();
            for &idx in &req.indexes {
                let idx = idx as usize;
                if idx < block.transactions.len() {
                    if let Ok(bytes) = serde_json::to_vec(&block.transactions[idx]) {
                        txs.push(bytes);
                    }
                }
            }
            found_txs = Some(txs);
        }
        if let Some(txs) = found_txs {
            let resp = BlockTxnMsg {
                block_hash: req.block_hash,
                transactions: txs,
            };
            let payload = serde_json::to_vec(&resp).unwrap_or_default();
            drop(chain);
            node.peer_manager
                .send_to(peer_addr, NetMessage::new("blocktxn", payload))
                .await;
        }
    }
    Ok(())
}

/// Handle a `blocktxn` message — complete a pending compact block reconstruction.
pub(crate) async fn handle_blocktxn(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
) -> Result<()> {
    if let Ok(resp) = serde_json::from_slice::<BlockTxnMsg>(&msg.payload) {
        let Some(pending) = node.pending_compact_blocks.remove(&resp.block_hash) else {
            tracing::debug!(
                "blocktxn: no pending compact block for {} from {}",
                hex::encode(resp.block_hash),
                peer_addr
            );
            return Ok(());
        };
        tracing::debug!(
            "blocktxn: received {} txs for block {} from {}, completing reconstruction",
            resp.transactions.len(),
            hex::encode(resp.block_hash),
            peer_addr
        );

        let mut header_bytes = Vec::with_capacity(80);
        header_bytes.extend_from_slice(&pending.version.to_le_bytes());
        header_bytes.extend_from_slice(&pending.prev_block_hash);
        header_bytes.extend_from_slice(&pending.merkle_root);
        header_bytes.extend_from_slice(&pending.timestamp.to_le_bytes());
        header_bytes.extend_from_slice(&pending.bits.to_le_bytes());
        header_bytes.extend_from_slice(&pending.nonce.to_le_bytes());
        let (k0, k1) = derive_siphash_keys(&header_bytes, pending.siphash_nonce);

        let mempool_map = {
            let mp = node.mempool.lock().await;
            let entries = mp.get_entries();
            let mut map = std::collections::HashMap::new();
            for entry in entries {
                let txid = entry.tx.txid();
                let sid = short_txid(&txid, k0, k1);
                if let Ok(bytes) = serde_json::to_vec(&entry.tx) {
                    map.insert(sid, bytes);
                }
            }
            map
        };
        // The blocktxn response lists transactions positionally in the order
        // they were requested. Recompute the requested (absolute) indexes the
        // same way the getblocktxn request was built, then map positions to
        // absolute indexes — decode_with_received looks up by absolute index.
        let requested_indexes: Vec<usize> = {
            match CompactBlockDecoder::decode_with_received(&pending, &mempool_map, &HashMap::new())
            {
                Err(CompactBlockDecodeError::MissingTransactions(indexes)) => {
                    indexes.into_iter().map(|i| i as usize).collect()
                }
                _ => {
                    tracing::debug!(
                        "blocktxn: pending block {} no longer missing txs",
                        hex::encode(resp.block_hash)
                    );
                    return Ok(());
                }
            }
        };
        let mut received_map = std::collections::HashMap::new();
        for (pos, tx_bytes) in resp.transactions.iter().enumerate() {
            if let Some(&abs_index) = requested_indexes.get(pos) {
                received_map.insert(abs_index, tx_bytes.clone());
            }
        }

        match CompactBlockDecoder::decode_with_received(&pending, &mempool_map, &received_map) {
            Ok(tx_bytes_list) => {
                let mut txs: Vec<Transaction> = Vec::new();
                let mut all_ok = true;
                for bytes in &tx_bytes_list {
                    match serde_json::from_slice::<Transaction>(bytes) {
                        Ok(tx) => txs.push(tx),
                        Err(e) => {
                            tracing::warn!(
                                "blocktxn: failed to decode tx from {}: {}",
                                peer_addr,
                                e
                            );
                            all_ok = false;
                            break;
                        }
                    }
                }
                if all_ok {
                    let stake_modifier = {
                        let chain = node.chain.lock().await;
                        chain
                            .get_block(&pending.prev_block_hash)
                            .map(|p| {
                                compute_stake_modifier(
                                    p.header.stake_modifier,
                                    &pending.prev_block_hash,
                                )
                            })
                            .unwrap_or(0)
                    };
                    let block = Block {
                        header: BlockHeader {
                            version: pending.version,
                            prev_block_hash: pending.prev_block_hash,
                            merkle_root: pending.merkle_root,
                            timestamp: pending.timestamp,
                            bits: pending.bits,
                            nonce: pending.nonce,
                            stake_modifier,
                        },
                        transactions: txs,
                    };
                    let hash = block.hash();
                    let tx_count = block.transactions.len();
                    let timestamp = block.header.timestamp;
                    let size_bytes = serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
                    let block_arc = std::sync::Arc::new(block);
                    let mut chain = node.chain.lock().await;
                    match chain.add_block((*block_arc).clone()) {
                        Ok(acceptance) => {
                            if let BlockAcceptance::MainChain {
                                height,
                                utxos_added,
                                utxos_removed,
                                claimed_addresses,
                            } = acceptance
                            {
                                tracing::info!(
                                    "blocktxn: accepted block {} at height {}",
                                    hex::encode(hash),
                                    height
                                );
                                {
                                    let confirmed: Vec<[u8; 32]> =
                                        block_arc.transactions.iter().map(|tx| tx.txid()).collect();
                                    let mut mp = node.mempool.lock().await;
                                    mp.handle_confirmed_block(&confirmed, &utxos_removed);
                                }
                                node.emit(NodeEvent::NewBlock {
                                    height,
                                    hash,
                                    tx_count,
                                    timestamp,
                                    size_bytes,
                                    block: block_arc.clone(),
                                    utxos_added,
                                    utxos_removed,
                                    claimed_addresses,
                                });
                                for tx in block_arc.transactions.iter() {
                                    node.emit(NodeEvent::TxConfirmed {
                                        txid: tx.txid(),
                                        block_height: height,
                                        block_hash: hash,
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "cmpctblock: rejected completed block from {}: {}",
                                peer_addr,
                                e
                            );
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "blocktxn: failed to complete block from {}: {:?}",
                    peer_addr,
                    e
                );
            }
        }
    }
    Ok(())
}

/// Handle a `getdata` message — serve blocks and transactions to requesting peers.
pub(crate) async fn handle_getdata(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
    peer_version: u32,
) -> Result<()> {
    if let Ok(req) = decode_for_peer::<GetDataMsg>(&msg.payload, peer_version) {
        // Bound the request: each item can trigger a full block (up to 1 MB)
        // or transaction response, so a large list is a bandwidth DoS vector.
        const MAX_GETDATA_ITEMS: usize = 500;
        if req.items.len() > MAX_GETDATA_ITEMS {
            tracing::debug!(
                "getdata from {} with {} items — rejecting (max {})",
                peer_addr,
                req.items.len(),
                MAX_GETDATA_ITEMS
            );
            node.peer_manager
                .record_misbehaviour(peer_addr, Misbehaviour::OversizedMessage)
                .await;
            return Ok(());
        }
        for item in &req.items {
            match item.inv_type {
                InvType::Block => {
                    let maybe_block = {
                        let chain = node.chain.lock().await;
                        chain.get_block(&item.hash).cloned()
                    };
                    if let Some(block) = maybe_block {
                        let payload = node.serialize_block_for_peer(&block, peer_version);
                        if !payload.is_empty() {
                            node.peer_manager
                                .send_to(peer_addr, NetMessage::new("block", payload))
                                .await;
                            tracing::debug!(
                                "getdata: served block {} to {} (v2={})",
                                hex::encode(item.hash),
                                peer_addr,
                                crate::node::p2p::is_v2_peer_version(peer_version)
                            );
                        }
                    } else {
                        let nf = encode_for_peer(
                            &InvMsg {
                                items: vec![item.clone()],
                            },
                            peer_version,
                        );
                        node.peer_manager
                            .send_to(peer_addr, NetMessage::new("notfound", nf))
                            .await;
                    }
                }
                InvType::Transaction => {
                    let maybe_tx = {
                        let mp = node.mempool.lock().await;
                        mp.get_transaction(&item.hash).cloned()
                    };
                    if let Some(tx) = maybe_tx {
                        let payload = node.serialize_tx_for_peer(&tx, peer_version);
                        if !payload.is_empty() {
                            node.peer_manager
                                .send_to(peer_addr, NetMessage::new("tx", payload))
                                .await;
                            tracing::debug!(
                                "getdata: served tx {} to {} (v2={})",
                                hex::encode(item.hash),
                                peer_addr,
                                crate::node::p2p::is_v2_peer_version(peer_version)
                            );
                        }
                    } else {
                        let nf = encode_for_peer(
                            &InvMsg {
                                items: vec![item.clone()],
                            },
                            peer_version,
                        );
                        node.peer_manager
                            .send_to(peer_addr, NetMessage::new("notfound", nf))
                            .await;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Handle a `getheaders` message — serve block headers.
pub(crate) async fn handle_getheaders(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
    peer_version: u32,
) -> Result<()> {
    if let Ok(req) = decode_for_peer::<GetHeadersMsg>(&msg.payload, peer_version) {
        const MAX_LOCATOR_HASHES: usize = 64;
        if req.block_locator_hashes.len() > MAX_LOCATOR_HASHES {
            tracing::debug!(
                "getheaders from {} with {} locator hashes — rejecting",
                peer_addr,
                req.block_locator_hashes.len()
            );
            node.peer_manager
                .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                .await;
        } else {
            let chain = node.chain.lock().await;
            let our_height = chain.best_height();

            let locator: std::collections::HashSet<[u8; 32]> =
                req.block_locator_hashes.iter().copied().collect();
            let mut start_height = 1u32;
            for h in (1..=our_height).rev() {
                if let Some(b) = chain.get_block_at_height(h) {
                    if locator.contains(&b.hash()) {
                        start_height = h + 1;
                        break;
                    }
                }
            }

            let mut headers: Vec<HeaderEntry> = Vec::new();
            for h in start_height..=our_height.min(start_height + super::HEADERS_PER_BATCH as u32) {
                if let Some(block) = chain.get_block_at_height(h) {
                    let hash = block.hash();
                    if req.hash_stop != [0u8; 32] && hash == req.hash_stop {
                        let header_bytes = bincode::serialize(&block.header).unwrap_or_default();
                        headers.push(HeaderEntry {
                            header: header_bytes,
                            tx_count: 0,
                        });
                        break;
                    }
                    let header_bytes = bincode::serialize(&block.header).unwrap_or_default();
                    headers.push(HeaderEntry {
                        header: header_bytes,
                        tx_count: 0,
                    });
                }
            }

            if !headers.is_empty() {
                let headers_msg = HeadersMsg { headers };
                let payload = encode_for_peer(&headers_msg, peer_version);
                drop(chain);
                node.peer_manager
                    .send_to(peer_addr, NetMessage::new("headers", payload))
                    .await;
            }
        }
    }
    Ok(())
}

/// Handle a `headers` message — process received headers and request missing blocks.
pub(crate) async fn handle_headers(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: &NetMessage,
    peer_version: u32,
) -> Result<()> {
    if let Ok(resp) = decode_for_peer::<HeadersMsg>(&msg.payload, peer_version) {
        let count = resp.headers.len();
        if count == 0 {
            return Ok(());
        }
        tracing::debug!("headers: received {} headers from {}", count, peer_addr);

        let decoded: Vec<BlockHeader> = resp
            .headers
            .iter()
            .filter_map(|h| bincode::deserialize::<BlockHeader>(&h.header).ok())
            .collect();

        let want: Vec<InvItem> = {
            let chain = node.chain.lock().await;
            decoded
                .iter()
                .map(|hdr| hdr.hash())
                .filter(|hash| chain.get_block(hash).is_none())
                .map(|hash| InvItem {
                    inv_type: InvType::Block,
                    hash,
                })
                .collect()
        };

        if !want.is_empty() {
            let payload = encode_for_peer(
                &vtorrent_p2p::message::GetDataMsg { items: want },
                peer_version,
            );
            node.peer_manager
                .send_to(peer_addr, NetMessage::new("getdata", payload))
                .await;
        }

        if count == super::HEADERS_PER_BATCH {
            let last_hash = decoded.last().map(|hdr| hdr.hash()).unwrap_or([0u8; 32]);
            let gh_msg = GetHeadersMsg {
                version: PROTOCOL_VERSION,
                block_locator_hashes: vec![last_hash],
                hash_stop: [0u8; 32],
            };
            let payload = encode_for_peer(&gh_msg, peer_version);
            node.peer_manager
                .send_to(peer_addr, NetMessage::new("getheaders", payload))
                .await;
        }
    }
    Ok(())
}

/// Handle a `getblocktxn` request — a thin wrapper around the inner struct.
#[derive(serde::Deserialize)]
pub(crate) struct BlockTxnReq {
    pub block_hash: [u8; 32],
    pub indexes: Vec<u16>,
}
