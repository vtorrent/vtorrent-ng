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
    block::{compute_merkle_root_from_txids, Block, BlockHeader, Transaction, TxInput, TxOutput, TxType},
    chain::Utxo,
    consensus::{
        check_stake_kernel, compute_pos_reward, compute_stake_modifier, MAX_STAKE_AGE,
        MIN_STAKE_AGE, MIN_STAKE_AMOUNT,
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
}

impl StakingEngine {
    /// Create a new staking engine for the given address.
    pub fn new(address: String) -> Self {
        Self { address, wif: None }
    }

    /// Create a new staking engine with a signing key.
    pub fn with_wif(address: String, wif: String) -> Self {
        Self {
            address,
            wif: Some(wif),
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
                let block = self.assemble_block(
                    prev_hash,
                    prev_stake_modifier,
                    timestamp,
                    coinstake,
                    pending_txs,
                );
                tracing::info!(
                    "Found stake kernel: utxo {}:{} at height {}",
                    hex::encode(utxo.txid),
                    utxo.vout,
                    height
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
        if (coin_age_seconds as u64) < MIN_STAKE_AGE {
            return false;
        }

        // Must not exceed maximum coin age (prevents very old coins from
        // dominating the staking weight indefinitely).
        if (coin_age_seconds as u64) > MAX_STAKE_AGE {
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
        let stake_output = TxOutput {
            value: utxo.value.saturating_add(reward),
            script_pubkey: self.address_to_script(&self.address),
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
    fn address_to_script(&self, address: &str) -> Vec<u8> {
        let Ok(addr) = vtorrent_core::address::Address::parse(address) else {
            // Fallback: OP_RETURN with address bytes
            let mut script = vec![0x6a];
            let addr_bytes = address.as_bytes();
            script.push(addr_bytes.len() as u8);
            script.extend_from_slice(addr_bytes);
            return script;
        };

        // Standard P2PKH: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
        let mut script = Vec::with_capacity(25);
        script.push(0x76); // OP_DUP
        script.push(0xa9); // OP_HASH160
        script.push(0x14); // push 20 bytes
        script.extend_from_slice(&addr.hash);
        script.push(0x88); // OP_EQUALVERIFY
        script.push(0xac); // OP_CHECKSIG
        script
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
        let script = engine.address_to_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT");
        // May be 25 (P2PKH) or fallback OP_RETURN — just check it's non-empty
        assert!(!script.is_empty());
    }

    #[test]
    fn test_address_to_script_p2pkh() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let script = engine.address_to_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT");
        // Standard P2PKH: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
        assert_eq!(script.len(), 25);
        assert_eq!(&script[..3], &[0x76, 0xa9, 0x14]);
        assert_eq!(&script[23..], &[0x88, 0xac]);
    }
}
