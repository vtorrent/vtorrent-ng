/// Blockchain state manager.
///
/// Manages the chain of blocks, UTXO set, and processes new blocks.

use std::collections::HashMap;
use crate::{
    block::{Block, Transaction, TxType},
    consensus::validate_block,
    error::{NodeError, Result},
    genesis::create_genesis_block,
};

/// A UTXO (unspent transaction output).
#[derive(Debug, Clone)]
pub struct Utxo {
    pub txid: [u8; 32],
    pub vout: u32,
    pub value: u64,
    pub script_pubkey: Vec<u8>,
    pub height: u32,
    pub timestamp: u32,
}

/// The blockchain state.
pub struct Chain {
    /// All blocks indexed by hash.
    blocks: HashMap<[u8; 32], Block>,
    /// Block hash at each height.
    height_index: Vec<[u8; 32]>,
    /// UTXO set: (txid, vout) → Utxo.
    utxo_set: HashMap<([u8; 32], u32), Utxo>,
    /// Set of legacy addresses that have already been claimed.
    claimed_addresses: std::collections::HashSet<String>,
}

impl Chain {
    /// Initialize a new chain with the genesis block.
    pub fn new() -> Result<Self> {
        let genesis = create_genesis_block();
        let genesis_hash = genesis.hash();

        let mut chain = Self {
            blocks: HashMap::new(),
            height_index: Vec::new(),
            utxo_set: HashMap::new(),
            claimed_addresses: std::collections::HashSet::new(),
        };

        chain.blocks.insert(genesis_hash, genesis.clone());
        chain.height_index.push(genesis_hash);

        // Process genesis block outputs into UTXO set
        chain.apply_block(&genesis, 0)?;

        tracing::info!("Chain initialized with genesis block: {}", hex::encode(genesis_hash));
        Ok(chain)
    }

    /// Get the current best block height.
    pub fn best_height(&self) -> u32 {
        self.height_index.len().saturating_sub(1) as u32
    }

    /// Get the best block hash.
    pub fn best_hash(&self) -> Option<[u8; 32]> {
        self.height_index.last().copied()
    }

    /// Get a block by hash.
    pub fn get_block(&self, hash: &[u8; 32]) -> Option<&Block> {
        self.blocks.get(hash)
    }

    /// Get a block by height.
    pub fn get_block_at_height(&self, height: u32) -> Option<&Block> {
        self.height_index.get(height as usize)
            .and_then(|hash| self.blocks.get(hash))
    }

    /// Get the UTXO for a specific output.
    pub fn get_utxo(&self, txid: &[u8; 32], vout: u32) -> Option<&Utxo> {
        self.utxo_set.get(&(*txid, vout))
    }

    /// Get all UTXOs for a specific scriptPubKey.
    pub fn get_utxos_for_script(&self, script: &[u8]) -> Vec<&Utxo> {
        self.utxo_set.values()
            .filter(|u| u.script_pubkey == script)
            .collect()
    }

    /// Get all UTXOs belonging to a specific address.
    pub fn get_utxos_for_address(&self, address: &str) -> Vec<Utxo> {
        // Build the expected P2PKH script for this address
        let script = self.address_to_p2pkh_script(address);
        self.utxo_set.values()
            .filter(|u| u.script_pubkey == script)
            .cloned()
            .collect()
    }

    /// Convert a vTorrent address to a P2PKH scriptPubKey for UTXO matching.
    fn address_to_p2pkh_script(&self, address: &str) -> Vec<u8> {
        const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut num: u128 = 0;
        for c in address.bytes() {
            if let Some(d) = ALPHABET.iter().position(|&b| b == c) {
                num = num.saturating_mul(58).saturating_add(d as u128);
            }
        }
        let bytes = num.to_be_bytes();
        let trim = bytes.iter().position(|&b| b != 0).unwrap_or(15);
        let trimmed = &bytes[trim..];
        if trimmed.len() >= 21 {
            let hash160 = &trimmed[1..21];
            let mut script = Vec::with_capacity(25);
            script.push(0x76); // OP_DUP
            script.push(0xa9); // OP_HASH160
            script.push(0x14); // push 20 bytes
            script.extend_from_slice(hash160);
            script.push(0x88); // OP_EQUALVERIFY
            script.push(0xac); // OP_CHECKSIG
            script
        } else {
            Vec::new()
        }
    }

    /// Get the full UTXO set as a flat list of all UTXOs.
    pub fn get_utxo_set(&self) -> &std::collections::HashMap<([u8; 32], u32), Utxo> {
        &self.utxo_set
    }

    /// Get the genesis block.
    pub fn genesis_block(&self) -> &Block {
        let genesis_hash = self.height_index[0];
        self.blocks.get(&genesis_hash).expect("genesis block always present")
    }

    pub fn is_claimed(&self, address: &str) -> bool {
        self.claimed_addresses.contains(address)
    }

    /// Add a new block to the chain.
    pub fn add_block(&mut self, block: Block) -> Result<()> {
        let height = self.best_height() + 1;
        let prev_block = self.get_block_at_height(height - 1)
            .ok_or_else(|| NodeError::Chain("Previous block not found".into()))?;

        // Validate the block
        validate_block(&block, height - 1, prev_block.header.timestamp)?;

        // Check prev hash
        if block.header.prev_block_hash != prev_block.hash() {
            return Err(NodeError::InvalidBlock("Previous block hash mismatch".into()));
        }

        let block_hash = block.hash();
        self.apply_block(&block, height)?;
        self.blocks.insert(block_hash, block);
        self.height_index.push(block_hash);

        tracing::info!("Added block {} at height {}", hex::encode(block_hash), height);
        Ok(())
    }

    /// Apply a block's transactions to the UTXO set.
    fn apply_block(&mut self, block: &Block, height: u32) -> Result<()> {
        for tx in &block.transactions {
            self.apply_transaction(tx, height, block.header.timestamp)?;
        }
        Ok(())
    }

    /// Apply a transaction to the UTXO set.
    fn apply_transaction(&mut self, tx: &Transaction, height: u32, timestamp: u32) -> Result<()> {
        let txid = tx.txid();

        // Spend inputs (except for coinbase)
        if !tx.is_coinbase() {
            for input in &tx.inputs {
                let key = (input.prev_txid, input.prev_vout);
                if self.utxo_set.remove(&key).is_none() && !tx.is_legacy_claim() {
                    return Err(NodeError::InvalidTransaction(
                        format!("Input {}:{} not found in UTXO set",
                            hex::encode(input.prev_txid), input.prev_vout)
                    ));
                }
            }
        }

        // Track claimed legacy addresses
        if tx.is_legacy_claim() {
            if let Some(addr) = &tx.claim_address {
                if self.claimed_addresses.contains(addr) {
                    return Err(NodeError::ClaimAlreadyProcessed(addr.clone()));
                }
                self.claimed_addresses.insert(addr.clone());
            }
        }

        // Add outputs to UTXO set
        for (vout, output) in tx.outputs.iter().enumerate() {
            self.utxo_set.insert((txid, vout as u32), Utxo {
                txid,
                vout: vout as u32,
                value: output.value,
                script_pubkey: output.script_pubkey.clone(),
                height,
                timestamp,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chain_initialization() {
        let chain = Chain::new().expect("Chain init failed");
        assert_eq!(chain.best_height(), 0);
        assert!(chain.best_hash().is_some());
    }
}
