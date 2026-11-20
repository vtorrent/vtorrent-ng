//! Rebuild the trusted SPV header view from the canonical VTR chain.

use crate::state::AppState;
use vtorrent_spv::SpvHeader;

fn header_at(chain: &vtorrent_node::chain::Chain, height: u32) -> Result<SpvHeader, String> {
    let header = chain
        .get_header_at_height(height)
        .ok_or_else(|| format!("missing VTR header at height {height}"))?;
    Ok(SpvHeader {
        version: header.version,
        prev_hash: header.prev_block_hash,
        merkle_root: header.merkle_root,
        utxo_root: header.utxo_root,
        timestamp: header.timestamp,
        bits: header.bits,
        nonce: header.nonce,
        stake_modifier: header.stake_modifier,
        height,
    })
}

/// Make the in-memory SPV header view match the full node's canonical chain.
///
/// The SPV chain is not persisted. It is normally fed incrementally from block
/// events, but an event can be lost, an insertion can fail, or a reorg can move
/// the tip to a branch the SPV view never followed. In those cases the view is
/// rebuilt from headers the full node has already validated. A synced view is
/// left untouched, so the periodic call is cheap.
///
/// A full replacement is built outside the locks and installed only after both
/// the chain tip and the SPV tip are re-checked. This closes the window where a
/// reorg between the snapshot and the install could re-publish the abandoned
/// branch.
pub async fn sync_spv_from_chain(state: &AppState) -> Result<(), String> {
    let (spv_height, spv_hash) = {
        let spv = state.spv_chain.read().await;
        (spv.best_height(), spv.best_hash())
    };
    let (best, rebuild, start, snapshot_tip, generation) = {
        let chain = state.chain.lock().await;
        let rebuild = match spv_hash {
            Some(hash) => chain.block_hash_at_height(spv_height) != Some(hash),
            None => true,
        };
        let start = if rebuild { 0 } else { spv_height + 1 };
        (
            chain.best_height(),
            rebuild,
            start,
            chain.best_hash(),
            chain.tip_generation(),
        )
    };
    if !rebuild && start > best {
        return Ok(());
    }

    // Snapshot in bounded chunks so a full rebuild does not hold the consensus
    // chain lock for the entire header clone. Any block or reorg invalidates
    // the generation and defers the sync to the next tick.
    const SNAPSHOT_CHUNK: u32 = 2_048;
    let mut headers: Vec<SpvHeader> = Vec::new();
    let mut next_height = start;
    while next_height <= best {
        let chain = state.chain.lock().await;
        if chain.tip_generation() != generation || chain.best_hash() != snapshot_tip {
            return Ok(());
        }
        let end = next_height.saturating_add(SNAPSHOT_CHUNK - 1).min(best);
        for height in next_height..=end {
            headers.push(header_at(&chain, height)?);
        }
        drop(chain);
        next_height = end.saturating_add(1);
        tokio::task::yield_now().await;
    }

    // Build outside every lock. A rebuild replaces the whole value, so the
    // install itself is O(1).
    let rebuilt = if rebuild {
        let (genesis, bits) = {
            let spv = state.spv_chain.read().await;
            spv.pinned_genesis()
                .ok_or_else(|| "SPV chain has no pinned genesis".to_string())?
        };
        let mut rebuilt = vtorrent_spv::SpvChain::with_genesis(genesis, bits);
        for header in &headers {
            rebuilt
                .add_trusted_header(header.clone())
                .map_err(|error| {
                    format!("could not rebuild SPV header {}: {error}", header.height)
                })?;
        }
        Some(rebuilt)
    } else {
        None
    };

    // Chain -> SPV lock order is the same order used elsewhere. Both tips must
    // still match the snapshot; otherwise the chain moved or another updater
    // won, and the next tick will reconcile the newer state.
    let chain = state.chain.lock().await;
    if chain.best_hash() != snapshot_tip {
        return Ok(());
    }
    let mut spv = state.spv_chain.write().await;
    if spv.best_hash() != spv_hash {
        return Ok(());
    }
    if let Some(rebuilt) = rebuilt {
        *spv = rebuilt;
    } else {
        for header in headers {
            let height = header.height;
            spv.add_trusted_header(header)
                .map_err(|error| format!("could not sync SPV header {height}: {error}"))?;
        }
    }
    spv.prune_side_headers();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtorrent_spv::SpvHeader;

    fn spv_header(block: &vtorrent_node::block::Block, height: u32) -> SpvHeader {
        SpvHeader {
            version: block.header.version,
            prev_hash: block.header.prev_block_hash,
            merkle_root: block.header.merkle_root,
            utxo_root: block.header.utxo_root,
            timestamp: block.header.timestamp,
            bits: block.header.bits,
            nonce: block.header.nonce,
            stake_modifier: block.header.stake_modifier,
            height,
        }
    }

    #[tokio::test]
    async fn rebuilds_spv_after_a_divergent_tip() {
        let state = AppState::new();
        let mut chain = vtorrent_node::chain::Chain::new_regtest().unwrap();
        let address = vtorrent_core::address::Address::from_hash160(&[7u8; 20], 70)
            .unwrap()
            .to_string();
        chain.mint_to_address(&address, 100_000_000).unwrap();
        let canonical = chain.best_hash().unwrap();
        let genesis = spv_header(chain.get_block_at_height(0).unwrap(), 0);
        let canonical_header = spv_header(chain.get_block_at_height(1).unwrap(), 1);
        *state.chain.lock().await = chain;
        {
            let mut spv = state.spv_chain.write().await;
            spv.set_genesis(
                vtorrent_node::genesis::GENESIS_HASH,
                vtorrent_node::genesis::GENESIS_BITS,
            )
            .unwrap();
            spv.add_trusted_header(genesis).unwrap();
            let mut fake = canonical_header.clone();
            while fake.hash() == canonical_header.hash() {
                fake.nonce = fake.nonce.wrapping_add(1);
            }
            spv.add_trusted_header(fake).unwrap();
        }

        sync_spv_from_chain(&state).await.unwrap();

        let spv = state.spv_chain.read().await;
        assert_eq!(spv.best_height(), 1);
        assert_eq!(spv.best_hash(), Some(canonical));
    }
}
