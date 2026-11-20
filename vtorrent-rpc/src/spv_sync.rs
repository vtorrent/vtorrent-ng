//! Rebuild the trusted SPV header view from the canonical VTR chain.

use crate::state::AppState;
use vtorrent_spv::SpvHeader;

/// Make the in-memory SPV header view match the full node's canonical chain.
///
/// The SPV chain is not persisted. It is normally fed incrementally from block
/// events, but an event can be lost, an insertion can fail, or a reorg can move
/// the tip to a branch the SPV view never followed. In those cases the view is
/// rebuilt from headers the full node has already validated. A synced view is
/// left untouched, so the periodic call is cheap.
pub async fn sync_spv_from_chain(state: &AppState) -> Result<(), String> {
    let chain = state.chain.lock().await;
    let best = chain.best_height();
    let (rebuild, start) = {
        let spv = state.spv_chain.read().await;
        match spv.best_hash() {
            Some(hash) if chain.block_hash_at_height(spv.best_height()) == Some(hash) => {
                (false, spv.best_height() + 1)
            }
            _ => (true, 0),
        }
    };
    if !rebuild && start > best {
        return Ok(());
    }
    let headers: Vec<SpvHeader> = (start..=best)
        .map(|height| {
            chain
                .get_header_at_height(height)
                .map(|header| SpvHeader {
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
                .ok_or_else(|| format!("missing VTR header at height {height}"))
        })
        .collect::<Result<_, _>>()?;
    drop(chain);

    let mut spv = state.spv_chain.write().await;
    if rebuild {
        spv.reset_headers();
    }
    for header in headers {
        let height = header.height;
        spv.add_trusted_header(header)
            .map_err(|error| format!("could not sync SPV header {height}: {error}"))?;
    }
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
