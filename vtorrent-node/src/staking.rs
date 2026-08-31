/// PoS Staking Engine for vTorrent 2.0
///
/// Implements Proof-of-Stake block production:
/// - Selects eligible UTXOs (age >= MIN_STAKE_AGE, amount >= MIN_STAKE_AMOUNT)
/// - Computes the stake kernel hash (simplified: SHA256d of utxo_txid + timestamp)
/// - Builds a coinstake transaction with the stake reward
/// - Assembles a complete PoS block ready to be added to the chain
///
/// The stake kernel check is a simplified version of the PPCoin protocol.
/// A full implementation would use the stake modifier from the chain.
use crate::{
    block::{
        compute_merkle_root_from_txids, Block, BlockHeader, Transaction, TxInput, TxOutput, TxType,
    },
    chain::Utxo,
    consensus::{
        check_stake_kernel, compute_pos_reward, compute_stake_modifier, stake_kernel_hash,
        MAX_STAKE_AGE, MIN_STAKE_AGE, MIN_STAKE_AMOUNT,
    },
};

/// Runtime staking control command sent from RPC/tauri to the node.
#[derive(Debug, Clone)]
pub enum StakingCommand {
    /// Start staking with the given address and optional signing WIF.
    Start {
        address: String,
        wif: Option<String>,
    },
    /// Stop staking.
    Stop,
}

/// The staking engine.
pub struct StakingEngine {
    /// The address whose UTXOs are used for staking.
    pub address: String,
    /// The WIF-encoded private key used to sign the coinstake input.
    pub wif: Option<String>,
    /// Minimum coin age in seconds for UTXO eligibility.
    pub min_stake_age: u64,
    /// Maximum coin age in seconds for UTXO eligibility.
    pub max_stake_age: u64,
}

impl StakingEngine {
    /// Create a new staking engine for the given address.
    pub fn new(address: String) -> Self {
        Self {
            address,
            wif: None,
            min_stake_age: MIN_STAKE_AGE,
            max_stake_age: MAX_STAKE_AGE,
        }
    }

    /// Create a new staking engine with a signing key.
    pub fn with_wif(address: String, wif: String) -> Self {
        Self {
            address,
            wif: Some(wif),
            min_stake_age: MIN_STAKE_AGE,
            max_stake_age: MAX_STAKE_AGE,
        }
    }

    /// Create a fast staking engine for regtest soak testing.
    ///
    /// Lowers min stake age to 60s and max stake age to 3600s (1 hour) so
    /// blocks are produced rapidly with few UTXOs.
    pub fn new_fast(address: String) -> Self {
        Self {
            address,
            wif: None,
            min_stake_age: 60,
            max_stake_age: u64::MAX,
        }
    }

    /// Create a fast staking engine with a signing key for regtest.
    pub fn with_wif_fast(address: String, wif: String) -> Self {
        Self {
            address,
            wif: Some(wif),
            min_stake_age: 60,
            max_stake_age: u64::MAX,
        }
    }

