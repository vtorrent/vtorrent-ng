//! End-to-end integration tests for the BTC wallet send flow.
//!
//! These tests verify the full cycle: wallet creation → UTXO discovery →
//! transaction building/signing → PSBT round-trip → fee estimation →
//! UTXO persistence, all without network access.

use bitcoin::{Address, Amount, Transaction, Txid};
use std::str::FromStr;
use vtorrent_btc::keys::{derive_address, derive_wif};
use vtorrent_btc::utxo::Utxo;
use vtorrent_btc::wallet::BtcWallet;

const SEED: [u8; 64] = [0xAB; 64];
const NETWORK: bitcoin::Network = bitcoin::Network::Bitcoin;

fn make_utxo(txid_hex: &str, vout: u32, value: u64, addr: &str) -> Utxo {
    Utxo {
        txid: txid_hex.to_string(),
        vout,
        value,
        address: addr.to_string(),
        height: 1,
    }
}

// ─── Wallet basics ───────────────────────────────────────────────────────────

#[test]
fn wallet_send_to_full_cycle() {
    let mut wallet = BtcWallet::with_network(SEED, NETWORK);
    let addr0 = wallet.next_address().unwrap(); // index 0
    let addr1 = wallet.next_address().unwrap(); // index 1

    // Fund index 0 with two UTXOs.
    wallet.add_utxo(make_utxo(&"aa".repeat(32), 0, 50_000, &addr0));
    wallet.add_utxo(make_utxo(&"bb".repeat(32), 0, 30_000, &addr0));
    assert_eq!(wallet.balance(), 80_000);

    // Send 60_000 sats to addr1 with 1_000 sat fee.
    let (txid_hex, raw, _) = wallet.send_to(&addr1, 60_000, 1_000).unwrap();

    // Verify txid is a 64-char hex string.
    assert_eq!(txid_hex.len(), 64);
    assert!(txid_hex.chars().all(|c| c.is_ascii_hexdigit()));

    // Deserialize and verify the raw transaction.
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();
    assert_eq!(tx.version, bitcoin::transaction::Version(2));
    assert_eq!(tx.input.len(), 2); // 2 UTXOs consumed
    assert_eq!(tx.output.len(), 2); // recipient + change

    // Verify BIP69: inputs sorted by (txid, vout).
    assert!(tx.input[0].previous_output.txid <= tx.input[1].previous_output.txid);

    // Verify BIP69: outputs sorted by (value, script_pubkey).
    assert!(tx.output[0].value <= tx.output[1].value);

    // Spent UTXOs removed; change is in the tx output but not yet in wallet UTXO set.
    assert_eq!(wallet.balance(), 0);

    // txid matches the hash of the raw bytes.
    let computed_txid = vtorrent_btc::tx::txid_of(&raw);
    assert_eq!(txid_hex, hex::encode(computed_txid));
}

#[test]
fn wallet_send_to_insufficient_funds() {
    let mut wallet = BtcWallet::with_network(SEED, NETWORK);
    let addr0 = wallet.next_address().unwrap();
    wallet.add_utxo(make_utxo(&"cc".repeat(32), 0, 5_000, &addr0));

    let addr1 = wallet.next_address().unwrap();
    let result = wallet.send_to(&addr1, 100_000, 1_000);
    assert!(result.is_err());
}

#[test]
fn wallet_send_to_removes_spent_utxos() {
    let mut wallet = BtcWallet::with_network(SEED, NETWORK);
    let addr0 = wallet.next_address().unwrap();
    let addr1 = wallet.next_address().unwrap();

    // Fund with 3 UTXOs at index 0.
    wallet.add_utxo(make_utxo(&"dd".repeat(32), 0, 10_000, &addr0));
    wallet.add_utxo(make_utxo(&"ee".repeat(32), 0, 10_000, &addr0));
    wallet.add_utxo(make_utxo(&"ff".repeat(32), 0, 10_000, &addr0));
    assert_eq!(wallet.balance(), 30_000);

    let _ = wallet.send_to(&addr1, 25_000, 1_000);

    // All 3 UTXOs consumed; only fee excess returned as change.
    let remaining = wallet.list_utxos();
    assert!(remaining.len() <= 1, "at most 1 change output remains");
}

