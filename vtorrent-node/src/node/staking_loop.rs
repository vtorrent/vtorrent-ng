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

/// Drop staking candidates whose outpoint a mempool transaction already
/// spends. Spending one in a coinstake would invalidate that transaction and
/// evict it, which can strand an incentive payment that the daemon just
/// broadcast.
fn exclude_mempool_spent(
    utxos: Vec<crate::chain::Utxo>,
    mempool: &crate::mempool::Mempool,
) -> Vec<crate::chain::Utxo> {
    utxos
        .into_iter()
        .filter(|utxo| !mempool.is_outpoint_spent(&utxo.txid, utxo.vout))
        .collect()
}

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
            let utxos = chain.get_utxos_for_staking_address(&staking_address);
            // The height-1 bootstrap block's UTXO is brand new, so the next
            // block (height 2) must exempt it from the minimum stake age (T3).
            let tip_is_bootstrap = best_height == 1
                && best_block
                    .and_then(|b| b.transactions.first())
                    .map(|tx| tx.is_legacy_claim())
                    .unwrap_or(false);
            let total_staked = if tip_is_bootstrap {
                chain.total_staked()
            } else {
                crate::consensus::eligible_stake_value(chain.get_utxo_set().values(), &params, now)
            };
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
        // A mempool transaction — for example an incentive payment — may
        // already spend one of these outpoints. Staking it would make our own
        // block spend the same coin, evicting that transaction from every
        // mempool and stranding its payment intent. Skip committed coins until
        // the transaction confirms or is invalidated.
        let stake_utxos = {
            let mempool = self.mempool.lock().await;
            exclude_mempool_spent(stake_utxos, &mempool)
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

        let block_opt = {
            let mut chain = self.chain.lock().await;
            if chain.best_hash() != Some(best_hash) {
                return Ok(());
            }
            let candidates = self.mempool.lock().await.get_transactions();
            let pending_txs =
                chain.select_block_transactions(&kernel.1, candidates, best_height + 1, now)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Transaction, TxInput, TxOutput, TxType};

    fn utxo(txid_byte: u8) -> crate::chain::Utxo {
        crate::chain::Utxo {
            txid: [txid_byte; 32],
            vout: 0,
            value: 100_000_000,
            script_pubkey: vec![0x76, 0xa9, 0x14, 0x00, 0x88, 0xac],
            height: 1,
            timestamp: 0,
        }
    }

    #[test]
    fn staking_skips_a_coin_that_a_mempool_payment_already_spends() {
        let mut mempool = crate::mempool::Mempool::new(16);
        mempool
            .add_transaction_with_fee(
                Transaction {
                    version: 1,
                    tx_type: TxType::Standard,
                    inputs: vec![TxInput {
                        prev_txid: [1u8; 32],
                        prev_vout: 0,
                        script_sig: vec![],
                        sequence: 0xFFFF_FFFF,
                    }],
                    outputs: vec![TxOutput {
                        value: 99_000_000,
                        script_pubkey: vec![0x76, 0xa9, 0x14, 0x07, 0x88, 0xac],
                    }],
                    lock_time: 0,
                    claim_address: None,
                    claim_signature: None,
                },
                1_000,
            )
            .expect("mempool accepts the test transaction");

        let candidates = exclude_mempool_spent(vec![utxo(1), utxo(2)], &mempool);
        assert_eq!(candidates.len(), 1, "the spent coin must not be staked");
        assert_eq!(candidates[0].txid, [2u8; 32]);
    }
}
