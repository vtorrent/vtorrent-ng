//! Transaction builder with coin selection and secp256k1 UTXO signing.
//!
//! # Overview
//!
//! `TxBuilder` constructs a signed `Transaction` from wallet keys and a set of
//! available UTXOs. It handles:
//!
//! - **Coin selection** — greedy largest-first algorithm with dust filtering
//! - **Change output** — automatically added when selected inputs exceed target
//! - **P2PKH script signing** — DER-encoded ECDSA signature + compressed pubkey
//! - **Fee estimation** — based on serialized transaction size × fee rate
//! - **RBF signalling** — opt-in Replace-By-Fee via sequence numbers
//!
//! # Example
//!
//! ```no_run
//! use vtorrent_wallet::tx_builder::TxBuilder;
//! use vtorrent_node::chain::Utxo;
//!
//! let utxos: Vec<Utxo> = vec![]; // from chain.get_utxos_for_address(...)
//! let tx = TxBuilder::new()
//!     .recipient("VRecipientAddress123", 1_000_000)
//!     .fee_rate(10) // satoshis per byte
//!     .sign_with_wif("7YourPrivateKeyWIF")
//!     .build(&utxos)
//!     .unwrap();
//! ```

use crate::error::{Result, WalletError};
use secp256k1::{Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use vtorrent_node::block::{Transaction, TxInput, TxOutput, TxType};
use vtorrent_node::chain::Utxo;

/// Dust threshold: outputs below this value are not economical to spend.
pub const DUST_SATOSHIS: u64 = 546;

/// Default fee rate in satoshis per byte.
pub const DEFAULT_FEE_RATE: u64 = 10;

/// Approximate size of a P2PKH input in bytes (32 txid + 4 vout + 107 scriptsig + 4 seq).
/// Absolute minimum transaction fee accepted by the node's relay policy.
/// Must stay in sync with `vtorrent_node::mempool::MIN_RELAY_FEE` (a unit
/// test in the node crate asserts equality).
pub const MIN_ABSOLUTE_FEE_SATS: u64 = 1_000;

const P2PKH_INPUT_SIZE: usize = 147;

/// Approximate size of a P2PKH output in bytes (8 value + 25 scriptpubkey).
const P2PKH_OUTPUT_SIZE: usize = 33;

/// Fixed transaction overhead in bytes (version + locktime + input/output count varints).
const TX_OVERHEAD: usize = 10;

// ─── Coin selection ───────────────────────────────────────────────────────────

/// Select UTXOs to cover `target_sats` + estimated fee using a greedy
/// largest-first algorithm. Returns the selected UTXOs and the total fee.
///
/// This algorithm is simple and predictable. It prefers large UTXOs to
/// minimize the number of inputs and therefore the transaction size.
pub fn select_coins(
    utxos: &[Utxo],
    target_sats: u64,
    fee_rate: u64,
    min_absolute_fee: u64,
    n_outputs: usize,
) -> Result<(Vec<Utxo>, u64)> {
    // Filter out dust UTXOs.
    let mut candidates: Vec<&Utxo> = utxos.iter().filter(|u| u.value >= DUST_SATOSHIS).collect();

    // Sort by value descending (largest first).
    candidates.sort_by_key(|utxo| std::cmp::Reverse(utxo.value));

    let mut selected: Vec<Utxo> = Vec::new();
    let mut selected_value: u64 = 0;

    for utxo in candidates {
        selected.push(utxo.clone());
        selected_value = selected_value
            .checked_add(utxo.value)
            .ok_or_else(|| WalletError::BuildError("UTXO value overflow".into()))?;

        // Estimate fee for the current selection.
        let n_inputs = selected.len();
        let tx_size = TX_OVERHEAD + n_inputs * P2PKH_INPUT_SIZE + n_outputs * P2PKH_OUTPUT_SIZE;
        let fee = ((tx_size as u64).saturating_mul(fee_rate)).max(min_absolute_fee);

        let required = target_sats.saturating_add(fee);
        if selected_value >= required {
            return Ok((selected, fee));
        }
    }

    Err(WalletError::InsufficientFunds {
        available: selected_value,
        required: target_sats,
    })
}

// ─── Script builders ──────────────────────────────────────────────────────────

/// Build a P2PKH scriptPubKey for the given address.
///
/// Format: OP_DUP OP_HASH160 <20-byte hash160> OP_EQUALVERIFY OP_CHECKSIG
pub fn p2pkh_script_pubkey(address: &str) -> Result<Vec<u8>> {
    let hash160 = address_to_hash160(address)?;
    let mut script = Vec::with_capacity(25);
    script.push(0x76); // OP_DUP
    script.push(0xa9); // OP_HASH160
    script.push(0x14); // push 20 bytes
    script.extend_from_slice(&hash160);
    script.push(0x88); // OP_EQUALVERIFY
    script.push(0xac); // OP_CHECKSIG
    Ok(script)
}

/// Decode a vTorrent/Bitcoin Base58Check address to its 20-byte Hash160.
fn address_to_hash160(address: &str) -> Result<[u8; 20]> {
    let decoded = bs58::decode(address)
        .into_vec()
        .map_err(|_| WalletError::InvalidAddress(address.to_string()))?;

    // Exact: 1 version byte + 20 hash bytes + 4 checksum bytes = 25 bytes.
    // Reject longer payloads to prevent trailing garbage from passing validation.
    if decoded.len() != 25 {
        return Err(WalletError::InvalidAddress(format!(
            "Address too short: {}",
            address
        )));
    }

    // Verify checksum.
    let (payload, check) = decoded.split_at(decoded.len() - 4);
    let expected = double_sha256_checksum(payload);
    if check != expected {
        return Err(WalletError::InvalidAddress(format!(
            "Address checksum mismatch: {}",
            address
        )));
    }

    // The version byte must be the vTorrent mainnet P2PKH prefix (70).
    // Without this check a Base58Check address from any other network
    // (e.g. a Bitcoin `1...` address) passes validation and funds sent to
    // it are unrecoverable on the VTR chain.
    if payload[0] != vtorrent_core::network::legacy::PUBKEY_ADDRESS_PREFIX {
        return Err(WalletError::InvalidAddress(format!(
            "Address {} is not a vTorrent mainnet address (version byte {})",
            address, payload[0]
        )));
    }

    // payload[1..21] is the hash160.
    let mut hash = [0u8; 20];
    hash.copy_from_slice(&payload[1..21]);
    Ok(hash)
}

fn double_sha256_checksum(data: &[u8]) -> [u8; 4] {
    let h1 = Sha256::digest(data);
    let h2 = Sha256::digest(h1);
    [h2[0], h2[1], h2[2], h2[3]]
}

// ─── Signing ──────────────────────────────────────────────────────────────────

/// Compute the sighash for a P2PKH input.
///
/// Delegates to `Transaction::sighash` in `vtorrent-node` so the signer and the
/// chain's verifier use the identical message.
fn compute_sighash(tx: &Transaction, input_index: usize, subscript: &[u8]) -> Result<[u8; 32]> {
    Ok(tx.sighash(input_index, subscript))
}

/// Build a DER-encoded ECDSA signature + SIGHASH_ALL byte.
fn sign_input(secret_key_bytes: &[u8; 32], sighash: &[u8; 32]) -> Result<Vec<u8>> {
    let secp = Secp256k1::new();
    let secret_key =
        SecretKey::from_slice(secret_key_bytes).map_err(|e| WalletError::Signing(e.to_string()))?;
    let message = Message::from_digest(*sighash);
    let sig = secp.sign_ecdsa(&message, &secret_key);
    let mut der = sig.serialize_der().to_vec();
    der.push(0x01); // SIGHASH_ALL
    Ok(der)
}

/// Build a P2PKH scriptSig: <sig> <pubkey>.
fn build_script_sig(sig: &[u8], pubkey: &[u8]) -> Vec<u8> {
    // DER-encoded ECDSA signatures are typically 70-72 bytes; compressed
    // pubkeys are exactly 33.  Reject pathological inputs to avoid
    // silent length-byte truncation.
    assert!(sig.len() <= 255, "signature too large: {} bytes", sig.len());
    assert!(
        pubkey.len() <= 255,
        "pubkey too large: {} bytes",
        pubkey.len()
    );
    let mut script = Vec::with_capacity(1 + sig.len() + 1 + pubkey.len());
    script.push(sig.len() as u8);
    script.extend_from_slice(sig);
    script.push(pubkey.len() as u8);
    script.extend_from_slice(pubkey);
    script
}

// ─── TxBuilder ────────────────────────────────────────────────────────────────

/// Fluent transaction builder.
pub struct TxBuilder {
    recipients: Vec<(String, u64)>,
    fee_rate: u64,
    min_absolute_fee: u64,
    wif_keys: Vec<String>,
    change_address: Option<String>,
    signal_rbf: bool,
    lock_time: u32,
}

impl TxBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            recipients: Vec::new(),
            fee_rate: DEFAULT_FEE_RATE,
            min_absolute_fee: 0,
            wif_keys: Vec::new(),
            change_address: None,
            signal_rbf: false,
            lock_time: 0,
        }
    }

    /// Add a recipient (address, amount in satoshis).
    pub fn recipient(mut self, address: &str, amount_sats: u64) -> Self {
        self.recipients.push((address.to_string(), amount_sats));
        self
    }

    /// Set the fee rate in satoshis per byte (default: 10).
    pub fn fee_rate(mut self, sats_per_byte: u64) -> Self {
        self.fee_rate = sats_per_byte;
        self
    }

    /// Enforce an absolute minimum fee regardless of the size estimate.
    /// Set this to the node's relay floor so small transfers are not rejected.
    pub fn min_absolute_fee(mut self, sats: u64) -> Self {
        self.min_absolute_fee = sats;
        self
    }

    /// Add a WIF-encoded private key for signing inputs.
    ///
    /// Multiple keys can be added for multi-input transactions.
    pub fn sign_with_wif(mut self, wif: &str) -> Self {
        self.wif_keys.push(wif.to_string());
        self
    }

    /// Set the change address. If not set, change goes to the first signing key's address.
    pub fn change_address(mut self, address: &str) -> Self {
        self.change_address = Some(address.to_string());
        self
    }

    /// Enable opt-in Replace-By-Fee (BIP-125) signalling.
    pub fn signal_rbf(mut self) -> Self {
        self.signal_rbf = true;
        self
    }

    /// Set the transaction lock time.
    pub fn lock_time(mut self, lock_time: u32) -> Self {
        self.lock_time = lock_time;
        self
    }

    /// Build and sign the transaction using the provided UTXOs.
    ///
    /// Returns a fully signed `Transaction` ready to be submitted to the mempool.
    pub fn build(self, available_utxos: &[Utxo]) -> Result<Transaction> {
        if self.recipients.is_empty() {
            return Err(WalletError::BuildError("No recipients specified".into()));
        }
        if self.wif_keys.is_empty() {
            return Err(WalletError::BuildError("No signing keys specified".into()));
        }

        // Decode all signing keys.
        let secp = Secp256k1::new();
        let mut key_pairs: Vec<([u8; 32], Vec<u8>)> = Vec::new(); // (secret_bytes, compressed_pubkey)
        for wif in &self.wif_keys {
            let key = vtorrent_core::keys::PrivateKey::from_wif(wif)
                .map_err(|e| WalletError::Signing(e.to_string()))?;
            let secret_key = SecretKey::from_slice(key.as_bytes())
                .map_err(|e| WalletError::Signing(e.to_string()))?;
            let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
            let pubkey_bytes = pubkey.serialize().to_vec(); // compressed
            key_pairs.push((*key.as_bytes(), pubkey_bytes));
        }

        // Determine change address.
        let change_addr = if let Some(ref addr) = self.change_address {
            addr.clone()
        } else {
            // Derive address from first signing key.
            let (_, ref pubkey_bytes) = key_pairs[0];
            pubkey_to_vtorrent_address(pubkey_bytes)?
        };

        // Total amount to send.
        let total_send: u64 = self.recipients.iter().try_fold(0u64, |acc, (_, v)| {
            acc.checked_add(*v)
                .ok_or_else(|| WalletError::BuildError("Total send amount overflow".into()))
        })?;
        let n_outputs = self.recipients.len() + 1; // +1 for change

        // Coin selection.
        let (selected_utxos, fee) = select_coins(
            available_utxos,
            total_send,
            self.fee_rate,
            self.min_absolute_fee,
            n_outputs,
        )?;

        let total_input: u64 = selected_utxos.iter().try_fold(0u64, |acc, u| {
            acc.checked_add(u.value)
                .ok_or_else(|| WalletError::BuildError("Total input amount overflow".into()))
        })?;
        let change = total_input.saturating_sub(total_send.saturating_add(fee));

        // Build unsigned inputs.
        let sequence = if self.signal_rbf {
            0xFFFFFFFD
        } else {
            0xFFFFFFFF
        };
        let inputs: Vec<TxInput> = selected_utxos
            .iter()
            .map(|u| TxInput {
                prev_txid: u.txid,
                prev_vout: u.vout,
                script_sig: Vec::new(), // filled in during signing
                sequence,
            })
            .collect();

        // Build outputs.
        let mut outputs: Vec<TxOutput> = Vec::new();
        for (addr, amount) in &self.recipients {
            // Reject dust recipients: an output below the dust threshold costs
            // more to spend than it is worth and would linger in the UTXO set.
            if *amount < DUST_SATOSHIS {
                return Err(WalletError::BuildError(format!(
                    "Recipient amount {} sat is below the dust threshold {} sat",
                    amount, DUST_SATOSHIS
                )));
            }
            outputs.push(TxOutput {
                value: *amount,
                script_pubkey: p2pkh_script_pubkey(addr)?,
            });
        }
        // Add change output if above dust threshold.
        if change >= DUST_SATOSHIS {
            outputs.push(TxOutput {
                value: change,
                script_pubkey: p2pkh_script_pubkey(&change_addr)?,
            });
        }

        // Assemble unsigned transaction.
        let mut tx = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs,
            outputs,
            lock_time: self.lock_time,
            claim_address: None,
            claim_signature: None,
        };

        // Sign each input, matching each UTXO to the key that owns it.
        for (input_index, utxo) in selected_utxos.iter().enumerate() {
            // The subscript is the previous output's scriptPubKey.
            let subscript = utxo.script_pubkey.clone();

            // Compute sighash.
            let sighash = compute_sighash(&tx, input_index, &subscript)?;

            // Match the signing key to this UTXO's P2PKH script. Fall back to
            // the sole key for single-key wallets; error when a UTXO cannot be
            // matched (signing with the wrong key would produce an invalid
            // signature rejected by the chain).
            let (secret_bytes, pubkey_bytes) = match find_key_for_script(&key_pairs, &subscript) {
                Some(kp) => kp,
                None if key_pairs.len() == 1 => &key_pairs[0],
                None => {
                    return Err(WalletError::BuildError(format!(
                        "No signing key matches UTXO {}",
                        hex::encode(utxo.txid)
                    )))
                }
            };

            let sig = sign_input(secret_bytes, &sighash)?;
            let script_sig = build_script_sig(&sig, pubkey_bytes);

            tx.inputs[input_index].script_sig = script_sig;
        }

        Ok(tx)
    }
}