    /// Try to build a valid PoS block from available UTXOs.
    ///
    /// Returns `Some(block)` if a valid stake kernel was found, `None` otherwise.
    pub fn build_stake_block(
        &self,
        prev_hash: [u8; 32],
        prev_stake_modifier: u64,
        height: u32,
        timestamp: u32,
        utxos: Vec<Utxo>,
        pending_txs: Vec<Transaction>,
    ) -> Option<Block> {
        // Filter eligible UTXOs
        let eligible: Vec<&Utxo> = utxos
            .iter()
            .filter(|u| self.is_eligible(u, timestamp))
            .collect();
        for u in &utxos {
            if !self.is_eligible(u, timestamp) {
                let age = timestamp.saturating_sub(u.timestamp);
                tracing::debug!(
                    "UTXO {}:{} value={} REJECTED: age={}s (min {} max {})",
                    hex::encode(u.txid),
                    u.vout,
                    u.value,
                    age,
                    self.min_stake_age,
                    self.max_stake_age
                );
            }
        }

        if eligible.is_empty() {
            tracing::debug!("No eligible UTXOs for staking at height {}", height);
            return None;
        }

        // Try each eligible UTXO as a stake kernel.  Move pending_txs on
        // the first (and only) success — the function returns immediately.
        for utxo in &eligible {
            if let Some(coinstake) =
                self.try_stake_kernel(prev_stake_modifier, utxo, timestamp, height)
            {
                // Drop pending txs whose inputs collide with the coinstake's
                // stake input (or with each other). The mempool admits txs
                // against the current UTXO set, but the coinstake consumes a
                // UTXO inside this same block — a stale mempool tx spending
                // the staked outpoint would otherwise make the block invalid
                // and permanently wedge staking.
                let stake_outpoint = (coinstake.inputs[0].prev_txid, coinstake.inputs[0].prev_vout);
                let mut seen: std::collections::HashSet<([u8; 32], u32)> =
                    std::collections::HashSet::new();
                seen.insert(stake_outpoint);
                let non_conflicting: Vec<Transaction> = pending_txs
                    .into_iter()
                    .filter(|tx| {
                        let conflict = tx.inputs.iter().any(|i| !seen.insert((i.prev_txid, i.prev_vout)));
                        if conflict {
                            tracing::debug!(
                                "Excluding mempool tx {} from block template: input conflicts with coinstake or earlier tx",
                                hex::encode(tx.txid())
                            );
                        }
                        !conflict
                    })
                    .collect();
                let block = self.assemble_block(
                    prev_hash,
                    prev_stake_modifier,
                    timestamp,
                    coinstake,
                    non_conflicting,
                );
                let block_hash = block.hash();
                tracing::info!(
                    height = %height,
                    stake_utxo = %format!("{}:{}", hex::encode(utxo.txid), utxo.vout),
                    stake_value = %utxo.value,
                    timestamp = %timestamp,
                    block_hash = %hex::encode(block_hash),
                    tx_count = %block.transactions.len(),
                    "Successfully staked block"
                );
                return Some(block);
            }
        }

        None
    }

    /// Check if a UTXO is eligible for staking.
    fn is_eligible(&self, utxo: &Utxo, current_timestamp: u32) -> bool {
        // Must meet minimum amount
        if utxo.value < MIN_STAKE_AMOUNT {
            return false;
        }

        // Must have minimum coin age
        let coin_age_seconds = current_timestamp.saturating_sub(utxo.timestamp);
        if (coin_age_seconds as u64) < self.min_stake_age {
            return false;
        }

        // Must not exceed maximum coin age (prevents very old coins from
        // dominating the staking weight indefinitely).
        if (coin_age_seconds as u64) > self.max_stake_age {
            return false;
        }

        true
    }

    /// Try to find a valid stake kernel for a UTXO.
    ///
    /// The stake kernel hash must be below the target (proportional to stake
    /// amount), using the tip's stake modifier.
    fn try_stake_kernel(
        &self,
        stake_modifier: u64,
        utxo: &Utxo,
        timestamp: u32,
        height: u32,
    ) -> Option<Transaction> {
        // The kernel check is shared with the chain's block validation so a
        // block that passes validation provably met the difficulty requirement.
        if !check_stake_kernel(stake_modifier, utxo, timestamp) {
            let kernel_hash = stake_kernel_hash(stake_modifier, utxo, timestamp);
            let kv = u32::from_le_bytes([
                kernel_hash[0],
                kernel_hash[1],
                kernel_hash[2],
                kernel_hash[3],
            ]);
            tracing::debug!(
                "Kernel miss: value={} target={} kernel_val={} modifier={}",
                utxo.value,
                (utxo.value / 1000).min(u32::MAX as u64),
                kv,
                stake_modifier
            );
            return None;
        }

        // Compute coin age and stake reward
        let coin_age_seconds = timestamp.saturating_sub(utxo.timestamp);
        let reward = compute_pos_reward(utxo.value, coin_age_seconds as u64);

        // Build coinstake transaction
        // Input: the staked UTXO
        let stake_input = TxInput {
            prev_txid: utxo.txid,
            prev_vout: utxo.vout,
            script_sig: Vec::new(), // filled in below after signing
            sequence: u32::MAX,
        };

        // Output 0: empty marker (PPCoin protocol: first output of coinstake is empty)
        let marker_output = TxOutput {
            value: 0,
            script_pubkey: Vec::new(),
        };

        // Output 1: stake return + reward to staking address
        let stake_script = self.address_to_script(&self.address)?;
        let stake_output = TxOutput {
            value: utxo.value.saturating_add(reward),
            script_pubkey: stake_script,
        };

        let mut coinstake = Transaction {
            version: 1,
            tx_type: TxType::Coinstake,
            inputs: vec![stake_input],
            outputs: vec![marker_output, stake_output],
            lock_time: height,
            claim_address: None,
            claim_signature: None,
        };

        // Sign the coinstake input over the same sighash the chain verifies.
        // Without a valid signature the block would be rejected by the chain's
        // script verification.
        if let Some(wif) = &self.wif {
            let script_sig = self.sign_coinstake_input(&coinstake, utxo, wif)?;
            coinstake.inputs[0].script_sig = script_sig;
        } else {
            tracing::warn!(
                "Staking engine has no signing key; coinstake would be rejected by the chain"
            );
        }

        Some(coinstake)
    }

