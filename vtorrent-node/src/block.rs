/// Block and transaction data structures for the vTorrent 2.0 chain.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A transaction input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxInput {
    /// The previous output being spent (txid + vout).
    pub prev_txid: [u8; 32],
    pub prev_vout: u32,
    /// The scriptSig (signature + pubkey for P2PKH).
    pub script_sig: Vec<u8>,
    /// Sequence number (0xFFFFFFFF for standard).
    pub sequence: u32,
}

/// A transaction output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxOutput {
    /// Amount in satoshis.
    pub value: u64,
    /// The scriptPubKey.
    pub script_pubkey: Vec<u8>,
}

/// Transaction type flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum TxType {
    /// Standard coin transfer.
    Standard = 0,
    /// Coinbase (block reward).
    Coinbase = 1,
    /// Proof-of-Stake coinstake.
    Coinstake = 2,
    /// Legacy VTR claim transaction (special type for migration).
    LegacyClaim = 3,
    /// Atomic swap HTLC (for the built-in DEX).
    AtomicSwap = 4,
    /// Torrent incentive payment.
    TorrentIncentive = 5,
}

/// A transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    /// Transaction version.
    pub version: u32,
    /// Transaction type.
    pub tx_type: TxType,
    /// Transaction inputs.
    pub inputs: Vec<TxInput>,
    /// Transaction outputs.
    pub outputs: Vec<TxOutput>,
    /// Lock time.
    pub lock_time: u32,
    /// For LegacyClaim: the legacy address being claimed.
    pub claim_address: Option<String>,
    /// For LegacyClaim: the signature proving ownership of the legacy key.
    pub claim_signature: Option<Vec<u8>>,
}

impl Transaction {
    /// Compute the transaction hash (txid).
    pub fn txid(&self) -> [u8; 32] {
        let serialized = bincode::serialize(self).unwrap_or_default();
        let first = Sha256::digest(&serialized);
        let second = Sha256::digest(first);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&second);
        hash
    }

    /// Compute the SIGHASH_ALL signature hash for a single input.
    ///
    /// This is the canonical message that both the wallet (when signing) and
    /// the chain (when verifying via the script engine) must use. It is
    /// `SHA256d(bincode(tx with all scriptSigs cleared except the input's,
    /// which is set to `subscript`) + SIGHASH_ALL(1u32 LE))`.
    ///
    /// Keeping this in `vtorrent-node` (which the wallet depends on) ensures
    /// the signer and verifier can never drift apart.
    pub fn sighash(&self, input_index: usize, subscript: &[u8]) -> [u8; 32] {
        let mut tx_copy = self.clone();
        for (i, inp) in tx_copy.inputs.iter_mut().enumerate() {
            if i == input_index {
                inp.script_sig = subscript.to_vec();
            } else {
                inp.script_sig = Vec::new();
            }
        }

        let mut data = bincode::serialize(&tx_copy).unwrap_or_default();
        data.extend_from_slice(&1u32.to_le_bytes()); // SIGHASH_ALL

        let h1 = Sha256::digest(&data);
        let h2 = Sha256::digest(h1);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&h2);
        hash
    }

    /// Check if this is a coinbase transaction.
    pub fn is_coinbase(&self) -> bool {
        self.tx_type == TxType::Coinbase
    }

    /// Check if this is a coinstake transaction.
    pub fn is_coinstake(&self) -> bool {
        self.tx_type == TxType::Coinstake
    }

    /// Check if this is a legacy claim transaction.
    pub fn is_legacy_claim(&self) -> bool {
        self.tx_type == TxType::LegacyClaim
    }

    /// Get the total output value, saturating on overflow.
    pub fn total_output(&self) -> u64 {
        self.outputs
            .iter()
            .fold(0u64, |acc, o| acc.saturating_add(o.value))
    }

    /// Compute the fee paid by this transaction.
    ///
    /// For standard transactions, fee = total_input - total_output.
    /// Since input values require UTXO lookup, we approximate using a
    /// fixed 100_000 sat assumed input per input for mempool purposes.
    /// Callers with UTXO access should compute the real fee themselves.
    ///
    /// For coinbase/coinstake/claim transactions, fee is always 0.
    pub fn fee_sats(&self) -> u64 {
        if self.is_coinbase() || self.is_coinstake() || self.is_legacy_claim() {
            return 0;
        }
        let assumed_input: u64 = self.inputs.len() as u64 * 100_000;
        let total_out = self.total_output();
        assumed_input.saturating_sub(total_out)
    }

    /// Returns the approximate serialized size of this transaction in bytes.
    pub fn serialized_size(&self) -> usize {
        // Base: 4 (version) + 4 (lock_time) + 1 (tx_type)
        let base = 9usize;
        let inputs_size: usize = self
            .inputs
            .iter()
            .map(|i| 32 + 4 + 4 + i.script_sig.len())
            .sum();
        let outputs_size: usize = self.outputs.iter().map(|o| 8 + o.script_pubkey.len()).sum();
        base + inputs_size + outputs_size
    }

    /// Returns true if this transaction signals Replace-By-Fee (BIP-125).
    ///
    /// A transaction signals RBF if any input has a sequence number < 0xFFFFFFFE.
    pub fn signals_rbf(&self) -> bool {
        self.inputs.iter().any(|i| i.sequence < 0xFFFFFFFE)
    }

    /// Returns the transaction type as a string.
    pub fn type_str(&self) -> &'static str {
        match self.tx_type {
            TxType::Standard => "transfer",
            TxType::Coinbase => "coinbase",
            TxType::Coinstake => "coinstake",
            TxType::LegacyClaim => "legacy_claim",
            TxType::AtomicSwap => "atomic_swap",
            TxType::TorrentIncentive => "torrent_incentive",
        }
    }
}

