//! Producer/chain UTXO-commitment parity tests.
//!
//! `build_stake_block_with_proof` commits to a post-apply UTXO root that the
//! chain recomputes in `add_block` (journal root). Because the header hash
//! includes `utxo_root`, the producer's root must match the journal root
//! exactly — otherwise the announced block hash diverges from the stored
//! canonical hash and peers can never fetch the block.

use super::*;
use crate::block::{TxInput, TxOutput, TxType};
use crate::chain::Chain;
use crate::consensus::{COIN, MIN_STAKE_AGE};

const FUNDING_TS: u32 = 1_700_000_001;

fn fund_chain(address: &str) -> Chain {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();
    let script_pubkey = vtorrent_core::address::validate_p2pkh(address)
        .map(|a| vtorrent_core::address::p2pkh_script_pubkey(&a.hash))
        .expect("valid address");
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
            value: 100 * COIN,
            script_pubkey,
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
            timestamp: FUNDING_TS,
            bits: crate::genesis::GENESIS_BITS,
            nonce: 1,
            stake_modifier: compute_stake_modifier(0, &genesis_hash),
        },
        transactions,
    };
    block.header.merkle_root = block.compute_merkle_root();
    chain.add_block(block).unwrap();
    chain
}

fn staker_keys(seed: u8) -> (String, String) {
    use secp256k1::{PublicKey, SecretKey};
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = seed;
    let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
    let wif = key.to_wif(198);
    let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
    let pubkey = PublicKey::from_secret_key(&SECP_CTX, &secret);
    let address = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70).to_string();
    (address, wif)
}

/// Append a PoW coinbase block paying `value` to `address`.
fn add_coinbase_block(chain: &mut Chain, address: &str, value: u64) {
    let script_pubkey = vtorrent_core::address::validate_p2pkh(address)
        .map(|a| vtorrent_core::address::p2pkh_script_pubkey(&a.hash))
        .expect("valid address");
    let height = chain.best_height() + 1;
    let parent_hash = chain.best_hash().unwrap();
    let parent_modifier = chain
        .get_block_at_height(chain.best_height())
        .unwrap()
        .header
        .stake_modifier;
    let transactions = vec![Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![TxInput {
            prev_txid: [0u8; 32],
            prev_vout: 0xffffffff,
            script_sig: vec![height as u8],
            sequence: 0xffffffff,
        }],
        outputs: vec![TxOutput {
            value,
            script_pubkey,
        }],
        lock_time: height,
        claim_address: None,
        claim_signature: None,
    }];
    let mut b = Block {
        header: BlockHeader {
            version: 1,
            prev_block_hash: parent_hash,
            merkle_root: [0u8; 32],
            utxo_root: [0u8; 32],
            timestamp: FUNDING_TS + height,
            bits: crate::genesis::GENESIS_BITS,
            nonce: height,
            stake_modifier: compute_stake_modifier(parent_modifier, &parent_hash),
        },
        transactions,
    };
    b.header.merkle_root = b.compute_merkle_root();
    chain.add_block(b).unwrap();
}

/// Snapshot of the chain's full UTXO set in deterministic key order.
fn all_utxos(chain: &Chain) -> Vec<Utxo> {
    let mut keys: Vec<([u8; 32], u32)> = chain.get_utxo_set().keys().copied().collect();
    keys.sort();
    keys.into_iter()
        .map(|k| chain.get_utxo_set()[&k].clone())
        .collect()
}