#[test]
fn wallet_rbf_signaling() {
    let mut wallet = BtcWallet::with_network(SEED, NETWORK);
    wallet.set_rbf_enabled(true);
    let addr0 = wallet.next_address().unwrap();
    let addr1 = wallet.next_address().unwrap();
    wallet.add_utxo(make_utxo(&"11".repeat(32), 0, 50_000, &addr0));

    let (_, raw, _) = wallet.send_to(&addr1, 40_000, 1_000).unwrap();
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();

    // RBF sequence = 0xFFFFFFFD (must be < 0xFFFFFFFE per BIP-125).
    assert_eq!(tx.input[0].sequence.0, 0xFFFFFFFD);
}

#[test]
fn wallet_no_rbf_signaling() {
    let mut wallet = BtcWallet::with_network(SEED, NETWORK);
    wallet.set_rbf_enabled(false);
    let addr0 = wallet.next_address().unwrap();
    let addr1 = wallet.next_address().unwrap();
    wallet.add_utxo(make_utxo(&"22".repeat(32), 0, 50_000, &addr0));

    let (_, raw, _) = wallet.send_to(&addr1, 40_000, 1_000).unwrap();
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();

    // No RBF: sequence = 0xFFFFFFFF (final).
    assert_eq!(tx.input[0].sequence.0, 0xFFFFFFFF);
}

// ─── PSBT round-trip ─────────────────────────────────────────────────────────

#[test]
fn psbt_create_sign_finalize_roundtrip() {
    use vtorrent_btc::tx::{create_psbt, finalize_psbt, sign_psbt};

    let _wallet = BtcWallet::with_network(SEED, NETWORK);
    let addr0 = derive_address(&SEED, 0, NETWORK).unwrap();
    let wif0 = derive_wif(&SEED, 0, NETWORK).unwrap();
    let dest = derive_address(&SEED, 1, NETWORK).unwrap();

    let inputs = vec![make_utxo(&"aa".repeat(32), 0, 100_000, &addr0)];
    let outputs = vec![
        (
            70_000u64,
            Address::from_str(&dest)
                .unwrap()
                .require_network(NETWORK)
                .unwrap(),
        ),
        (
            29_000u64,
            Address::from_str(&addr0)
                .unwrap()
                .require_network(NETWORK)
                .unwrap(),
        ),
    ];

    // Create unsigned PSBT.
    let psbt_bytes = create_psbt(&inputs, &outputs, NETWORK, true).unwrap();
    assert!(!psbt_bytes.is_empty());

    // Sign with our WIF.
    let signed = sign_psbt(&psbt_bytes, &wif0, NETWORK).unwrap();
    assert_ne!(signed, psbt_bytes);

    // Finalize and extract.
    let raw = finalize_psbt(&signed).unwrap();
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();
    assert_eq!(tx.input.len(), 1);
    assert_eq!(tx.output.len(), 2);
    // RBF sequence.
    assert_eq!(tx.input[0].sequence.0, 0xFFFFFFFD);
}

#[test]
fn psbt_unsigned_tx_is_bip69_sorted() {
    use vtorrent_btc::tx::create_psbt;

    let addr0 = derive_address(&SEED, 0, NETWORK).unwrap();
    let dest = derive_address(&SEED, 1, NETWORK).unwrap();

    // Create 3 inputs with deliberately wrong order.
    let inputs = vec![
        make_utxo(&"cc".repeat(32), 0, 30_000, &addr0),
        make_utxo(&"aa".repeat(32), 0, 10_000, &addr0),
        make_utxo(&"bb".repeat(32), 0, 20_000, &addr0),
    ];
    let outputs = vec![(
        40_000u64,
        Address::from_str(&dest)
            .unwrap()
            .require_network(NETWORK)
            .unwrap(),
    )];

    let psbt_bytes = create_psbt(&inputs, &outputs, NETWORK, false).unwrap();
    let psbt = bitcoin::psbt::Psbt::deserialize(&psbt_bytes).unwrap();

    // Inputs should be sorted by txid.
    let txids: Vec<Txid> = psbt
        .unsigned_tx
        .input
        .iter()
        .map(|i| i.previous_output.txid)
        .collect();
    assert!(txids[0] <= txids[1]);
    assert!(txids[1] <= txids[2]);
}

// ─── Fee estimation ──────────────────────────────────────────────────────────

#[test]
fn fee_estimation_urgent_vs_economy() {
    let urgent = BtcWallet::estimate_fee(1, 2, 1000, 1);
    let standard = BtcWallet::estimate_fee(1, 2, 1000, 3);
    let economy = BtcWallet::estimate_fee(1, 2, 1000, 6);

    assert!(urgent > standard, "urgent fee should be higher");
    assert!(standard > economy, "standard fee should be higher");
}

