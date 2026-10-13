//! PoS block production loop.
//!
//! Extracted from `node/mod.rs`: the per-tick staking attempt that builds a
//! coinstake block when a kernel meets the target, applies it to the chain,
//! emits persistence/confirmation events, and announces it to peers.

use vtorrent_p2p::message::{InvItem, InvMsg, InvType, NetMessage};

use crate::{
    error::{NodeError, Result},
    events::NodeEvent,
};

use super::Node;
use vtorrent_core::time::now_timestamp_u32;

impl Node {
    /// Attempt to produce a new PoS block.
    pub(crate) async fn attempt_stake(&mut self) -> Result<()> {
        if self.staking.is_none() {
            return Err(NodeError::Chain("Staking not enabled".into()));
        }

        let now = now_timestamp_u32();
        let (
            best_height,
            best_hash,
            best_timestamp,
            best_stake_modifier,
            stake_utxos,
            total_staked,
            tip_is_bootstrap,
            params,
        ) = {
            let chain = self.chain.lock().await;
            let params = chain.next_block_params(now);
            let total_staked = if params.min_stake_amount != chain.params().min_stake_amount {
                chain
                    .get_utxo_set()
                    .values()
                    .filter(|u| crate::consensus::is_stakeable_with(&params, u))
                    .map(|u| u.value)
                    .sum()
            } else {
                chain.total_staked()
            };
            let best_height = chain.best_height();
            let best_hash = chain.best_hash().unwrap_or([0u8; 32]);
            let best_block = chain.get_block_at_height(best_height);
            let best_timestamp = best_block.map(|b| b.header.timestamp).unwrap_or(0);
            let best_stake_modifier = best_block.map(|b| b.header.stake_modifier).unwrap_or(0);
            let staking_address = self
                .staking
                .as_ref()
                .map(|s| s.address.clone())
                .unwrap_or_default();
            let utxos = chain.get_utxos_for_address(&staking_address);
            // The height-1 bootstrap block's UTXO is brand new, so the next
            // block (height 2) must exempt it from the minimum stake age (T3).
            let tip_is_bootstrap = best_height == 1
                && best_block
                    .and_then(|b| b.transactions.first())
                    .map(|tx| tx.is_legacy_claim())
                    .unwrap_or(false);
            (
                best_height,
                best_hash,
                best_timestamp,
                best_stake_modifier,
                utxos,
                total_staked,
                tip_is_bootstrap,
                params,
            )
        };
        if let Some(staking) = self.staking.as_mut() {
            staking.bootstrap_exempt = tip_is_bootstrap;
            staking.set_consensus_params(params);
        }
        let staking = self
            .staking
            .as_ref()
            .ok_or_else(|| NodeError::Chain("Staking not enabled".into()))?;

        tracing::trace!(
            "Stake tick: evaluating {} wallet UTXOs for address {}",
            stake_utxos.len(),
            staking.address
        );
        if stake_utxos.is_empty() {
            tracing::trace!("Stake tick: no UTXOs available");
            return Ok(());
        }

        if now <= best_timestamp.saturating_add(params.target_block_time as u32) {
            return Ok(());
        }

        let Some(kernel) = staking.find_stake_kernel(
            best_stake_modifier,
            best_height + 1,
            now,
            total_staked,
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
        // block invalid. Governance transactions are additionally filtered so
        // the block cannot contain a vote its own validation rejects (for
        // example a vote whose proposal is not yet recorded).
        let pending_txs = {
            let chain = self.chain.lock().await;
            let mempool = self.mempool.lock().await;
            let candidates: Vec<crate::block::Transaction> = mempool
                .get_transactions()
                .into_iter()
                .filter(|tx| chain.compute_tx_fee(tx).is_some())
                .collect();
            chain.includable_transactions(candidates, best_height + 1, now)
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
                // Emit StakingReward event. The reward is total outputs minus
                // the staked principal — a coinstake returns `stake + reward`,
                // so summing outputs would overstate it by the stake amount.
                let reward_sats: u64 = {
                    let chain = self.chain.lock().await;
                    block_arc
                        .transactions
                        .iter()
                        .filter(|tx| matches!(tx.tx_type, crate::block::TxType::Coinstake))
                        .filter_map(|tx| chain.coinstake_reward(tx))
                        .sum()
                };
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