    /// Sign the coinstake input with the staking key over the P2PKH sighash.
    fn sign_coinstake_input(
        &self,
        coinstake: &Transaction,
        utxo: &Utxo,
        wif: &str,
    ) -> Option<Vec<u8>> {
        use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};

        let key = vtorrent_core::keys::PrivateKey::from_wif(wif).ok()?;
        let secret_key = SecretKey::from_slice(key.as_bytes()).ok()?;
        let secp = Secp256k1::new();
        let pubkey = PublicKey::from_secret_key(&secp, &secret_key);

        // The subscript is the previous output's scriptPubKey (P2PKH).
        let sighash = coinstake.sighash(0, &utxo.script_pubkey);
        let message = Message::from_digest(sighash);
        let sig = secp.sign_ecdsa(&message, &secret_key);
        let mut der = sig.serialize_der().to_vec();
        der.push(0x01); // SIGHASH_ALL

        // Build P2PKH scriptSig: <sig> <pubkey>
        let pubkey_bytes = pubkey.serialize();
        if der.len() > 255 || pubkey_bytes.len() > 255 {
            return None;
        }
        let mut script = Vec::with_capacity(1 + der.len() + 1 + pubkey_bytes.len());
        script.push(der.len() as u8);
        script.extend_from_slice(&der);
        script.push(pubkey_bytes.len() as u8);
        script.extend_from_slice(&pubkey_bytes);
        Some(script)
    }

    /// Assemble a complete PoS block.
    fn assemble_block(
        &self,
        prev_hash: [u8; 32],
        prev_stake_modifier: u64,
        timestamp: u32,
        coinstake: Transaction,
        pending_txs: Vec<Transaction>,
    ) -> Block {
        // PoS blocks have nonce = 0 (identified by BlockHeader::is_pos())
        let mut transactions = vec![coinstake];

        // Include pending transactions from mempool (up to block size limit)
        let mut block_size = 0usize;
        const MAX_BLOCK_SIZE: usize = 1_000_000;

        for tx in pending_txs {
            let tx_size = tx.inputs.len() * 148 + tx.outputs.len() * 34 + 10;
            if block_size + tx_size > MAX_BLOCK_SIZE {
                break;
            }
            transactions.push(tx);
            block_size += tx_size;
        }

        let merkle_root = {
            let mut txids: Vec<[u8; 32]> = transactions.iter().map(|tx| tx.txid()).collect();
            compute_merkle_root_from_txids(&mut txids)
        };

        let header = BlockHeader {
            version: 2,
            prev_block_hash: prev_hash,
            merkle_root,
            timestamp,
            bits: 0x1e0fffff,
            nonce: 0,
            stake_modifier: compute_stake_modifier(prev_stake_modifier, &prev_hash),
        };

        Block {
            header,
            transactions,
        }
    }

    /// Convert a vTorrent address to a P2PKH scriptPubKey.
    /// Decodes the Base58Check address and builds the standard script.
    /// Returns `None` if the address is invalid (avoids silent fund burn).
    fn address_to_script(&self, address: &str) -> Option<Vec<u8>> {
        let addr = vtorrent_core::address::validate_p2pkh(address).ok()?;
        Some(vtorrent_core::address::p2pkh_script_pubkey(&addr.hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{chain::Utxo, consensus::COIN};

    fn make_utxo(value: u64, age_seconds: u32) -> Utxo {
        let now = 1_700_000_000u32;
        Utxo {
            txid: [1u8; 32],
            vout: 0,
            value,
            script_pubkey: vec![0x76, 0xa9, 0x14],
            height: 100,
            timestamp: now - age_seconds,
        }
    }

    #[test]
    fn test_eligible_utxo() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(100 * COIN, (MIN_STAKE_AGE as u32).saturating_add(3600));
        assert!(engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_ineligible_utxo_too_young() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(100 * COIN, (MIN_STAKE_AGE / 2) as u32);
        assert!(!engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_ineligible_utxo_too_small() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(
            MIN_STAKE_AMOUNT / 2,
            (MIN_STAKE_AGE as u32).saturating_add(3600),
        );
        assert!(!engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_address_to_script_length() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        // A valid P2PKH script is always 25 bytes
        let script = engine
            .address_to_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT")
            .expect("valid address");
        assert_eq!(script.len(), 25);
    }

    #[test]
    fn test_address_to_script_p2pkh() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let script = engine
            .address_to_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT")
            .expect("valid address");
        // Standard P2PKH: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
        assert_eq!(script.len(), 25);
        assert_eq!(&script[..3], &[0x76, 0xa9, 0x14]);
        assert_eq!(&script[23..], &[0x88, 0xac]);
    }
}

#[cfg(test)]
mod conflict_tests {
    use super::*;
    use crate::block::{Transaction, TxInput, TxOutput, TxType};
    use crate::chain::Utxo;
    use crate::consensus::COIN;

    fn make_utxo(value: u64, age_seconds: u32) -> Utxo {
        let now = 1_700_000_000u32;
        Utxo {
            txid: [1u8; 32],
            vout: 0,
            value,
            script_pubkey: vec![0x76, 0xa9, 0x14],
            height: 100,
            timestamp: now - age_seconds,
        }
    }

    fn transfer_spending(txid: [u8; 32], vout: u32) -> Transaction {
        Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: txid,
                prev_vout: vout,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value: 1000,
                script_pubkey: vec![],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        }
    }

    /// A pending mempool tx spending the same outpoint the coinstake spends
    /// must be excluded from the block template — otherwise the block fails
    /// UTXO validation and staking wedges forever.
    #[test]
    fn test_build_stake_block_excludes_conflicting_mempool_tx() {
        let engine = StakingEngine::new_fast("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(1000 * COIN, (MIN_STAKE_AGE as u32).saturating_add(3600));
        // A stale mempool tx spending the exact outpoint the coinstake will spend.
        let conflicting = transfer_spending([1u8; 32], 0);
        // An unrelated pending tx that must be kept.
        let unrelated = transfer_spending([9u8; 32], 3);

        // Scan timestamps until a kernel hits so the block actually builds.
        let prev_modifier = 0xdead_beef_u64;
        let mut block = None;
        for ts in 1_700_000_000..1_700_000_000 + 3600 {
            block = engine.build_stake_block(
                [2u8; 32],
                prev_modifier,
                101,
                ts,
                vec![utxo.clone()],
                vec![conflicting.clone(), unrelated.clone()],
            );
            if block.is_some() {
                break;
            }
        }
        let block = block.expect("kernel should hit within an hour of timestamps");
        assert_eq!(block.transactions[0].tx_type, TxType::Coinstake);
        let txids: Vec<[u8; 32]> = block.transactions[1..].iter().map(|t| t.txid()).collect();
        assert!(
            !txids.contains(&conflicting.txid()),
            "conflicting mempool tx must be excluded from the block"
        );
        assert!(
            txids.contains(&unrelated.txid()),
            "unrelated mempool tx must be included"
        );
    }
}
