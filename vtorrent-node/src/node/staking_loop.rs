//! PoS block production loop.
//!
//! Extracted from `node/mod.rs`: the per-tick staking attempt that builds a
//! coinstake block when a kernel meets the target, applies it to the chain,
//! emits persistence/confirmation events, and announces it to peers.

use vtorrent_p2p::message::{InvItem, InvMsg, InvType, NetMessage};

use crate::{
    consensus::TARGET_BLOCK_TIME,
    error::{NodeError, Result},
    events::NodeEvent,
};

use super::Node;
use vtorrent_core::time::now_timestamp_u32;

impl Node {
    /// Attempt to produce a new PoS block.
    pub(crate) async fn attempt_stake(&mut self) -> Result<()> {
        let staking = self
            .staking
            .as_ref()
            .ok_or_else(|| NodeError::Chain("Staking not enabled".into()))?;

        let (best_height, best_hash, best_timestamp, best_stake_modifier, stake_utxos) = {
            let chain = self.chain.lock().await;
            let best_height = chain.best_height();
            let best_hash = chain.best_hash().unwrap_or([0u8; 32]);
            let best_block = chain.get_block_at_height(best_height);
            let best_timestamp = best_block.map(|b| b.header.timestamp).unwrap_or(0);
            let best_stake_modifier = best_block.map(|b| b.header.stake_modifier).unwrap_or(0);
            let utxos = chain.get_utxos_for_address(&staking.address);
            (
                best_height,
                best_hash,
                best_timestamp,
                best_stake_modifier,
                utxos,
            )
        };

        tracing::trace!(
            "Stake tick: evaluating {} wallet UTXOs for address {}",
            stake_utxos.len(),
            staking.address
        );
        if stake_utxos.is_empty() {
            tracing::trace!("Stake tick: no UTXOs available");
            return Ok(());
        }

        let now = now_timestamp_u32();

        if now <= best_timestamp + TARGET_BLOCK_TIME as u32 {
            return Ok(());
        }

        let Some(kernel) = staking.find_stake_kernel(
            best_stake_modifier,
            best_height + 1,
            now,
            stake_utxos.iter(),
        ) else {
            tracing::trace!(
                "Stake tick: no kernel met target (height {}, now {})",
                best_height + 1,
                now
            );
            return Ok(());
        };

        // Only include pending txs whose inputs are still unspent in the
        // current UTXO set — mempool entries can go stale when a competing
        // block confirms the same inputs, and including them would make our
        // block invalid.
        let pending_txs = {
            let chain = self.chain.lock().await;
            let mempool = self.mempool.lock().await;
            mempool
                .get_transactions()
                .into_iter()
                .filter(|tx| chain.compute_tx_fee(tx).is_some())
                .collect()
        };

        let block_opt = {
            let chain = self.chain.lock().await;
            if chain.best_hash() != Some(best_hash) {
                return Ok(());
            }
            staking.build_from_kernel_with_proof(
                best_hash,
                best_stake_modifier,
                best_height + 1,
                now,
                chain.get_utxo_set(),
                pending_txs,
                kernel,
            )
        };
        if let Some((block, proof)) = block_opt {
            let block_hash = block.hash();
            let tx_count = block.transactions.len();
            let timestamp = block.header.timestamp;
            let size_bytes = serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
            let block_arc = std::sync::Arc::new(block);
            let acceptance = {
                let mut chain = self.chain.lock().await;
                let result = chain.add_block((*block_arc).clone())?;
                tracing::info!(
                    "Staked new block {} at height {}",
                    hex::encode(block_hash),
                    chain.best_height()
                );
                result
            };

            // Emit NewBlock event (carries UTXO diff for BlockStore persistence)
            use crate::chain::BlockAcceptance;
            if let BlockAcceptance::MainChain {
                height,
                utxos_added,
                utxos_removed,
                claimed_addresses,
            } = acceptance
            {
                self.cache_stake_proof(block_hash, proof).await;
                {
                    let confirmed: Vec<[u8; 32]> =
                        block_arc.transactions.iter().map(|tx| tx.txid()).collect();
                    let mut mp = self.mempool.lock().await;
                    mp.handle_confirmed_block(&confirmed, &utxos_removed);
                }
                self.emit(NodeEvent::NewBlock {
                    height,
                    hash: block_hash,
                    tx_count,
                    timestamp,
                    size_bytes,
                    block: block_arc.clone(),
                    utxos_added,
                    utxos_removed,
                    claimed_addresses,
                });
                for tx in block_arc.transactions.iter() {
                    self.emit(NodeEvent::TxConfirmed {
                        txid: tx.txid(),
                        block_height: height,
                        block_hash,
                    });
                }
                // Emit StakingReward event
                let reward_sats: u64 = block_arc
                    .transactions
                    .iter()
                    .filter(|tx| matches!(tx.tx_type, crate::block::TxType::Coinstake))
                    .flat_map(|tx| tx.outputs.iter())
                    .map(|o| o.value)
                    .sum();
                let staking_addr = staking.address.clone();
                self.emit(NodeEvent::StakingReward {
                    block_height: height,
                    reward_sats,
                    address: staking_addr,
                });
            }

            // Announce to peers
            let payload = serde_json::to_vec(&InvMsg {
                items: vec![InvItem {
                    inv_type: InvType::Block,
                    hash: block_hash,
                }],
            })
            .unwrap_or_default();
            self.peer_manager
                .broadcast(NetMessage::new("inv", payload))
                .await;
        }

        Ok(())
    }
}