/// Build a signed P2PKH spend of `source` paying `outputs` (mempool-shaped).
fn signed_transfer(source: &Utxo, outputs: Vec<TxOutput>, wif: &str) -> Transaction {
    use secp256k1::{Message, PublicKey, SecretKey};
    let key = vtorrent_core::keys::PrivateKey::from_wif(wif).unwrap();
    let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
    let pubkey = PublicKey::from_secret_key(&SECP_CTX, &secret);

    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: source.txid,
            prev_vout: source.vout,
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        outputs,
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    let sighash = tx.sighash(0, &source.script_pubkey);
    let sig = SECP_CTX.sign_ecdsa(&Message::from_digest(sighash), &secret);
    let mut der = sig.serialize_der().to_vec();
    der.push(0x01);
    let mut script = Vec::with_capacity(1 + der.len() + 1 + 33);
    script.push(der.len() as u8);
    script.extend_from_slice(&der);
    script.push(33);
    script.extend_from_slice(&pubkey.serialize());
    tx.inputs[0].script_sig = script;
    tx
}

/// The producer's post-apply utxo_root must match what the chain journals on
/// add_block when pending mempool txs ride along: their inputs must be
/// removed and their outputs added (txid/vout/height/timestamp), in
/// transaction order after the coinstake. Otherwise the producer block's
/// hash diverges from the stored canonical hash and the announce breaks.
///
/// Pending txs spend dust outputs (below MIN_STAKE_AMOUNT) so they can never
/// win the kernel race, keeping the fixture deterministic.
#[test]
fn test_producer_root_includes_pending_tx_effects() {
    let (address, wif) = staker_keys(42);
    let mut chain = fund_chain(&address);

    // Two spendable dust UTXOs for the pending transfers (value < COIN, so
    // they are ineligible for staking and can never be the kernel UTXO).
    add_coinbase_block(&mut chain, &address, 500_000);
    add_coinbase_block(&mut chain, &address, 300_000);
    let dust: Vec<Utxo> = chain
        .get_utxos_for_address(&address)
        .into_iter()
        .filter(|u| u.value < COIN)
        .collect();
    assert_eq!(dust.len(), 2, "two dust UTXOs must exist");

    // Producer working set: the full UTXO set (what an honest producer
    // commits to — it must match the journal root exactly).
    let stake_utxos: Vec<Utxo> = all_utxos(&chain);
    assert!(stake_utxos.len() >= 3);

    // Pending tx 1: spends dust #1, pays 2 outputs.
    // Pending tx 2: spends dust #2, pays 1 output.
    let transfer = signed_transfer(
        &dust[0],
        vec![
            TxOutput {
                value: 200_000,
                script_pubkey: vec![0x51],
            },
            TxOutput {
                value: 100_000,
                script_pubkey: vec![0x52],
            },
        ],
        &wif,
    );
    let fresh = signed_transfer(
        &dust[1],
        vec![TxOutput {
            value: 150_000,
            script_pubkey: vec![0x53],
        }],
        &wif,
    );

    let prev_modifier = chain
        .get_block_at_height(chain.best_height())
        .unwrap()
        .header
        .stake_modifier;
    let engine = StakingEngine::with_wif(address, wif);
    let mut found: Option<(Block, StakeProof)> = None;
    for ts in (FUNDING_TS + MIN_STAKE_AGE as u32 + 1)..(FUNDING_TS + MIN_STAKE_AGE as u32 + 60_000)
    {
        if let Some(r) = engine.build_stake_block_with_proof(
            chain.best_hash().unwrap(),
            prev_modifier,
            chain.best_height() + 1,
            ts,
            stake_utxos.clone(),
            vec![transfer.clone(), fresh.clone()],
        ) {
            found = Some(r);
            break;
        }
    }
    let (block, _proof) = found.expect("kernel should hit");
    // The pending txs must ride in the block (coinstake + 2).
    assert_eq!(block.transactions.len(), 3);

    let acceptance = chain.add_block(block.clone()).unwrap();
    assert!(matches!(
        acceptance,
        crate::chain::BlockAcceptance::MainChain { height: 4, .. }
    ));

    let stored = chain.get_block_at_height(4).unwrap();
    assert_eq!(
        block.header.utxo_root, stored.header.utxo_root,
        "producer post-apply root must equal the chain journal root"
    );
    assert_eq!(
        block.hash(),
        stored.hash(),
        "producer block hash must equal the stored canonical hash"
    );
    assert!(
        stored
            .transactions
            .iter()
            .any(|tx| tx.txid() == transfer.txid()),
        "transfer tx confirmed"
    );
    assert!(
        stored
            .transactions
            .iter()
            .any(|tx| tx.txid() == fresh.txid()),
        "fresh tx confirmed"
    );
}