/// Sign a pre-built transaction whose inputs spend the supplied P2PKH UTXOs.
///
/// This preserves custom outputs and transaction types (such as `AtomicSwap`)
/// while applying the same SIGHASH_ALL and scriptSig format as `TxBuilder`.
pub fn sign_custom_transaction(
    mut tx: Transaction,
    input_utxos: &[Utxo],
    wif: &str,
) -> Result<Transaction> {
    if tx.inputs.len() != input_utxos.len() {
        return Err(WalletError::BuildError(
            "Transaction input count does not match provided UTXOs".into(),
        ));
    }

    let key = vtorrent_core::keys::PrivateKey::from_wif(wif)
        .map_err(|e| WalletError::Signing(e.to_string()))?;
    let secret_key =
        SecretKey::from_slice(key.as_bytes()).map_err(|e| WalletError::Signing(e.to_string()))?;
    let secp = Secp256k1::new();
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key)
        .serialize()
        .to_vec();

    for (input_index, utxo) in input_utxos.iter().enumerate() {
        let (prev_txid, prev_vout) = {
            let input = &tx.inputs[input_index];
            (input.prev_txid, input.prev_vout)
        };
        if prev_txid != utxo.txid || prev_vout != utxo.vout {
            return Err(WalletError::BuildError(
                "Transaction input does not match its signing UTXO".into(),
            ));
        }
        let sighash = compute_sighash(&tx, input_index, &utxo.script_pubkey)?;
        let signature = sign_input(key.as_bytes(), &sighash)?;
        tx.inputs[input_index].script_sig = build_script_sig(&signature, &pubkey);
    }

    Ok(tx)
}

