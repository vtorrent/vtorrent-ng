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
        decode_for_peer, encode_for_peer, BlockTxnMsg, CmpctBlockMsg, GetBlocksMsg, GetDataMsg,
        GetHeadersMsg, GetProofMsg, HeaderEntry, HeadersMsg, InvItem, InvMsg, InvType, NetMessage,
        ProofMsg, PROTOCOL_VERSION,
    },
};

use crate::{
    block::{Block, BlockHeader, Transaction},
    chain::BlockAcceptance,
    error::{NodeError, Result},
    events::NodeEvent,
};

use super::Node;

fn reconstruct_compact_block(compact: &CmpctBlockMsg, transactions: Vec<Transaction>) -> Block {
    Block {
        header: BlockHeader {
            version: compact.version,
            prev_block_hash: compact.prev_block_hash,
            merkle_root: compact.merkle_root,
            utxo_root: compact.utxo_root,
            timestamp: compact.timestamp,
            bits: compact.bits,
            nonce: compact.nonce,
            stake_modifier: compact.stake_modifier,
        },
        transactions,
    }
}

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
            let acceptance = chain.add_block((*block_arc).clone());
            drop(chain);
            match acceptance {
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
                            if block_arc.header.is_pos() {
                                let payload = encode_for_peer(
                                    &GetProofMsg { block_hash: hash },
                                    peer_version,
                                );
                                node.peer_manager
                                    .send_to(peer_addr, NetMessage::new("getproof", payload))
                                    .await;
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
                                    let _ = mp.admit_with_chain_fee(&chain, tx);
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
            let chain = node.chain.lock().await;
            let mut mp = node.mempool.lock().await;
            let result = mp.admit_with_chain_fee(&chain, tx.clone());
            drop(mp);
            drop(chain);
            match result {
                Ok(fee) => {
                    let txid = tx.txid();
                    let size_bytes = tx.serialized_size();
                    tracing::debug!("Accepted tx {}", hex::encode(txid));
                    node.emit(NodeEvent::TxUnconfirmed {
                        txid,
                        fee_sats: fee,
                        size_bytes,
                    });
                    let inv_msg = InvMsg {
                        items: vec![InvItem {
                            inv_type: InvType::Transaction,
                            hash: txid,
                        }],
                    };
                    let payload = encode_for_peer(&inv_msg, peer_version);
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
    if let Ok(cmpct) = serde_json::from_slice::<CmpctBlockMsg>(&msg.payload) {
        let mut header_bytes = Vec::with_capacity(120);
        header_bytes.extend_from_slice(&cmpct.version.to_le_bytes());
        header_bytes.extend_from_slice(&cmpct.prev_block_hash);
        header_bytes.extend_from_slice(&cmpct.merkle_root);
        header_bytes.extend_from_slice(&cmpct.utxo_root);
        header_bytes.extend_from_slice(&cmpct.timestamp.to_le_bytes());
        header_bytes.extend_from_slice(&cmpct.bits.to_le_bytes());
        header_bytes.extend_from_slice(&cmpct.nonce.to_le_bytes());
        header_bytes.extend_from_slice(&cmpct.stake_modifier.to_le_bytes());
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
                    let block = reconstruct_compact_block(&cmpct, txs);
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
                let probe_block_hash = { reconstruct_compact_block(&cmpct, Vec::new()).hash() };
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

        let mut header_bytes = Vec::with_capacity(120);
        header_bytes.extend_from_slice(&pending.version.to_le_bytes());
        header_bytes.extend_from_slice(&pending.prev_block_hash);
        header_bytes.extend_from_slice(&pending.merkle_root);
        header_bytes.extend_from_slice(&pending.utxo_root);
        header_bytes.extend_from_slice(&pending.timestamp.to_le_bytes());
        header_bytes.extend_from_slice(&pending.bits.to_le_bytes());
        header_bytes.extend_from_slice(&pending.nonce.to_le_bytes());
        header_bytes.extend_from_slice(&pending.stake_modifier.to_le_bytes());
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
                    let block = reconstruct_compact_block(&pending, txs);
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
        // Bound the batch: each header triggers a getdata item (full block
        // fetch), so an oversized announcement is a bandwidth DoS vector.
        if count > super::HEADERS_PER_BATCH {
            tracing::debug!(
                "headers from {} with {} entries — rejecting (max {})",
                peer_addr,
                count,
                super::HEADERS_PER_BATCH
            );
            node.peer_manager
                .record_misbehaviour(peer_addr, Misbehaviour::OversizedMessage)
                .await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockHeader, TxInput, TxOutput, TxType};
    use crate::consensus::compute_stake_modifier;
    use crate::node::NodeConfig;
    use vtorrent_p2p::message::PROTOCOL_VERSION;

    fn test_node() -> Node {
        let config = NodeConfig {
            isolated: true,
            use_dht: false,
            use_overlay: false,
            ..NodeConfig::default()
        };
        Node::new(config).expect("test node creation failed")
    }

    fn coinbase_block(prev_hash: [u8; 32], prev_modifier: u64, height: u32) -> Block {
        let tx = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![TxInput {
                prev_txid: [0u8; 32],
                prev_vout: 0xffffffff,
                script_sig: vec![height as u8],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: 1_000_000,
                script_pubkey: vec![
                    0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0x88, 0xac,
                ],
            }],
            lock_time: height,
            claim_address: None,
            claim_signature: None,
        };
        let mut block = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: prev_hash,
                merkle_root: [0u8; 32],
                utxo_root: [0u8; 32],
                timestamp: 1_700_000_000 + height,
                bits: crate::genesis::GENESIS_BITS,
                nonce: height,
                stake_modifier: compute_stake_modifier(prev_modifier, &prev_hash),
            },
            transactions: vec![tx],
        };
        block.header.merkle_root = block.compute_merkle_root();
        block
    }

    fn v2_msg<T: serde::Serialize>(command: &str, body: &T) -> NetMessage {
        NetMessage::new(command, vtorrent_p2p::message::encode_v2(body).unwrap())
    }

    fn peer(port: u16) -> SocketAddr {
        format!("127.0.0.1:{}", port).parse().unwrap()
    }

    #[test]
    fn compact_reconstruction_preserves_committed_header() {
        let compact = CmpctBlockMsg {
            version: 3,
            prev_block_hash: [1; 32],
            merkle_root: [2; 32],
            utxo_root: [3; 32],
            timestamp: 1_700_000_000,
            bits: crate::genesis::GENESIS_BITS,
            nonce: 0,
            stake_modifier: 42,
            siphash_nonce: 7,
            short_ids: Vec::new(),
            prefilled_txs: Vec::new(),
        };
        let block = reconstruct_compact_block(&compact, Vec::new());
        assert_eq!(block.header.utxo_root, compact.utxo_root);
        assert_eq!(block.header.stake_modifier, compact.stake_modifier);
    }

    #[tokio::test]
    async fn handle_block_accepts_valid_block_and_updates_chain() {
        let mut node = test_node();
        let genesis_hash = {
            let chain = node.chain.lock().await;
            chain.best_hash().unwrap()
        };
        let block = coinbase_block(genesis_hash, 0, 1);
        let payload = node.serialize_block_for_peer(&block, PROTOCOL_VERSION);
        let msg = NetMessage::new("block", payload);

        handle_block(&mut node, peer(1), &msg, PROTOCOL_VERSION)
            .await
            .unwrap();

        let chain = node.chain.lock().await;
        assert_eq!(chain.best_height(), 1);
        let stored = chain.get_block_at_height(1).unwrap();
        assert_eq!(chain.best_hash().unwrap(), stored.hash());
    }

    #[tokio::test]
    async fn handle_block_rejects_invalid_block_and_bans_peer() {
        let mut node = test_node();
        // A block whose parent is unknown (not the genesis hash) fails validation.
        let block = coinbase_block([0xaa; 32], 0, 1);
        let payload = node.serialize_block_for_peer(&block, PROTOCOL_VERSION);
        let msg = NetMessage::new("block", payload);
        let addr = peer(2);

        handle_block(&mut node, addr, &msg, PROTOCOL_VERSION)
            .await
            .unwrap();

        assert!(
            node.peer_manager.is_banned(addr).await,
            "peer sending an invalid block must be banned"
        );
        let chain = node.chain.lock().await;
        assert_eq!(chain.best_height(), 0);
    }

    #[tokio::test]
    async fn handle_block_malformed_payload_records_misbehaviour() {
        let mut node = test_node();
        let addr = peer(3);
        let msg = NetMessage::new("block", vec![0xde, 0xad, 0xbe, 0xef]);

        handle_block(&mut node, addr, &msg, PROTOCOL_VERSION)
            .await
            .unwrap();

        let chain = node.chain.lock().await;
        assert_eq!(chain.best_height(), 0);
        assert!(!node.peer_manager.is_banned(addr).await);
    }

    #[tokio::test]
    async fn handle_tx_rejects_unsigned_transaction() {
        let mut node = test_node();
        // Fund an address via a coinbase block, then send a transfer spending it.
        let genesis_hash = {
            let chain = node.chain.lock().await;
            chain.best_hash().unwrap()
        };
        let block = coinbase_block(genesis_hash, 0, 1);
        {
            let mut chain = node.chain.lock().await;
            chain.add_block(block).unwrap();
        }
        let (coinbase_txid, utxo) = {
            let chain = node.chain.lock().await;
            let cb = chain.get_block_at_height(1).unwrap().transactions[0].clone();
            let txid = cb.txid();
            let utxo = chain.get_utxo(&txid, 0).unwrap().clone();
            (txid, utxo)
        };

        let tx = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: coinbase_txid,
                prev_vout: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value: utxo.value - 1_000,
                script_pubkey: utxo.script_pubkey.clone(),
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let payload = bincode::serialize(&tx).unwrap();
        let msg = NetMessage::new("tx", payload);

        handle_tx(&mut node, peer(3), &msg, PROTOCOL_VERSION)
            .await
            .unwrap();

        let mp = node.mempool.lock().await;
        assert!(mp.get_transaction(&tx.txid()).is_none());
    }

    #[tokio::test]
    async fn handle_tx_rejects_unknown_input_and_records_misbehaviour() {
        let mut node = test_node();
        let addr = peer(4);
        let tx = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: [0x99; 32],
                prev_vout: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value: 1_000,
                script_pubkey: vec![0x51],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let payload = bincode::serialize(&tx).unwrap();
        let msg = NetMessage::new("tx", payload);

        handle_tx(&mut node, addr, &msg, PROTOCOL_VERSION)
            .await
            .unwrap();

        let mp = node.mempool.lock().await;
        assert!(mp.get_transaction(&tx.txid()).is_none());
    }

    #[tokio::test]
    async fn handle_inv_requests_unknown_blocks_and_txs() {
        let mut node = test_node();
        // Parent must exist: build block 1 on genesis first.
        let genesis_hash = {
            let chain = node.chain.lock().await;
            chain.best_hash().unwrap()
        };
        let block = coinbase_block(genesis_hash, 0, 1);
        {
            let mut chain = node.chain.lock().await;
            chain.add_block(block).unwrap();
        }
        let known_block_hash = {
            let chain = node.chain.lock().await;
            chain.get_block_at_height(1).unwrap().hash()
        };
        let unknown_block: [u8; 32] = [0x11; 32];
        let unknown_tx: [u8; 32] = [0x22; 32];

        let inv = InvMsg {
            items: vec![
                InvItem {
                    inv_type: InvType::Block,
                    hash: known_block_hash,
                },
                InvItem {
                    inv_type: InvType::Block,
                    hash: unknown_block,
                },
                InvItem {
                    inv_type: InvType::Transaction,
                    hash: unknown_tx,
                },
            ],
        };
        let msg = v2_msg("inv", &inv);

        handle_inv(&mut node, peer(5), &msg, PROTOCOL_VERSION)
            .await
            .unwrap();
        // No panic and no misbehaviour: the handler filters known items internally.
        let score = node
            .peer_manager
            .ban_manager
            .read()
            .await
            .score(peer(5).ip());
        assert_eq!(score, 0);
        assert!(!node.peer_manager.is_banned(peer(5)).await);
    }

    #[tokio::test]
    async fn handle_inv_oversized_inventory_is_penalized() {
        let mut node = test_node();
        let addr = peer(6);
        let items: Vec<InvItem> = (0u32..1_001)
            .map(|i| InvItem {
                inv_type: InvType::Transaction,
                hash: [i as u8; 32],
            })
            .collect();
        let msg = v2_msg("inv", &InvMsg { items });

        handle_inv(&mut node, addr, &msg, PROTOCOL_VERSION)
            .await
            .unwrap();

        // A single oversized inv scores 20 points (ban threshold is 100), so
        // the peer is not banned outright but the misbehaviour is recorded.
        let score = node.peer_manager.ban_manager.read().await.score(addr.ip());
        assert_eq!(score, 20, "oversized inv must record misbehaviour");
    }

    #[tokio::test]
    async fn handle_getheaders_returns_headers() {
        let mut node = test_node();
        let genesis_hash = {
            let chain = node.chain.lock().await;
            chain.best_hash().unwrap()
        };
        let block = coinbase_block(genesis_hash, 0, 1);
        {
            let mut chain = node.chain.lock().await;
            chain.add_block(block).unwrap();
        }

        let msg = v2_msg(
            "getheaders",
            &GetHeadersMsg {
                version: PROTOCOL_VERSION,
                block_locator_hashes: vec![genesis_hash],
                hash_stop: [0u8; 32],
            },
        );
        handle_getheaders(&mut node, peer(7), &msg, PROTOCOL_VERSION)
            .await
            .unwrap();
        // No panic and no ban: response goes through the peer manager.
        assert!(!node.peer_manager.is_banned(peer(7)).await);
    }

    #[tokio::test]
    async fn handle_getdata_unknown_item_is_tolerated() {
        let mut node = test_node();
        let msg = v2_msg(
            "getdata",
            &GetDataMsg {
                items: vec![InvItem {
                    inv_type: InvType::Block,
                    hash: [0x33; 32],
                }],
            },
        );
        handle_getdata(&mut node, peer(8), &msg, PROTOCOL_VERSION)
            .await
            .unwrap();
        assert!(!node.peer_manager.is_banned(peer(8)).await);
    }

    #[tokio::test]
    async fn handle_blocktxn_without_pending_compact_block_is_tolerated() {
        let mut node = test_node();
        let msg = NetMessage::new(
            "blocktxn",
            serde_json::to_vec(&BlockTxnMsg {
                block_hash: [0x44; 32],
                transactions: vec![],
            })
            .unwrap(),
        );
        handle_blocktxn(&mut node, peer(9), &msg).await.unwrap();
        assert!(!node.peer_manager.is_banned(peer(9)).await);
    }
}

/// Rate-limit and dispatch an inbound P2P message.
///
/// Extracted from `Node::handle_message` so the routing table is testable
/// independently of the connection loop. `MAX_MSGS_PER_WINDOW` and
/// `MSG_WINDOW_SECS` are re-exported from `super`.
pub(crate) async fn dispatch_message(
    node: &mut Node,
    peer_addr: SocketAddr,
    msg: NetMessage,
) -> Result<()> {
    use super::{MAX_MSGS_PER_WINDOW, MSG_WINDOW_SECS};
    use crate::atomic_swap::OrderAnnouncement;
    use vtorrent_p2p::ban_manager::Misbehaviour;
    use vtorrent_p2p::message::{AddrMsg, FeeFilterMsg, PingMsg, SendCmpctMsg};

    // V2 wire sniffing: bincode for V2 peers (>=2, not legacy 70001), JSON fallback.
    // Unknown commands are ignored (not banned) to allow rolling upgrades.
    let peer_version = node
        .peer_versions
        .get(&peer_addr)
        .copied()
        .unwrap_or(vtorrent_p2p::message::LEGACY_PROTOCOL_VERSION);

    // Per-peer flood rate limiting: a peer that exceeds the message budget
    // within a window is banned and disconnected.
    let now = super::now_secs();
    let (count, window_start) = node.peer_msg_counts.entry(peer_addr).or_insert((0, now));
    if now.saturating_sub(*window_start) >= MSG_WINDOW_SECS {
        *count = 0;
        *window_start = now;
    }
    *count += 1;
    if *count > MAX_MSGS_PER_WINDOW {
        tracing::warn!(
            "Peer {} exceeded {} messages/{}s; banning",
            peer_addr,
            MAX_MSGS_PER_WINDOW,
            MSG_WINDOW_SECS
        );
        node.peer_manager
            .record_misbehaviour(peer_addr, Misbehaviour::Custom(100))
            .await;
        return Ok(());
    }

    match msg.command_str() {
        // ── PEX: Peer Exchange ────────────────────────────────────────────
        "addr" => {
            if let Ok(mut addr_msg) = serde_json::from_slice::<AddrMsg>(&msg.payload) {
                // Truncate oversized announcements: the address book caps
                // at 10k entries anyway, so anything beyond MAX_ADDR_PER_MSG
                // per message is wasted parse work from an untrusted peer.
                if addr_msg.addrs.len() > vtorrent_p2p::pex::MAX_ADDR_PER_MSG {
                    tracing::debug!(
                        "addr from {} with {} entries — truncating to {}",
                        peer_addr,
                        addr_msg.addrs.len(),
                        vtorrent_p2p::pex::MAX_ADDR_PER_MSG
                    );
                    addr_msg.addrs.truncate(vtorrent_p2p::pex::MAX_ADDR_PER_MSG);
                }
                let count = addr_msg.addrs.len();
                node.peer_manager.handle_addr_msg(&addr_msg);
                tracing::debug!("PEX: Received {} addresses from {}", count, peer_addr);
            } else {
                node.peer_manager
                    .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                    .await;
            }
        }

        "getaddr" => {
            // Respond with our known peer list
            let response = node.peer_manager.build_addr_response();
            node.peer_manager.send_to(peer_addr, response).await;
            tracing::debug!("PEX: Sent addr response to {}", peer_addr);
        }

        // ── Inventory (V2 bincode with JSON fallback) ─────────────────────
        "inv" => {
            handle_inv(node, peer_addr, &msg, peer_version).await?;
        }

        "block" => {
            handle_block(node, peer_addr, &msg, peer_version).await?;
        }

        "tx" => {
            handle_tx(node, peer_addr, &msg, peer_version).await?;
        }

        "getblocks" => {
            handle_getblocks(node, peer_addr, &msg, peer_version).await?;
        }

        // ── Compact Block Relay (BIP-152) ─────────────────────────────────
        "sendcmpct" => {
            if let Ok(msg_data) = serde_json::from_slice::<SendCmpctMsg>(&msg.payload) {
                let state = node.compact_peers.entry(peer_addr).or_default();
                state.enabled = true;
                state.high_bandwidth = msg_data.high_bandwidth;
                state.version = msg_data.version;
                tracing::debug!(
                    "Peer {} supports compact blocks (high_bw={}, v={})",
                    peer_addr,
                    msg_data.high_bandwidth,
                    msg_data.version
                );
            }
        }

        "cmpctblock" => {
            handle_cmpctblock(node, peer_addr, &msg).await?;
        }

        "getblocktxn" => {
            handle_getblocktxn(node, peer_addr, &msg).await?;
        }

        "blocktxn" => {
            handle_blocktxn(node, peer_addr, &msg).await?;
        }

        // ── Keepalive ─────────────────────────────────────────────────────
        "ping" => {
            // peer.rs already handles inbound ping→pong at the peer level;
            // this arm handles any ping that bubbles up (e.g. from test harness).
            if let Ok(ping) = serde_json::from_slice::<PingMsg>(&msg.payload) {
                let payload =
                    serde_json::to_vec(&PingMsg { nonce: ping.nonce }).unwrap_or_default();
                node.peer_manager
                    .send_to(peer_addr, NetMessage::new("pong", payload))
                    .await;
            }
        }

        "pong" => {
            // Validate the nonce matches what we sent
            if let Ok(pong) = serde_json::from_slice::<PingMsg>(&msg.payload) {
                if let Some(&expected) = node.peer_ping_nonces.get(&peer_addr) {
                    if pong.nonce == expected {
                        node.peer_ping_nonces.remove(&peer_addr);
                        tracing::trace!("Pong from {} confirmed (nonce={})", peer_addr, pong.nonce);
                    } else {
                        tracing::warn!(
                            "Pong nonce mismatch from {}: expected {} got {}",
                            peer_addr,
                            expected,
                            pong.nonce
                        );
                    }
                }
            }
        }

        // ── Fee filter ────────────────────────────────────────────────────
        "feefilter" => {
            if let Ok(ff) = serde_json::from_slice::<FeeFilterMsg>(&msg.payload) {
                node.peer_fee_filters.insert(peer_addr, ff.feerate);
                tracing::debug!(
                    "feefilter: peer {} min fee rate = {} sat/kB",
                    peer_addr,
                    ff.feerate
                );
            }
        }

        // ── Not-found ─────────────────────────────────────────────────────
        "notfound" => {
            if let Ok(nf) = serde_json::from_slice::<InvMsg>(&msg.payload) {
                for item in &nf.items {
                    tracing::debug!(
                        "notfound: peer {} does not have {:?} {}",
                        peer_addr,
                        item.inv_type,
                        hex::encode(item.hash)
                    );
                }
            }
        }

        // ── getdata: serve blocks and transactions to requesting peers ──────
        "getdata" => {
            handle_getdata(node, peer_addr, &msg, peer_version).await?;
        }

        // ── Header sync (getheaders / headers) ────────────────────────────
        "getheaders" => {
            handle_getheaders(node, peer_addr, &msg, peer_version).await?;
        }

        "headers" => {
            handle_headers(node, peer_addr, &msg, peer_version).await?;
        }

        "getproof" => {
            if let Ok(req) = decode_for_peer::<GetProofMsg>(&msg.payload, peer_version) {
                let proof = node
                    .stake_proofs
                    .read()
                    .await
                    .get(&req.block_hash)
                    .and_then(|proof| bincode::serialize(proof).ok());
                let payload = encode_for_peer(
                    &ProofMsg {
                        block_hash: req.block_hash,
                        proof,
                    },
                    peer_version,
                );
                node.peer_manager
                    .send_to(peer_addr, NetMessage::new("proof", payload))
                    .await;
            } else {
                node.peer_manager
                    .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                    .await;
            }
        }

        "proof" => {
            if let Ok(resp) = decode_for_peer::<ProofMsg>(&msg.payload, peer_version) {
                if let Some(bytes) = resp.proof {
                    use bincode::Options as _;
                    let decoded = bincode::options()
                        .with_fixint_encoding()
                        .with_limit(crate::consensus::MAX_BLOCK_SIZE as u64)
                        .deserialize::<vtorrent_spv::stake::StakeProof>(&bytes);
                    if let Ok(proof) = decoded {
                        let valid_for_known_block = {
                            let chain = node.chain.lock().await;
                            chain.get_block(&resp.block_hash).is_some_and(|block| {
                                let parent_root = chain
                                    .get_block(&block.header.prev_block_hash)
                                    .map(|parent| parent.header.utxo_root);
                                let leaf = vtorrent_spv::stake::hash_utxo(&proof.utxo);
                                block.transactions.first().is_some_and(|coinstake| {
                                    proof.coinstake.txid() == coinstake.txid()
                                        && proof
                                            .tx_merkle_proof
                                            .verify(&block.header.merkle_root)
                                            .is_ok()
                                        && parent_root.is_some_and(|root| {
                                            proof.utxo_proof.verify(&root, &leaf).is_ok()
                                        })
                                })
                            })
                        };
                        if valid_for_known_block {
                            node.cache_stake_proof(resp.block_hash, proof).await;
                        }
                    }
                }
            } else {
                node.peer_manager
                    .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                    .await;
            }
        }

        // ── DEX order gossip ─────────────────────────────────────────────
        "dexorder" => {
            if let Ok(ann) = serde_json::from_slice::<OrderAnnouncement>(&msg.payload) {
                let order_id = ann.order_id;
                if node.seen_orders.insert(order_id) {
                    if let Some(book) = &node.order_book {
                        book.write().await.add_order(ann.to_order());
                    }
                    // Re-broadcast to all peers except the sender.
                    let payload = serde_json::to_vec(&ann).unwrap_or_default();
                    for peer in node.peer_manager.connected_peers() {
                        if peer != peer_addr {
                            node.peer_manager
                                .send_to(peer, NetMessage::new("dexorder", payload.clone()))
                                .await;
                        }
                    }
                    tracing::debug!("DEX gossip: received order {}", hex::encode(order_id));
                }
            } else {
                node.peer_manager
                    .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                    .await;
            }
        }

        cmd => {
            // Unknown commands are ignored for forward-compatible command additions.
            tracing::trace!("Unknown command '{}' from {} — ignored", cmd, peer_addr);
        }
    }
    Ok(())
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::node::{NodeConfig, MAX_MSGS_PER_WINDOW};
    use vtorrent_p2p::message::{FeeFilterMsg, PingMsg};

    fn test_node() -> Node {
        let config = NodeConfig {
            isolated: true,
            use_dht: false,
            use_overlay: false,
            ..NodeConfig::default()
        };
        Node::new(config).expect("test node creation failed")
    }

    fn peer(port: u16) -> SocketAddr {
        format!("127.0.0.1:{}", port).parse().unwrap()
    }

    #[tokio::test]
    async fn dispatch_unknown_command_is_ignored_not_banned() {
        let mut node = test_node();
        let addr = peer(20);
        let msg = NetMessage::new("totallyunknowncmd", vec![]);
        dispatch_message(&mut node, addr, msg).await.unwrap();
        let score = node
            .peer_manager
            .ban_manager
            .read()
            .await
            .score(peer(20).ip());
        assert_eq!(
            score, 0,
            "unknown commands must be ignored (rolling upgrades)"
        );
    }

    #[tokio::test]
    async fn dispatch_rate_limits_flooding_peers() {
        let mut node = test_node();
        let addr = peer(21);
        let msg = NetMessage::new("ping", serde_json::to_vec(&PingMsg { nonce: 1 }).unwrap());

        // MAX_MSGS_PER_WINDOW = 500; send 501 messages from the same peer.
        for _ in 0..MAX_MSGS_PER_WINDOW {
            dispatch_message(&mut node, addr, msg.clone())
                .await
                .unwrap();
        }
        // The next message trips the budget and bans the peer.
        dispatch_message(&mut node, addr, msg).await.unwrap();
        assert!(
            node.peer_manager.is_banned(addr).await,
            "peer exceeding the message budget must be banned"
        );
    }

    #[tokio::test]
    async fn dispatch_malformed_addr_scores_misbehaviour() {
        let mut node = test_node();
        let addr = peer(22);
        let msg = NetMessage::new("addr", vec![0xff; 64]);
        dispatch_message(&mut node, addr, msg).await.unwrap();
        let score = node.peer_manager.ban_manager.read().await.score(addr.ip());
        assert_eq!(score, 20, "malformed addr must score MalformedMessage (20)");
    }

    #[tokio::test]
    async fn dispatch_getaddr_is_tolerated() {
        let mut node = test_node();
        let msg = NetMessage::new("getaddr", vec![]);
        dispatch_message(&mut node, peer(22), msg).await.unwrap();
        assert!(!node.peer_manager.is_banned(peer(22)).await);
    }

    #[tokio::test]
    async fn dispatch_feefilter_records_peer_minimum() {
        let mut node = test_node();
        let addr = peer(23);
        let msg = NetMessage::new(
            "feefilter",
            serde_json::to_vec(&FeeFilterMsg { feerate: 250 }).unwrap(),
        );
        dispatch_message(&mut node, addr, msg).await.unwrap();
        assert_eq!(node.peer_fee_filters.get(&addr), Some(&250));
    }

    #[tokio::test]
    async fn dispatch_pong_with_matching_nonce_clears_pending_ping() {
        let mut node = test_node();
        let addr = peer(23);
        node.peer_ping_nonces.insert(addr, 4242);
        let msg = NetMessage::new(
            "pong",
            serde_json::to_vec(&PingMsg { nonce: 4242 }).unwrap(),
        );
        dispatch_message(&mut node, addr, msg).await.unwrap();
        assert!(
            !node.peer_ping_nonces.contains_key(&addr),
            "matching pong must clear the pending ping nonce"
        );
    }

    #[tokio::test]
    async fn dispatch_unknown_command_is_ignored() {
        let mut node = test_node();
        let msg = NetMessage::new("somefuturecmd", vec![0x01]);
        dispatch_message(&mut node, peer(22), msg).await.unwrap();
        let score = node
            .peer_manager
            .ban_manager
            .read()
            .await
            .score(peer(22).ip());
        assert_eq!(score, 0);
    }
}

#[cfg(test)]
mod deserialize_limit_tests {
    use super::*;
    use crate::node::NodeConfig;

    fn test_node() -> Node {
        let config = NodeConfig {
            isolated: true,
            use_dht: false,
            use_overlay: false,
            ..NodeConfig::default()
        };
        Node::new(config).expect("test node creation failed")
    }

    /// A bincode block payload declaring a huge Vec length must fail the size
    /// limit instead of attempting the allocation (memory-amplification DoS).
    #[tokio::test]
    async fn test_deserialize_block_rejects_oversized_declared_length() {
        let node = test_node();
        // Hand-craft a bincode body declaring u64::MAX elements.
        let mut crafted = Vec::new();
        crafted.extend_from_slice(&u64::MAX.to_le_bytes());
        crafted.extend_from_slice(&[0u8; 16]);

        assert!(node.deserialize_block(&crafted).is_err());
        assert!(node.deserialize_tx(&crafted).is_err());
    }

    #[tokio::test]
    async fn test_deserialize_block_roundtrip_still_works() {
        let node = test_node();
        let block = crate::genesis::create_genesis_block();
        let bytes = bincode::serialize(&block).unwrap();
        let decoded = node
            .deserialize_block(&bytes)
            .expect("valid block must decode");
        assert_eq!(decoded.hash(), block.hash());
    }
}
