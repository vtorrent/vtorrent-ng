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
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};

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

fn funded_p2cs() -> (
    Chain,
    StakingEngine,
    Vec<Utxo>,
    SecretKey,
    SecretKey,
    Vec<u8>,
) {
    let (staking_key, staking_pk) = key(7);
    let (spending_key, spending_pk) = key(9);
    let sh = vtorrent_core::crypto::hash160(&staking_pk.serialize());
    let ph = vtorrent_core::crypto::hash160(&spending_pk.serialize());
    let p2cs_script = vtorrent_script::standard::build_p2cs(&sh, &ph, 0).unwrap();
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
            outputs: vec![
                TxOutput {
                    value: 1000 * COIN,
                    script_pubkey: p2cs_script.as_bytes().to_vec(),
                };
                2
            ],
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
    let engine = StakingEngine::with_wif_fast(
        "VMLVUkkn4hJ6Pex3w9RdmdU4BRUarszhHH".to_string(),
        staking_key.to_wif(198).to_string(),
    );
    let utxos: Vec<Utxo> = chain.get_utxo_set().values().cloned().collect();
    let staking_sk = SecretKey::from_slice(staking_key.as_bytes()).unwrap();
    let spending_sk = SecretKey::from_slice(spending_key.as_bytes()).unwrap();
    (
        chain,
        engine,
        utxos,
        staking_sk,
        spending_sk,
        p2cs_script.as_bytes().to_vec(),
    )
}

fn stake_candidate(chain: &Chain, engine: &StakingEngine, utxos: &[Utxo]) -> Block {
    let prev = chain.get_block_at_height(1).unwrap();
    let first_ts = 1_700_000_001 + MIN_STAKE_AGE as u32;
    (first_ts..)
        .take(100_000)
        .find_map(|ts| {
            engine.build_stake_block(
                chain.best_hash().unwrap(),
                prev.header.stake_modifier,
                2,
                ts,
                chain.total_staked(),
                utxos.to_vec(),
                vec![],
            )
        })
        .expect("staking kernel should hit")
}

fn sign_p2cs(tx: &mut Transaction, script: &[u8], sk: &SecretKey, selector: u8) {
    let secp = Secp256k1::new();
    let pk = PublicKey::from_secret_key(&secp, sk);
    let mut sig = secp
        .sign_ecdsa(&Message::from_digest(tx.sighash(0, script)), sk)
        .serialize_der()
        .to_vec();
    sig.push(1);
    let mut bytes = vec![sig.len() as u8];
    bytes.extend(sig);
    bytes.push(33);
    bytes.extend(pk.serialize());
    bytes.push(selector);
    tx.inputs[0].script_sig = bytes;
}

fn recommit(chain: &Chain, block: &mut Block) {
    let mut post = chain.get_utxo_set().clone();
    for tx in &block.transactions {
        for input in &tx.inputs {
            post.remove(&(input.prev_txid, input.prev_vout));
        }
        for (i, output) in tx.outputs.iter().enumerate() {
            if crate::block::is_utxo_eligible(output, tx, 2) {
                post.insert(
                    (tx.txid(), i as u32),
                    Utxo {
                        txid: tx.txid(),
                        vout: i as u32,
                        value: output.value,
                        script_pubkey: output.script_pubkey.clone(),
                        height: 2,
                        timestamp: block.header.timestamp,
                    },
                );
            }
        }
    }
    block.header.merkle_root = block.compute_merkle_root();
    block.header.utxo_root =
        crate::block::compute_utxo_root_sorted(&post.values().cloned().collect::<Vec<_>>());
}