/// Sign a single input of a pre-built transaction over an explicit subscript.
///
/// Returns the DER signature (with SIGHASH_ALL) and the compressed pubkey.
/// Used for HTLC claim/refund, where the subscript is the HTLC script rather
/// than a P2PKH scriptPubKey.
pub fn sign_input_over_subscript(
    tx: &Transaction,
    input_index: usize,
    subscript: &[u8],
    wif: &str,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let key = vtorrent_core::keys::PrivateKey::from_wif(wif)
        .map_err(|e| WalletError::Signing(e.to_string()))?;
    let secret_key =
        SecretKey::from_slice(key.as_bytes()).map_err(|e| WalletError::Signing(e.to_string()))?;
    let secp = Secp256k1::new();
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key)
        .serialize()
        .to_vec();

    let sighash = compute_sighash(tx, input_index, subscript)?;
    let signature = sign_input(key.as_bytes(), &sighash)?;
    Ok((signature, pubkey))
}

impl Default for TxBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Address derivation ───────────────────────────────────────────────────────

/// Derive a vTorrent address (version byte 70 → starts with 'V') from a
/// compressed public key.
pub fn pubkey_to_vtorrent_address(compressed_pubkey: &[u8]) -> Result<String> {
    use ripemd::Digest as RipemdDigest;
    use ripemd::Ripemd160;
    use sha2::Sha256;

    // Hash160 = RIPEMD160(SHA256(pubkey))
    let sha256_hash = Sha256::digest(compressed_pubkey);
    let hash160 = Ripemd160::digest(sha256_hash);

    // Version byte 70 gives addresses starting with 'V'.
    let mut payload = Vec::with_capacity(25);
    payload.push(70u8); // version byte
    payload.extend_from_slice(&hash160);

    // 4-byte checksum.
    let checksum = double_sha256_checksum(&payload);
    payload.extend_from_slice(&checksum);

    Ok(bs58::encode(payload).into_string())
}

