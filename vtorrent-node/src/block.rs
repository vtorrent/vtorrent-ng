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
        let serialized = bincode::serialize(self).unwrap_or_else(|e| {
            tracing::warn!("txid serialization failed: {}", e);
            Vec::new()
        });
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
    /// **Optimized**: hashes incrementally from the transaction fields instead
    /// of cloning the entire `Transaction` and serializing via bincode.  This
    /// avoids one heap allocation and O(n) memcpy per input verification.
    /// The output is bit-identical to the original bincode-based implementation.
    pub fn sighash(&self, input_index: usize, subscript: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        // An out-of-range index silently commits to an empty subscript for
        // every input, masking caller bugs with a "valid" hash. Fail loudly
        // in debug builds; in release the behavior is unchanged.
        debug_assert!(
            input_index < self.inputs.len(),
            "sighash input_index {} out of range ({} inputs)",
            input_index,
            self.inputs.len()
        );
        let mut h = Sha256::new();

        // bincode 1.x default: little-endian integers, u64 length prefix for Vec.
        // Enum variants are serialized as u32 LE.
        h.update(self.version.to_le_bytes());
        h.update((self.tx_type as u32).to_le_bytes());
        h.update((self.inputs.len() as u64).to_le_bytes());
        for (i, inp) in self.inputs.iter().enumerate() {
            h.update(inp.prev_txid);
            h.update(inp.prev_vout.to_le_bytes());
            let sig = if i == input_index {
                subscript
            } else {
                &[] as &[u8]
            };
            h.update((sig.len() as u64).to_le_bytes());
            h.update(sig);
            h.update(inp.sequence.to_le_bytes());
        }
        h.update((self.outputs.len() as u64).to_le_bytes());
        for out in &self.outputs {
            h.update(out.value.to_le_bytes());
            h.update((out.script_pubkey.len() as u64).to_le_bytes());
            h.update(&out.script_pubkey);
        }
        h.update(self.lock_time.to_le_bytes());
        match &self.claim_address {
            None => h.update([0u8]),
            Some(addr) => {
                h.update([1u8]);
                let bytes = addr.as_bytes();
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(bytes);
            }
        }
        match &self.claim_signature {
            None => h.update([0u8]),
            Some(sig) => {
                h.update([1u8]);
                h.update((sig.len() as u64).to_le_bytes());
                h.update(sig);
            }
        }
        // SIGHASH_ALL suffix
        h.update(1u32.to_le_bytes());

        let h1 = h.finalize();
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
        // Claim fields are part of the txid hash and wire format; count them
        // so fee-rate and block-size accounting reflect the real footprint.
        let claim_size = self
            .claim_address
            .as_ref()
            .map(|a| 1 + a.len())
            .unwrap_or(0)
            + self
                .claim_signature
                .as_ref()
                .map(|s| 1 + s.len())
                .unwrap_or(0);
        base + inputs_size + outputs_size + claim_size
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
        let serialized = bincode::serialize(self).unwrap_or_else(|e| {
            tracing::warn!("block header serialization failed: {}", e);
            Vec::new()
        });
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
    // Ensure capacity for odd-length duplication.  If the caller provided
    // exactly `len` slots and `len` is odd, we need one extra slot to
    // duplicate the last hash.  We reallocate in this rare case rather
    // than requiring callers to always over-allocate.
    let mut buf: Vec<[u8; 32]> = Vec::with_capacity(len + 1);
    buf.extend_from_slice(txids);
    let mut combined = [0u8; 64];
    while len > 1 {
        if len & 1 == 1 {
            buf.push(buf[len - 1]);
            len += 1;
        }
        let half = len / 2;
        for i in 0..half {
            combined[..32].copy_from_slice(&buf[i * 2]);
            combined[32..].copy_from_slice(&buf[i * 2 + 1]);
            let first = Sha256::digest(combined);
            let second = Sha256::digest(first);
            buf[i].copy_from_slice(&second);
        }
        len = half;
    }
    buf[0]
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

    #[test]
    fn test_sighash_matches_bincode_reference() {
        use sha2::Digest;

        let subscript = vec![
            0x76, 0xa9, 0x14, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc,
            0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0x88, 0xac,
        ];

        let tx = Transaction {
            version: 2,
            tx_type: TxType::Standard,
            inputs: vec![
                TxInput {
                    prev_txid: [0xAA; 32],
                    prev_vout: 0,
                    script_sig: vec![0x51, 0x52],
                    sequence: 0xFFFFFFFF,
                },
                TxInput {
                    prev_txid: [0xBB; 32],
                    prev_vout: 1,
                    script_sig: vec![0x53],
                    sequence: 0xFFFFFFFE,
                },
            ],
            outputs: vec![
                TxOutput {
                    value: 500_000_000,
                    script_pubkey: vec![
                        0x76, 0xa9, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0xac,
                    ],
                },
                TxOutput {
                    value: 100_000_000,
                    script_pubkey: vec![
                        0x76, 0xa9, 0x14, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x88, 0xac,
                    ],
                },
            ],
            lock_time: 1700000000,
            claim_address: Some("VTestAddr123".into()),
            claim_signature: Some(vec![0xDE, 0xAD]),
        };

        // Dump bincode format to understand the encoding
        let mut tx_copy = tx.clone();
        for (i, inp) in tx_copy.inputs.iter_mut().enumerate() {
            if i == 0 {
                inp.script_sig = subscript.clone();
            } else {
                inp.script_sig = Vec::new();
            }
        }

        // Reference: the original bincode-based implementation
        fn sighash_reference(tx: &Transaction, input_index: usize, subscript: &[u8]) -> [u8; 32] {
            let mut tx_copy = tx.clone();
            for (i, inp) in tx_copy.inputs.iter_mut().enumerate() {
                if i == input_index {
                    inp.script_sig = subscript.to_vec();
                } else {
                    inp.script_sig = Vec::new();
                }
            }
            let mut data = bincode::serialize(&tx_copy).unwrap_or_default();
            data.extend_from_slice(&1u32.to_le_bytes());
            let h1 = Sha256::digest(&data);
            let h2 = Sha256::digest(h1);
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&h2);
            hash
        }

        // Compare for both inputs and with None fields
        for idx in 0..tx.inputs.len() {
            let old = sighash_reference(&tx, idx, &subscript);
            let new = tx.sighash(idx, &subscript);
            if old != new {
                eprintln!("MISMATCH at input {}", idx);
                eprintln!("  old: {:02x?}", old);
                eprintln!("  new: {:02x?}", new);
            }
            assert_eq!(old, new, "sighash mismatch for input {}", idx);
        }

        // Also test with None claim fields
        let tx_no_claim = Transaction {
            claim_address: None,
            claim_signature: None,
            ..tx
        };
        let old = sighash_reference(&tx_no_claim, 0, &subscript);
        let new = tx_no_claim.sighash(0, &subscript);
        assert_eq!(old, new, "sighash mismatch for None claim fields");
    }

    #[test]
    fn test_serialized_size_includes_claim_fields() {
        let base = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 1_000,
                script_pubkey: vec![0x76, 0xa9, 0x14, 0x00, 0x88, 0xac],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let base_size = base.serialized_size();

        let claim = Transaction {
            tx_type: TxType::LegacyClaim,
            claim_address: Some("VTestAddr123".into()),
            claim_signature: Some(vec![0xDE; 65]),
            ..base
        };
        let claim_size = claim.serialized_size();

        // 1 (tag) + 12 (address) + 1 (tag) + 65 (sig) = 79 extra bytes
        assert_eq!(claim_size, base_size + 79);
    }
}