fn theft_tx(
    chain: &Chain,
    candidate: &Block,
    utxos: &[Utxo],
    script: &[u8],
    sk: &SecretKey,
    selector: u8,
) -> Transaction {
    let staked = &candidate.transactions[0].inputs[0];
    let victim = utxos
        .iter()
        .find(|u| {
            u.script_pubkey == script && (u.txid, u.vout) != (staked.prev_txid, staked.prev_vout)
        })
        .expect("second P2CS UTXO");
    let destination = vtorrent_script::standard::build_p2pkh(&[0x42; 20])
        .unwrap()
        .as_bytes()
        .to_vec();
    let mut theft = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: victim.txid,
            prev_vout: victim.vout,
            script_sig: vec![],
            sequence: if selector == 0x00 {
                0xfffffffe
            } else {
                0xffffffff
            },
        }],
        outputs: vec![TxOutput {
            value: victim.value - 10_000,
            script_pubkey: destination,
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    sign_p2cs(&mut theft, script, sk, selector);
    let _ = chain;
    theft
}

#[test]
fn hot_key_cannot_spend_p2cs_as_a_standard_transaction() {
    let (mut chain, engine, utxos, staking_sk, _spending_sk, script) = funded_p2cs();
    let mut candidate = stake_candidate(&chain, &engine, &utxos);
    candidate.transactions.push(theft_tx(
        &chain,
        &candidate,
        &utxos,
        &script,
        &staking_sk,
        0x51,
    ));
    recommit(&chain, &mut candidate);
    assert!(
        chain.add_block(candidate).is_err(),
        "the hot staking key must not spend delegated P2CS principal"
    );
}

#[test]
fn hot_key_p2cs_spend_is_rejected_by_mempool() {
    let (chain, engine, utxos, staking_sk, _spending_sk, script) = funded_p2cs();
    let candidate = stake_candidate(&chain, &engine, &utxos);
    let theft = theft_tx(&chain, &candidate, &utxos, &script, &staking_sk, 0x51);
    let mut mempool = crate::mempool::Mempool::new(100);
    let error = mempool
        .admit_with_chain_fee(&chain, theft)
        .expect_err("hot-key P2CS spend must not enter the mempool");
    assert!(matches!(
        error,
        crate::error::NodeError::PolicyRejected(_)
    ));
}

#[test]
fn cold_key_p2cs_spend_is_admitted_by_mempool() {
    let (chain, engine, utxos, _staking_sk, spending_sk, script) = funded_p2cs();
    let candidate = stake_candidate(&chain, &engine, &utxos);
    let spend = theft_tx(&chain, &candidate, &utxos, &script, &spending_sk, 0x00);
    let mut mempool = crate::mempool::Mempool::new(100);
    let fee = mempool
        .admit_with_chain_fee(&chain, spend)
        .expect("cold-key P2CS spend must enter the mempool");
    assert!(fee > 0);
}

#[test]
fn cold_key_can_spend_p2cs_as_a_standard_transaction() {
    let (mut chain, engine, utxos, _staking_sk, spending_sk, script) = funded_p2cs();
    let mut candidate = stake_candidate(&chain, &engine, &utxos);
    candidate.transactions.push(theft_tx(
        &chain,
        &candidate,
        &utxos,
        &script,
        &spending_sk,
        0x00,
    ));
    recommit(&chain, &mut candidate);
    assert!(
        chain.add_block(candidate).is_ok(),
        "the cold spending key must still be able to spend"
    );
}

#[test]
fn coinstake_cannot_burn_p2cs_principal() {
    let (mut chain, engine, utxos, staking_sk, _spending_sk, script) = funded_p2cs();
    let mut candidate = stake_candidate(&chain, &engine, &utxos);
    candidate.transactions[0].outputs[1].value = 1;
    sign_p2cs(&mut candidate.transactions[0], &script, &staking_sk, 0x51);
    recommit(&chain, &mut candidate);
    assert!(
        chain.add_block(candidate).is_err(),
        "a coinstake must not burn delegated P2CS principal"
    );
}

fn sign_p2sh(
    tx: &mut Transaction,
    utxo_script: &[u8],
    redeem: &[u8],
    sk: &SecretKey,
    selector: u8,
) {
    let secp = Secp256k1::new();
    let pk = PublicKey::from_secret_key(&secp, sk);
    let mut sig = secp
        .sign_ecdsa(&Message::from_digest(tx.sighash(0, utxo_script)), sk)
        .serialize_der()
        .to_vec();
    sig.push(1);
    let mut bytes = vec![sig.len() as u8];
    bytes.extend(sig);
    bytes.push(33);
    bytes.extend(pk.serialize());
    bytes.push(selector);
    bytes.push(redeem.len() as u8);
    bytes.extend(redeem);
    tx.inputs[0].script_sig = bytes;
}

#[test]
fn hot_key_cannot_spend_p2sh_wrapped_p2cs() {
    let (staking_key, staking_pk) = key(7);
    let (_spending_key, spending_pk) = key(9);
    let sh = vtorrent_core::crypto::hash160(&staking_pk.serialize());
    let ph = vtorrent_core::crypto::hash160(&spending_pk.serialize());
    let p2cs = vtorrent_script::standard::build_p2cs(&sh, &ph, 0).unwrap();
    let p2sh =
        vtorrent_script::standard::build_p2sh(&vtorrent_core::crypto::hash160(p2cs.as_bytes()))
            .unwrap();
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
            outputs: vec![
                TxOutput {
                    value: 1000 * COIN,
                    script_pubkey: p2cs.as_bytes().to_vec(),
                },
                TxOutput {
                    value: 1000 * COIN,
                    script_pubkey: p2sh.as_bytes().to_vec(),
                },
            ],
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
    let engine = StakingEngine::with_wif_fast(
        "VMLVUkkn4hJ6Pex3w9RdmdU4BRUarszhHH".to_string(),
        staking_key.to_wif(198).to_string(),
    );
    let utxos: Vec<Utxo> = chain.get_utxo_set().values().cloned().collect();
    let staking_sk = SecretKey::from_slice(staking_key.as_bytes()).unwrap();
    let mut candidate = stake_candidate(&chain, &engine, &utxos);
    let victim = utxos
        .iter()
        .find(|u| u.script_pubkey == p2sh.as_bytes())
        .expect("P2SH UTXO");
    let destination = vtorrent_script::standard::build_p2pkh(&[0x42; 20])
        .unwrap()
        .as_bytes()
        .to_vec();
    let mut theft = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: victim.txid,
            prev_vout: victim.vout,
            script_sig: vec![],
            sequence: 0xffffffff,
        }],
        outputs: vec![TxOutput {
            value: victim.value - 10_000,
            script_pubkey: destination,
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    sign_p2sh(
        &mut theft,
        p2sh.as_bytes(),
        p2cs.as_bytes(),
        &staking_sk,
        0x51,
    );
    candidate.transactions.push(theft);
    recommit(&chain, &mut candidate);
    assert!(
        chain.add_block(candidate).is_err(),
        "the hot staking key must not spend P2SH-wrapped P2CS principal"
    );
}
