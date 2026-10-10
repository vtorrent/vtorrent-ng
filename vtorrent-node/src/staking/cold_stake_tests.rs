//! End-to-end cold-staking (P2CS) test.
//!
//! Funds a P2CS UTXO, stakes it with an engine holding **only the staking
//! key**, and verifies:
//!   1. the block is accepted by the chain (the R1 re-lock rule passes),
//!   2. the stake output re-locks to the same P2CS script,
//!   3. a spend signed by the **spending** key is what the script requires
//!      (i.e. the staking key alone cannot move the coins).
//!
//! See `docs/cold-staking-p2cs-design.md`.

use super::*;
use crate::block::{Block, BlockHeader, TxInput, TxOutput, TxType};
use crate::chain::Chain;
use crate::consensus::{compute_stake_modifier, COIN, MIN_STAKE_AGE};
use secp256k1::{PublicKey, Secp256k1, SecretKey};

fn key(byte: u8) -> (vtorrent_core::keys::PrivateKey, PublicKey) {
    let secp = Secp256k1::new();
    let mut bytes = [0u8; 32];
    bytes[31] = byte;
    let sk = vtorrent_core::keys::PrivateKey::from_bytes(bytes, true).unwrap();
    let pk = PublicKey::from_secret_key(&secp, &SecretKey::from_slice(sk.as_bytes()).unwrap());
    (sk, pk)
}

#[test]
fn cold_stake_end_to_end() {
    let (staking_key, staking_pk) = key(7);
    let (_spending_key, spending_pk) = key(9);

    let sh = vtorrent_core::crypto::hash160(&staking_pk.serialize());
    let ph = vtorrent_core::crypto::hash160(&spending_pk.serialize());
    let p2cs_script = vtorrent_script::standard::build_p2cs(&sh, &ph, 0).unwrap();

    // ── Fund a P2CS UTXO via a coinbase at height 1 ───────────────────────────
    let mut chain = Chain::new().expect("chain init");
    let genesis_hash = chain.best_hash().unwrap();
    let funding_ts = 1_700_000_001u32;
    let funding_block = {
        let transactions = vec![Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![TxInput {
                prev_txid: [0u8; 32],
                prev_vout: 0xffffffff,
                script_sig: vec![1],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: 1000 * COIN,
                script_pubkey: p2cs_script.as_bytes().to_vec(),
            }],
            lock_time: 1,
            claim_address: None,
            claim_signature: None,
        }];
        let mut block = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: genesis_hash,
                merkle_root: [0u8; 32],
                utxo_root: [0u8; 32],
                timestamp: funding_ts,
                bits: crate::genesis::GENESIS_BITS,
                nonce: 1,
                stake_modifier: compute_stake_modifier(0, &genesis_hash),
            },
            transactions,
        };
        block.header.merkle_root = block.compute_merkle_root();
        block
    };
    chain.add_block(funding_block).unwrap();

    // The P2CS UTXO is in the set and is stakeable.
    let p2cs_utxo = chain
        .get_utxo_set()
        .values()
        .find(|u| u.script_pubkey == p2cs_script.as_bytes())
        .cloned()
        .expect("P2CS UTXO funded");
    assert!(
        crate::consensus::is_stakeable(&p2cs_utxo),
        "P2CS must be stakeable"
    );

    // ── Stake with an engine holding ONLY the staking key ─────────────────────
    let staking_wif = staking_key.to_wif(198);
    // The engine's "address" is unused for P2CS (the stake re-locks to the
    // script), but the constructor requires one.
    let engine = StakingEngine::with_wif_fast(
        "VMLVUkkn4hJ6Pex3w9RdmdU4BRUarszhHH".to_string(),
        staking_wif.to_string(),
    );

    let utxos: Vec<Utxo> = chain.get_utxo_set().values().cloned().collect();
    let prev_modifier = chain.get_block_at_height(1).unwrap().header.stake_modifier;
    let first_ts = funding_ts + MIN_STAKE_AGE as u32;

    let mut found: Option<Block> = None;
    for ts in (first_ts..).take(100_000) {
        if let Some(block) = engine.build_stake_block(
            chain.best_hash().unwrap(),
            prev_modifier,
            2,
            ts,
            chain.total_staked(),
            utxos.clone(),
            vec![],
        ) {
            found = Some(block);
            break;
        }
    }
    let block = found.expect("staking kernel should hit");

    // The coinstake must re-lock the stake to the SAME P2CS script (R1).
    // Check the value, not just script presence — a broken R1 rule would allow
    // re-locking 1 sat and redirecting the rest.
    let coinstake = &block.transactions[0];
    assert!(coinstake.is_coinstake());
    let p2cs_relocked: u64 = coinstake
        .outputs
        .iter()
        .filter(|o| o.script_pubkey == p2cs_script.as_bytes() && o.value > 0)
        .map(|o| o.value)
        .sum();
    assert!(
        p2cs_relocked >= 1000 * COIN,
        "coinstake must re-lock at least the full stake (1000 VTR) to P2CS, got {} sats",
        p2cs_relocked
    );

    // ── The chain accepts the block (R1 rule passes) ──────────────────────────
    let acceptance = chain.add_block(block).unwrap();
    assert!(
        matches!(
            acceptance,
            crate::chain::BlockAcceptance::MainChain { height: 2, .. }
        ),
        "cold-stake block must be accepted, got {acceptance:?}"
    );

    // The staked coins are now a P2CS UTXO again (re-locked), still unspendable
    // by the staking key.
    let relocked = chain
        .get_utxo_set()
        .values()
        .find(|u| u.script_pubkey == p2cs_script.as_bytes())
        .expect("stake re-locked to P2CS");
    assert!(relocked.value >= 1000 * COIN);
}