/// Regression for the no-pending-tx case: producer root and stored root must
/// agree even when the working set spans every spendable UTXO.
#[test]
fn test_producer_root_matches_full_set_without_pending() {
    let (address, wif) = staker_keys(43);
    let mut chain = fund_chain(&address);
    add_coinbase_block(&mut chain, &address, 500_000);
    let prev_modifier = chain
        .get_block_at_height(chain.best_height())
        .unwrap()
        .header
        .stake_modifier;
    let full_set: Vec<Utxo> = all_utxos(&chain);

    let engine = StakingEngine::with_wif(address, wif);
    let mut found: Option<(Block, StakeProof)> = None;
    for ts in (FUNDING_TS + MIN_STAKE_AGE as u32 + 1)..(FUNDING_TS + MIN_STAKE_AGE as u32 + 10_000)
    {
        if let Some(r) = engine.build_stake_block_with_proof(
            chain.best_hash().unwrap(),
            prev_modifier,
            chain.best_height() + 1,
            ts,
            full_set.clone(),
            vec![],
        ) {
            found = Some(r);
            break;
        }
    }
    let (block, _proof) = found.expect("kernel should hit");

    let mut tampered = block.clone();
    tampered.header.utxo_root = [0xaa; 32];
    let height_before = chain.best_height();
    let utxos_before = chain.get_utxo_set().clone();
    assert!(chain.add_block(tampered).is_err());
    assert_eq!(chain.best_height(), height_before);
    assert_eq!(chain.get_utxo_set(), &utxos_before);

    let acceptance = chain.add_block(block.clone()).unwrap();
    let height = match &acceptance {
        crate::chain::BlockAcceptance::MainChain { height, .. } => *height,
        other => panic!("expected MainChain, got {other:?}"),
    };
    let stored = chain.get_block_at_height(height).unwrap();
    // Which UTXO won the kernel?
    assert_eq!(block.header.utxo_root, stored.header.utxo_root);
    assert_eq!(block.hash(), stored.hash());
}

#[test]
fn test_borrowed_chain_state_staking_path_matches_journal() {
    let (address, wif) = staker_keys(44);
    let mut chain = fund_chain(&address);
    add_coinbase_block(&mut chain, &address, 500_000);
    let prev_modifier = chain
        .get_block_at_height(chain.best_height())
        .unwrap()
        .header
        .stake_modifier;
    let engine = StakingEngine::with_wif(address.clone(), wif);
    let wallet_utxos = chain.get_utxos_for_address(&address);
    let mut found = None;

    for timestamp in
        (FUNDING_TS + MIN_STAKE_AGE as u32 + 1)..(FUNDING_TS + MIN_STAKE_AGE as u32 + 10_000)
    {
        let Some(kernel) = engine.find_stake_kernel(
            prev_modifier,
            chain.best_height() + 1,
            timestamp,
            wallet_utxos.iter(),
        ) else {
            continue;
        };
        found = engine.build_from_kernel_with_proof(
            chain.best_hash().unwrap(),
            prev_modifier,
            chain.best_height() + 1,
            timestamp,
            chain.get_utxo_set(),
            vec![],
            kernel,
        );
        if found.is_some() {
            break;
        }
    }

    let (block, _proof) = found.expect("kernel should hit");
    let acceptance = chain.add_block(block.clone()).unwrap();
    let height = match acceptance {
        crate::chain::BlockAcceptance::MainChain { height, .. } => height,
        other => panic!("expected MainChain, got {other:?}"),
    };
    let stored = chain.get_block_at_height(height).unwrap();
    assert_eq!(block.header.utxo_root, stored.header.utxo_root);
    assert_eq!(block.hash(), stored.hash());
}