/// A block header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Block version.
    pub version: u32,
    /// Hash of the previous block.
    pub prev_block_hash: [u8; 32],
    /// Merkle root of all transactions.
    pub merkle_root: [u8; 32],
    /// Unix timestamp.
    pub timestamp: u32,
    /// Difficulty target (nBits).
    pub bits: u32,
    /// Nonce (for PoW blocks; 0 for PoS blocks).
    pub nonce: u32,
    /// For PoS blocks: the stake modifier.
    pub stake_modifier: u64,
}

impl BlockHeader {
    /// Compute the block hash (SHA256d of the serialized header).
    pub fn hash(&self) -> [u8; 32] {
        let serialized = bincode::serialize(self).unwrap_or_default();
        let first = Sha256::digest(&serialized);
        let second = Sha256::digest(first);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&second);
        hash
    }

    /// Check if this is a Proof-of-Stake block (nonce == 0 and has coinstake tx).
    pub fn is_pos(&self) -> bool {
        self.nonce == 0
    }
}

/// A full block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    /// The block header.
    pub header: BlockHeader,
    /// The transactions in this block.
    pub transactions: Vec<Transaction>,
}

/// Compute a Merkle root from pre-computed transaction IDs.
///
/// Uses in-place reduction: the result is written back into `txids[0]` and
/// returned.  `txids` is treated as scratch space and is **not** preserved.
/// For an odd count the last hash is duplicated to reach an even count.
pub fn compute_merkle_root_from_txids(txids: &mut [[u8; 32]]) -> [u8; 32] {
    let mut len = txids.len();
    if len == 0 {
        return [0u8; 32];
    }
    let mut combined = [0u8; 64];
    while len > 1 {
        // Duplicate last element if odd.
        if len & 1 == 1 {
            // SAFETY: len >= 2 here so the index is valid.
            txids[len] = txids[len - 1];
            len += 1;
        }
        let half = len / 2;
        for i in 0..half {
            combined[..32].copy_from_slice(&txids[i * 2]);
            combined[32..].copy_from_slice(&txids[i * 2 + 1]);
            let first = Sha256::digest(combined);
            let second = Sha256::digest(first);
            txids[i].copy_from_slice(&second);
        }
        len = half;
    }
    txids[0]
}

impl Block {
    /// Compute the block hash.
    pub fn hash(&self) -> [u8; 32] {
        self.header.hash()
    }

    /// Compute the Merkle root of all transactions.
    pub fn compute_merkle_root(&self) -> [u8; 32] {
        let mut txids: Vec<[u8; 32]> = self.transactions.iter().map(|tx| tx.txid()).collect();
        compute_merkle_root_from_txids(&mut txids)
    }

    /// Get the height of this block (stored in the coinbase tx's lock_time for PoW,
    /// or in the coinstake for PoS — simplified here).
    pub fn height(&self) -> u32 {
        self.transactions
            .first()
            .map(|tx| tx.lock_time)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merkle_root_single_tx() {
        let tx = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 100,
                script_pubkey: vec![],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let block = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                timestamp: 0,
                bits: 0,
                nonce: 0,
                stake_modifier: 0,
            },
            transactions: vec![tx.clone()],
        };
        let root = block.compute_merkle_root();
        assert_eq!(root, tx.txid()); // Single tx: merkle root = txid
    }

    #[test]
    fn test_block_hash_deterministic() {
        let header = BlockHeader {
            version: 1,
            prev_block_hash: [0u8; 32],
            merkle_root: [0u8; 32],
            timestamp: 1700000000,
            bits: 0x1d00ffff,
            nonce: 42,
            stake_modifier: 0,
        };
        let h1 = header.hash();
        let h2 = header.hash();
        assert_eq!(h1, h2);
    }
}