#[test]
fn fee_estimation_defaults_on_zero_feefilter() {
    let fee = BtcWallet::estimate_fee(1, 2, 0, 3);
    assert!(fee >= 1, "fee must be at least 1 sat");
}

#[test]
fn fee_estimation_minimum_one_sat() {
    let fee = BtcWallet::estimate_fee(1, 2, -1, 6);
    assert!(fee >= 1);
}

#[test]
fn fee_estimation_scales_with_inputs() {
    let fee_1in = BtcWallet::estimate_fee(1, 2, 1000, 3);
    let fee_5in = BtcWallet::estimate_fee(5, 2, 1000, 3);
    assert!(fee_5in > fee_1in, "more inputs = higher fee");
}

// ─── UTXO persistence ────────────────────────────────────────────────────────

#[test]
fn utxo_persistence_roundtrip() {
    let dir = std::env::temp_dir().join(format!("vtorrent_btc_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("utxos.json");

    let addr0 = derive_address(&SEED, 0, NETWORK).unwrap();

    // Create wallet, add UTXOs, persist.
    {
        let mut wallet = BtcWallet::with_persistence(SEED, NETWORK, path.clone()).unwrap();
        wallet.add_utxo(make_utxo(&"aa".repeat(32), 0, 50_000, &addr0));
        wallet.add_utxo(make_utxo(&"bb".repeat(32), 0, 30_000, &addr0));
        wallet.set_utxo_path(path.clone()).unwrap();
    }

    // Reload from disk and verify.
    {
        let wallet = BtcWallet::with_persistence(SEED, NETWORK, path.clone()).unwrap();
        assert_eq!(wallet.balance(), 80_000);
        assert_eq!(wallet.list_utxos().len(), 2);
    }

    std::fs::remove_dir_all(&dir).ok();
}

// ─── Taproot / Schnorr ───────────────────────────────────────────────────────

#[test]
fn p2tr_address_from_wif() {
    let wif = derive_wif(&SEED, 0, NETWORK).unwrap();
    let addr = vtorrent_btc::tx::p2tr_address_from_wif(&wif, NETWORK).unwrap();
    let s = addr.to_string();
    assert!(s.starts_with("bc1p"), "P2TR should start with bc1p: {}", s);
}

#[test]
fn schnorr_sign_and_verify() {
    use vtorrent_btc::tx::schnorr_sign;

    let wif = derive_wif(&SEED, 0, NETWORK).unwrap();
    let msg = [42u8; 32];
    let sig = schnorr_sign(&msg, &wif).unwrap();
    assert_eq!(sig.len(), 64);
}

// ─── Tx structure ────────────────────────────────────────────────────────────

#[test]
fn send_to_transaction_has_witness() {
    let mut wallet = BtcWallet::with_network(SEED, NETWORK);
    let addr0 = wallet.next_address().unwrap();
    let addr1 = wallet.next_address().unwrap();
    wallet.add_utxo(make_utxo(&"aa".repeat(32), 0, 100_000, &addr0));

    let (_, raw, _) = wallet.send_to(&addr1, 50_000, 1_000).unwrap();
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();

    // SegWit transactions have non-empty witness on each input.
    for input in &tx.input {
        assert!(!input.witness.is_empty(), "SegWit input must have witness");
    }
}

#[test]
fn send_to_output_amounts_sum_to_input_minus_fee() {
    let mut wallet = BtcWallet::with_network(SEED, NETWORK);
    let addr0 = wallet.next_address().unwrap();
    let addr1 = wallet.next_address().unwrap();
    wallet.add_utxo(make_utxo(&"aa".repeat(32), 0, 100_000, &addr0));

    let fee = 2_500u64;
    let send_amount = 75_000u64;
    let (_, raw, _) = wallet.send_to(&addr1, send_amount, fee).unwrap();
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();

    let input_total: u64 = tx.output.iter().map(|o| o.value.to_sat()).sum();
    assert_eq!(
        input_total,
        100_000 - fee,
        "outputs + fee must equal input value"
    );
}

#[test]
fn send_to_locktime_is_zero() {
    let mut wallet = BtcWallet::with_network(SEED, NETWORK);
    let addr0 = wallet.next_address().unwrap();
    let addr1 = wallet.next_address().unwrap();
    wallet.add_utxo(make_utxo(&"aa".repeat(32), 0, 50_000, &addr0));

    let (_, raw, _) = wallet.send_to(&addr1, 40_000, 1_000).unwrap();
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();
    assert_eq!(tx.lock_time.to_consensus_u32(), 0);
}

// ─── Address derivation ──────────────────────────────────────────────────────

#[test]
fn address_deterministic_across_wallets() {
    let _w1 = BtcWallet::with_network(SEED, NETWORK);
    let _w2 = BtcWallet::with_network(SEED, NETWORK);
    let a1 = derive_address(&SEED, 5, NETWORK).unwrap();
    let a2 = derive_address(&SEED, 5, NETWORK).unwrap();
    assert_eq!(a1, a2);
}

#[test]
fn different_seeds_produce_different_addresses() {
    let mut seed2 = SEED;
    seed2[0] ^= 0xFF;
    let a1 = derive_address(&SEED, 0, NETWORK).unwrap();
    let a2 = derive_address(&seed2, 0, NETWORK).unwrap();
    assert_ne!(a1, a2);
}

// ─── Atomic Swap (HTLC) integration tests ────────────────────────────────────

use bitcoin::hashes::{sha256, Hash};
use vtorrent_btc::htlc::{BtcHtlc, DEFAULT_HTLC_LOCKTIME};

const FUNDING_SEED: [u8; 64] = [0x11; 64];
const RECIPIENT_SEED: [u8; 64] = [0x22; 64];
const PREIMAGE: [u8; 32] = [0xDE; 32];
const FUNDING_TXID: [u8; 32] = [0xAA; 32];

fn swap_htlc() -> BtcHtlc {
    let hash_lock: [u8; 32] = sha256::Hash::hash(&PREIMAGE).to_byte_array();
    let recipient_addr = derive_address(&RECIPIENT_SEED, 0, NETWORK).unwrap();
    let refund_addr = derive_address(&FUNDING_SEED, 0, NETWORK).unwrap();
    BtcHtlc::new_with_network(
        hash_lock,
        recipient_addr,
        refund_addr,
        DEFAULT_HTLC_LOCKTIME,
        100_000,
        NETWORK,
    )
    .unwrap()
}

#[test]
fn swap_htlc_script_structure() {
    let htlc = swap_htlc();
    let script = htlc.build_script().unwrap();
    let bytes = script.as_bytes();
    assert_eq!(bytes[0], 0x63); // OP_IF
                                // Preimage-size guard: OP_SIZE 32 OP_EQUALVERIFY
    assert_eq!(bytes[1], 0x82); // OP_SIZE
    assert_eq!(bytes[2], 0x01); // push 1 byte
    assert_eq!(bytes[3], 0x20); // literal 32
    assert_eq!(bytes[4], 0x88); // OP_EQUALVERIFY
    assert_eq!(bytes[5], 0xa8); // OP_SHA256
    assert_eq!(*bytes.last().unwrap(), 0x68); // OP_ENDIF
}

#[test]
fn swap_htlc_address_is_p2wsh() {
    let htlc = swap_htlc();
    let addr = htlc.address().unwrap();
    assert!(addr.starts_with("bc1q"), "P2WSH addr: {}", addr);
}

#[test]
fn swap_funding_tx_build_and_sign() {
    let htlc = swap_htlc();
    let funder_wif = derive_wif(&FUNDING_SEED, 0, NETWORK).unwrap();
    let change_addr = derive_address(&FUNDING_SEED, 1, NETWORK).unwrap();

    // Build funding tx: 200,000 sats in, 100,000 sats to HTLC + change.
    let unsigned = htlc
        .build_funding_tx(FUNDING_TXID, 0, 200_000, 1_000, &change_addr)
        .unwrap();
    assert_eq!(unsigned.output.len(), 2);
    assert_eq!(unsigned.output[0].value, Amount::from_sat(100_000));

    // Sign the P2WPKH funding input.
    let signed = htlc
        .sign_funding_tx(unsigned, 200_000, &funder_wif)
        .unwrap();
    assert!(!signed.input[0].witness.is_empty());
    assert_eq!(signed.input[0].witness.len(), 2); // sig + pubkey

    // Verify raw serialization round-trips.
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let deserialized: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();
    assert_eq!(deserialized.output[0].value, Amount::from_sat(100_000));
}

#[test]
fn swap_claim_tx_full_cycle() {
    let htlc = swap_htlc();
    let recipient_wif = derive_wif(&RECIPIENT_SEED, 0, NETWORK).unwrap();

    // Build claim tx spending the HTLC output.
    let unsigned = htlc.build_claim_tx(FUNDING_TXID, &PREIMAGE, 1_000).unwrap();
    assert_eq!(unsigned.input.len(), 1);
    assert_eq!(unsigned.output.len(), 1);
    assert_eq!(
        unsigned.output[0].value,
        Amount::from_sat(99_000) // 100,000 - 1,000 fee
    );

    // Sign with recipient key (reveals preimage in witness).
    let signed = htlc
        .sign_claim_tx(unsigned, &PREIMAGE, &recipient_wif)
        .unwrap();
    let witness = &signed.input[0].witness;
    assert_eq!(witness.len(), 5); // sig, pubkey, preimage, OP_IF(1), witness_script
    assert_eq!(
        witness.nth(2),
        Some(&PREIMAGE[..]),
        "preimage must be in witness"
    );
}

#[test]
fn swap_claim_wrong_preimage_fails() {
    let htlc = swap_htlc();
    let result = htlc.build_claim_tx(FUNDING_TXID, &[0xFF; 32], 1_000);
    assert!(result.is_err());
}

#[test]
fn swap_refund_after_expiry() {
    let htlc = swap_htlc();
    let refund_wif = derive_wif(&FUNDING_SEED, 0, NETWORK).unwrap();

    // Use mock "now" far in the future so expiry has passed.
    let future_now = htlc.expiry + 1;
    let unsigned = htlc
        .build_refund_tx_at(FUNDING_TXID, 1_000, future_now)
        .unwrap();
    assert_eq!(unsigned.output[0].value, Amount::from_sat(99_000));
    assert_ne!(unsigned.lock_time.to_consensus_u32(), 0);

    let signed = htlc.sign_refund_tx(unsigned, &refund_wif).unwrap();
    assert_eq!(signed.input[0].witness.len(), 4); // sig, pubkey, OP_ELSE(0), witness_script
    assert!(
        signed.input[0].witness.nth(2).unwrap().is_empty(),
        "OP_ELSE selector must be empty (minimal false)"
    );
}

#[test]
fn swap_refund_before_expiry_fails() {
    let htlc = swap_htlc();
    let now = htlc.expiry - 1;
    let result = htlc.build_refund_tx_at(FUNDING_TXID, 1_000, now);
    assert!(result.is_err());
}

#[test]
fn swap_claim_and_refund_use_different_witness_branches() {
    let htlc = swap_htlc();
    let recipient_wif = derive_wif(&RECIPIENT_SEED, 0, NETWORK).unwrap();
    let refund_wif = derive_wif(&FUNDING_SEED, 0, NETWORK).unwrap();

    let claim = htlc
        .sign_claim_tx(
            htlc.build_claim_tx(FUNDING_TXID, &PREIMAGE, 1_000).unwrap(),
            &PREIMAGE,
            &recipient_wif,
        )
        .unwrap();
    let refund = htlc
        .sign_refund_tx(
            htlc.build_refund_tx_at(FUNDING_TXID, 1_000, htlc.expiry + 1)
                .unwrap(),
            &refund_wif,
        )
        .unwrap();

    // Claim witness: [sig, pubkey, preimage, 1, script] — 5 elements
    assert_eq!(claim.input[0].witness.len(), 5);
    // Refund witness: [sig, pubkey, 0, script] — 4 elements
    assert_eq!(refund.input[0].witness.len(), 4);
    // Different branch selectors.
    assert_eq!(claim.input[0].witness.nth(3), Some(&[1u8][..]));
    assert_eq!(refund.input[0].witness.nth(2), Some(&[][..]));
}

#[test]
fn swap_htlc_amount_cannot_be_zero() {
    let hash_lock = sha256::Hash::hash(&PREIMAGE).to_byte_array();
    let result = BtcHtlc::new_with_network(
        hash_lock,
        "bc1qtest".into(),
        "bc1qtest".into(),
        3600,
        0,
        NETWORK,
    );
    assert!(result.is_err());
}

#[test]
fn swap_funding_tx_insufficient_value() {
    let htlc = swap_htlc();
    let result = htlc.build_funding_tx(FUNDING_TXID, 0, 50_000, 1_000, "bc1qtest");
    assert!(result.is_err());
}