/// Build the standard P2PKH scriptPubKey for a compressed public key.
fn pubkey_to_p2pkh_script(compressed_pubkey: &[u8]) -> Vec<u8> {
    use ripemd::Digest as _;
    use ripemd::Ripemd160;
    use sha2::Sha256;

    let sha256_hash = Sha256::digest(compressed_pubkey);
    let hash160 = Ripemd160::digest(sha256_hash);

    let mut script = Vec::with_capacity(25);
    script.push(0x76); // OP_DUP
    script.push(0xa9); // OP_HASH160
    script.push(0x14); // push 20 bytes
    script.extend_from_slice(&hash160);
    script.push(0x88); // OP_EQUALVERIFY
    script.push(0xac); // OP_CHECKSIG
    script
}

/// Find the signing key whose P2PKH script matches the given scriptPubKey.
fn find_key_for_script<'a>(
    key_pairs: &'a [([u8; 32], Vec<u8>)],
    script_pubkey: &[u8],
) -> Option<&'a ([u8; 32], Vec<u8>)> {
    key_pairs
        .iter()
        .find(|(_, pubkey_bytes)| pubkey_to_p2pkh_script(pubkey_bytes) == script_pubkey)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vtorrent_core::keys::PrivateKey;
    use vtorrent_node::chain::Utxo;

    fn make_utxo(txid_byte: u8, vout: u32, value: u64, script: Vec<u8>) -> Utxo {
        Utxo {
            txid: [txid_byte; 32],
            vout,
            value,
            script_pubkey: script,
            height: 1,
            timestamp: 1_700_000_000,
        }
    }

    fn random_wif() -> (String, String) {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        loop {
            rand::thread_rng().fill_bytes(&mut bytes);
            if let Ok(key) = PrivateKey::from_bytes(bytes, true) {
                let wif = key.to_wif(198); // vTorrent WIF prefix
                let secp = Secp256k1::new();
                let sk = SecretKey::from_slice(key.as_bytes()).unwrap();
                let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
                let addr = pubkey_to_vtorrent_address(&pk.serialize()).unwrap();
                return (wif, addr);
            }
        }
    }

    #[test]
    fn test_address_from_pubkey_starts_with_v() {
        let (_, addr) = random_wif();
        assert!(addr.starts_with('V'), "Expected 'V' prefix, got: {}", addr);
    }

    #[test]
    fn test_dust_recipient_rejected() {
        let (wif, change_addr) = random_wif();
        let script = p2pkh_script_pubkey(&change_addr).unwrap();
        let utxos = vec![make_utxo(1, 0, 50_000_000_000, script)];
        let result = TxBuilder::new()
            .recipient(&change_addr, DUST_SATOSHIS - 1)
            .change_address(&change_addr)
            .sign_with_wif(&wif)
            .build(&utxos);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("below the dust threshold"),
            "dust recipient must be rejected"
        );
    }

    #[test]
    fn test_recipient_exactly_dust_accepted() {
        let (wif, change_addr) = random_wif();
        let script = p2pkh_script_pubkey(&change_addr).unwrap();
        let utxos = vec![make_utxo(2, 0, 50_000_000_000, script)];
        let tx = TxBuilder::new()
            .recipient(&change_addr, DUST_SATOSHIS)
            .change_address(&change_addr)
            .sign_with_wif(&wif)
            .build(&utxos)
            .unwrap();
        assert!(tx.outputs.iter().any(|o| o.value == DUST_SATOSHIS));
    }

    #[test]
    fn test_p2pkh_script_pubkey_length() {
        let (_, addr) = random_wif();
        let script = p2pkh_script_pubkey(&addr).unwrap();
        assert_eq!(script.len(), 25);
        assert_eq!(script[0], 0x76); // OP_DUP
        assert_eq!(script[1], 0xa9); // OP_HASH160
        assert_eq!(script[2], 0x14); // push 20 bytes
        assert_eq!(script[23], 0x88); // OP_EQUALVERIFY
        assert_eq!(script[24], 0xac); // OP_CHECKSIG
    }

    #[test]
    fn test_coin_selection_sufficient_funds() {
        let (wif, addr) = random_wif();
        let script = p2pkh_script_pubkey(&addr).unwrap();
        let utxos = vec![
            make_utxo(1, 0, 5_000_000, script.clone()),
            make_utxo(2, 0, 3_000_000, script.clone()),
        ];
        let (selected, fee) = select_coins(&utxos, 4_000_000, 10, 0, 2).unwrap();
        let total: u64 = selected.iter().map(|u| u.value).sum();
        assert!(total >= 4_000_000 + fee);
        let _ = wif; // suppress unused warning
    }

    #[test]
    fn test_coin_selection_insufficient_funds() {
        let script = vec![0x76, 0xa9];
        let utxos = vec![make_utxo(1, 0, 100_000, script)];
        let result = select_coins(&utxos, 5_000_000, 10, 0, 2);
        assert!(matches!(result, Err(WalletError::InsufficientFunds { .. })));
    }

    #[test]
    fn test_build_and_sign_transaction() {
        let (wif, sender_addr) = random_wif();
        let (_, recipient_addr) = random_wif();

        let script = p2pkh_script_pubkey(&sender_addr).unwrap();
        let utxos = vec![make_utxo(1, 0, 10_000_000, script.clone())];

        let tx = TxBuilder::new()
            .recipient(&recipient_addr, 5_000_000)
            .fee_rate(10)
            .sign_with_wif(&wif)
            .build(&utxos)
            .unwrap();

        // Transaction should have 1 input and 2 outputs (recipient + change).
        assert_eq!(tx.inputs.len(), 1);
        assert_eq!(tx.outputs.len(), 2);

        // All inputs should have non-empty scriptSigs.
        for input in &tx.inputs {
            assert!(
                !input.script_sig.is_empty(),
                "Input scriptSig should not be empty"
            );
        }

        // Total output should be less than total input (fee consumed).
        let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
        assert!(total_out < 10_000_000);
        assert!(total_out > 9_000_000); // fee should be < 1_000_000 for a small tx
    }

    #[test]
    fn test_build_no_change_when_exact() {
        let (wif, sender_addr) = random_wif();
        let (_, recipient_addr) = random_wif();

        let script = p2pkh_script_pubkey(&sender_addr).unwrap();
        // Provide exactly the right amount (fee will be deducted from change).
        let utxos = vec![make_utxo(1, 0, 1_000_000, script)];

        // Send 990_000 — fee ~1470 sat for 1-in-2-out tx at 10 sat/byte.
        let tx = TxBuilder::new()
            .recipient(&recipient_addr, 990_000)
            .fee_rate(10)
            .sign_with_wif(&wif)
            .build(&utxos)
            .unwrap();

        // Should have at least 1 output (recipient).
        assert!(!tx.outputs.is_empty());
        let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
        assert!(total_out <= 1_000_000);
    }

    #[test]
    fn test_rbf_signalling() {
        let (wif, sender_addr) = random_wif();
        let (_, recipient_addr) = random_wif();
        let script = p2pkh_script_pubkey(&sender_addr).unwrap();
        let utxos = vec![make_utxo(1, 0, 10_000_000, script)];

        let tx = TxBuilder::new()
            .recipient(&recipient_addr, 5_000_000)
            .signal_rbf()
            .sign_with_wif(&wif)
            .build(&utxos)
            .unwrap();

        assert!(tx.signals_rbf(), "Transaction should signal RBF");
    }

    #[test]
    fn test_multiple_recipients() {
        let (wif, sender_addr) = random_wif();
        let (_, r1) = random_wif();
        let (_, r2) = random_wif();

        let script = p2pkh_script_pubkey(&sender_addr).unwrap();
        let utxos = vec![make_utxo(1, 0, 20_000_000, script)];

        let tx = TxBuilder::new()
            .recipient(&r1, 5_000_000)
            .recipient(&r2, 5_000_000)
            .sign_with_wif(&wif)
            .build(&utxos)
            .unwrap();

        // 2 recipients + 1 change = 3 outputs.
        assert_eq!(tx.outputs.len(), 3);
    }

    #[test]
    fn test_sign_custom_transaction_preserves_atomic_swap_type() {
        let (wif, sender_addr) = random_wif();
        let (_, recipient_addr) = random_wif();
        let utxo = make_utxo(9, 1, 2_000_000, p2pkh_script_pubkey(&sender_addr).unwrap());
        let custom = Transaction {
            version: 1,
            tx_type: TxType::AtomicSwap,
            inputs: vec![TxInput {
                prev_txid: utxo.txid,
                prev_vout: utxo.vout,
                script_sig: Vec::new(),
                sequence: u32::MAX - 1,
            }],
            outputs: vec![TxOutput {
                value: 1_990_000,
                script_pubkey: p2pkh_script_pubkey(&recipient_addr).unwrap(),
            }],
            lock_time: 0,
            claim_address: Some(recipient_addr),
            claim_signature: None,
        };

        let signed = sign_custom_transaction(custom, &[utxo], &wif).unwrap();
        assert_eq!(signed.tx_type, TxType::AtomicSwap);
        assert!(!signed.inputs[0].script_sig.is_empty());
    }

    #[test]
    fn test_txid_is_deterministic() {
        let (wif, sender_addr) = random_wif();
        let (_, recipient_addr) = random_wif();
        let script = p2pkh_script_pubkey(&sender_addr).unwrap();
        let utxos = vec![make_utxo(1, 0, 10_000_000, script)];

        let tx1 = TxBuilder::new()
            .recipient(&recipient_addr, 5_000_000)
            .sign_with_wif(&wif)
            .build(&utxos)
            .unwrap();

        let tx2 = TxBuilder::new()
            .recipient(&recipient_addr, 5_000_000)
            .sign_with_wif(&wif)
            .build(&utxos)
            .unwrap();

        assert_eq!(
            tx1.txid(),
            tx2.txid(),
            "Same inputs/outputs should produce same txid"
        );
    }

    #[test]
    fn test_signed_tx_verifies_against_chain_sighash() {
        use vtorrent_script::{Engine, Script, ScriptEnv};

        let (wif, sender_addr) = random_wif();
        let (_, recipient_addr) = random_wif();
        let script = p2pkh_script_pubkey(&sender_addr).unwrap();
        let utxos = vec![make_utxo(1, 0, 10_000_000, script.clone())];

        let tx = TxBuilder::new()
            .recipient(&recipient_addr, 5_000_000)
            .sign_with_wif(&wif)
            .build(&utxos)
            .unwrap();

        // The chain verifies each input over tx.sighash(i, subscript); the
        // wallet must have signed over the identical message.
        let input = &tx.inputs[0];
        let tx_hash = tx.sighash(0, &script);
        let env = ScriptEnv {
            tx_hash,
            block_height: 1,
            block_time: 1_700_000_000,
            tx_lock_time: tx.lock_time,
            input_sequence: 0xffff_fffe,
        };
        let mut engine = Engine::new(env);
        let script_sig = Script::from_bytes(input.script_sig.clone()).unwrap();
        let script_pubkey = Script::from_bytes(script).unwrap();
        engine.execute(&script_sig, &script_pubkey).unwrap();
    }

    #[test]
    fn test_htlc_claim_verifies_against_script_engine() {
        use vtorrent_node::atomic_swap::Htlc;
        use vtorrent_script::{Engine, Script, ScriptEnv};

        let (taker_wif, taker_addr) = random_wif();
        let (_, maker_addr) = random_wif();

        let preimage = [42u8; 32];
        let hash_lock = {
            use sha2::Digest;
            let mut h = Sha256::new();
            h.update(preimage);
            let d = h.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&d);
            out
        };

        let htlc = Htlc::new(
            hash_lock,
            taker_addr.clone(),
            maker_addr.clone(),
            vtorrent_node::atomic_swap::DEFAULT_HTLC_LOCKTIME,
            100_000_000,
        )
        .unwrap();
        let htlc_script = htlc.build_script().unwrap();

        // Build the unsigned claim and sign over the HTLC script.
        let unsigned = htlc
            .build_claim_tx_unsigned([1u8; 32], &preimage, 10_000)
            .unwrap();
        let (sig, pubkey) =
            sign_input_over_subscript(&unsigned, 0, &htlc_script, &taker_wif).unwrap();

        // Assemble the scriptSig: <sig> <pubkey> <preimage> OP_1.
        let mut script_sig = Vec::new();
        script_sig.push(sig.len() as u8);
        script_sig.extend_from_slice(&sig);
        script_sig.push(pubkey.len() as u8);
        script_sig.extend_from_slice(&pubkey);
        script_sig.push(0x20);
        script_sig.extend_from_slice(&preimage);
        script_sig.push(0x51);

        let mut claim_tx = unsigned;
        claim_tx.inputs[0].script_sig = script_sig;

        // The chain verifies over tx.sighash(0, htlc_script).
        let tx_hash = claim_tx.sighash(0, &htlc_script);
        let env = ScriptEnv {
            tx_hash,
            block_height: 1,
            block_time: 1_700_000_000,
            tx_lock_time: claim_tx.lock_time,
            input_sequence: 0xffff_ffff,
        };
        let mut engine = Engine::new(env);
        let script_sig = Script::from_bytes(claim_tx.inputs[0].script_sig.clone()).unwrap();
        let script_pubkey = Script::from_bytes(htlc_script).unwrap();
        engine.execute(&script_sig, &script_pubkey).unwrap();
    }

    /// The VTR HTLC claim branch requires an exactly-32-byte preimage
    /// (OP_SIZE guard, matching the BTC-side script). A hand-crafted scriptSig
    /// with a different-length preimage must fail script execution.
    #[test]
    fn test_htlc_claim_rejects_non_32_byte_preimage() {
        use vtorrent_node::atomic_swap::Htlc;
        use vtorrent_script::{Engine, Script, ScriptEnv};

        let (taker_wif, taker_addr) = random_wif();
        let (_, maker_addr) = random_wif();

        // 20-byte preimage (wrong length) whose SHA256 is the hash lock.
        let preimage = [7u8; 20];
        let hash_lock = {
            use sha2::Digest;
            let mut h = Sha256::new();
            h.update(preimage);
            let d = h.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&d);
            out
        };

        let htlc = Htlc::new(
            hash_lock,
            taker_addr.clone(),
            maker_addr.clone(),
            vtorrent_node::atomic_swap::DEFAULT_HTLC_LOCKTIME,
            100_000_000,
        )
        .unwrap();
        let htlc_script = htlc.build_script().unwrap();

        // Build a minimal claim tx manually (the sighash covers the HTLC
        // script but not the scriptSig, so the preimage length is free here —
        // exactly the case the OP_SIZE guard defends against).
        let unsigned = Transaction {
            version: 1,
            tx_type: vtorrent_node::block::TxType::AtomicSwap,
            inputs: vec![vtorrent_node::block::TxInput {
                prev_txid: [1u8; 32],
                prev_vout: 0,
                script_sig: Vec::new(),
                sequence: 0xffff_ffff,
            }],
            outputs: vec![vtorrent_node::block::TxOutput {
                value: 100_000_000 - 10_000,
                script_pubkey: p2pkh_script_pubkey(&taker_addr).unwrap(),
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let (sig, pubkey) =
            sign_input_over_subscript(&unsigned, 0, &htlc_script, &taker_wif).unwrap();

        let mut script_sig = Vec::new();
        script_sig.push(sig.len() as u8);
        script_sig.extend_from_slice(&sig);
        script_sig.push(pubkey.len() as u8);
        script_sig.extend_from_slice(&pubkey);
        script_sig.push(preimage.len() as u8);
        script_sig.extend_from_slice(&preimage);
        script_sig.push(0x51);

        let mut claim_tx = unsigned;
        claim_tx.inputs[0].script_sig = script_sig;

        let tx_hash = claim_tx.sighash(0, &htlc_script);
        let env = ScriptEnv {
            tx_hash,
            block_height: 1,
            block_time: 1_700_000_000,
            tx_lock_time: claim_tx.lock_time,
            input_sequence: 0xffff_ffff,
        };
        let mut engine = Engine::new(env);
        let script_sig = Script::from_bytes(claim_tx.inputs[0].script_sig.clone()).unwrap();
        let script_pubkey = Script::from_bytes(htlc_script).unwrap();
        assert!(
            engine.execute(&script_sig, &script_pubkey).is_err(),
            "non-32-byte preimage must fail the OP_SIZE guard"
        );
    }

    #[test]
    fn test_htlc_refund_verifies_against_script_engine() {
        use vtorrent_node::atomic_swap::Htlc;
        use vtorrent_script::{Engine, Script, ScriptEnv};

        let (maker_wif, maker_addr) = random_wif();
        let (_, taker_addr) = random_wif();

        let preimage = [42u8; 32];
        let hash_lock = {
            use sha2::Digest;
            let mut h = Sha256::new();
            h.update(preimage);
            let d = h.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&d);
            out
        };

        // Use a short locktime so the refund is valid at the test's block time.
        let htlc = Htlc::new(
            hash_lock,
            taker_addr.clone(),
            maker_addr.clone(),
            vtorrent_node::atomic_swap::MIN_HTLC_LOCKTIME,
            100_000_000,
        )
        .unwrap();
        let htlc_script = htlc.build_script().unwrap();

        // Build the unsigned refund and sign over the HTLC script.
        let unsigned = htlc.build_refund_tx_unsigned([1u8; 32], 10_000).unwrap();
        let (sig, pubkey) =
            sign_input_over_subscript(&unsigned, 0, &htlc_script, &maker_wif).unwrap();

        // Assemble the scriptSig: <sig> <pubkey> OP_0.
        let mut script_sig = Vec::new();
        script_sig.push(sig.len() as u8);
        script_sig.extend_from_slice(&sig);
        script_sig.push(pubkey.len() as u8);
        script_sig.extend_from_slice(&pubkey);
        script_sig.push(0x00);

        let mut refund_tx = unsigned;
        refund_tx.inputs[0].script_sig = script_sig;

        // The chain verifies over tx.sighash(0, htlc_script) with a block time
        // past the HTLC expiry (so OP_CLTV passes).
        let tx_hash = refund_tx.sighash(0, &htlc_script);
        let env = ScriptEnv {
            tx_hash,
            block_height: 1,
            block_time: htlc.expiry + 1,
            tx_lock_time: refund_tx.lock_time,
            input_sequence: 0xffff_fffe,
        };
        let mut engine = Engine::new(env);
        let script_sig = Script::from_bytes(refund_tx.inputs[0].script_sig.clone()).unwrap();
        let script_pubkey = Script::from_bytes(htlc_script).unwrap();
        engine.execute(&script_sig, &script_pubkey).unwrap();
    }
}
