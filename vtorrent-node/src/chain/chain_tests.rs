use super::*;
use crate::block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType};
use crate::consensus::{check_stake_kernel, compute_pos_reward};

fn make_block(prev_hash: [u8; 32], prev_stake_modifier: u64, height: u32) -> Block {
    let transactions = vec![Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![TxInput {
            prev_txid: [0u8; 32],
            prev_vout: 0xffffffff,
            script_sig: vec![height as u8], // unique per height
            sequence: 0xffffffff,
        }],
        outputs: vec![TxOutput {
            value: 1_000_000,
            script_pubkey: vec![
                0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0x88, 0xac,
            ],
        }],
        lock_time: height, // height is encoded in the first tx's lock_time
        claim_address: None,
        claim_signature: None,
    }];
    // Build a partial block to compute the merkle root
    let mut block = Block {
        header: BlockHeader {
            version: 1,
            prev_block_hash: prev_hash,
            merkle_root: [0u8; 32],
            utxo_root: [0u8; 32],
            timestamp: 1_700_000_000 + height,
            bits: crate::genesis::GENESIS_BITS,
            nonce: height, // PoW-style: non-zero nonce for a coinbase block
            stake_modifier: compute_stake_modifier(prev_stake_modifier, &prev_hash),
        },
        transactions,
    };
    block.header.merkle_root = block.compute_merkle_root();
    block
}

#[test]
fn test_chain_initialization() {
    let chain = Chain::new().expect("Chain init failed");
    assert_eq!(chain.best_height(), 0);
    assert!(chain.best_hash().is_some());
    // Genesis embeds the legacy snapshot (~11.59M VTR).
    assert_eq!(
        chain.total_supply(),
        crate::genesis::LEGACY_TOTAL_SUPPLY_SATOSHIS
    );
}

#[test]
fn test_total_supply_tracks_minted_value() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();
    let base = chain.total_supply();

    // A 1M-satoshi coinbase block mints value into the supply.
    let block = make_block(genesis_hash, 0, 1);
    chain.add_block(block).unwrap();
    assert_eq!(chain.total_supply(), base + 1_000_000);

    // Rolling the block back restores the supply.
    chain.rollback_one_block().unwrap();
    assert_eq!(chain.total_supply(), base);
}

#[test]
fn test_block_exceeding_max_supply_rejected() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();

    // A block minting 10M VTR would push total supply over the 20M cap
    // (genesis already embeds ~11.59M VTR).
    let mut block = make_block(genesis_hash, 0, 1);
    block.transactions[0].outputs[0].value = 10_000_000 * crate::consensus::COIN;
    block.header.merkle_root = block.compute_merkle_root();
    let result = chain.add_block(block);
    assert!(
        result.is_err(),
        "block exceeding MAX_SUPPLY must be rejected"
    );
    assert_eq!(chain.best_height(), 0);

    // A block minting a small amount is still accepted.
    let block = make_block(genesis_hash, 0, 1);
    chain.add_block(block).unwrap();
    assert_eq!(chain.best_height(), 1);
}

#[test]
fn test_rejected_block_does_not_corrupt_utxo_set() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();

    // Mint a spendable UTXO to a known address.
    let addr = "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT";
    chain
        .mint_to_address(addr, 100 * crate::consensus::COIN)
        .unwrap();
    let utxos_before = chain.get_utxos_for_address(addr);
    assert_eq!(utxos_before.len(), 1);
    let utxo = utxos_before[0].clone();

    // Build a block that spends the UTXO with an invalid scriptSig, so
    // script verification fails *after* the input is removed.
    let spend = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: utxo.txid,
            prev_vout: utxo.vout,
            script_sig: vec![0x00], // invalid: empty signature
            sequence: 0xffffffff,
        }],
        outputs: vec![TxOutput {
            value: utxo.value,
            script_pubkey: utxo.script_pubkey.clone(),
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    let mut block = make_block(genesis_hash, 0, 2);
    block.transactions.push(spend);
    block.header.merkle_root = block.compute_merkle_root();

    let result = chain.add_block(block);
    assert!(
        result.is_err(),
        "block with invalid scriptSig must be rejected"
    );

    // The UTXO must still be present and unspent.
    let utxos_after = chain.get_utxos_for_address(addr);
    assert_eq!(
        utxos_after.len(),
        1,
        "rejected block must not delete the UTXO"
    );
    assert_eq!(utxos_after[0].txid, utxo.txid);
}

#[test]
fn test_add_block_main_chain() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();
    let block = make_block(genesis_hash, 0, 1);
    let result = chain.add_block(block).unwrap();
    assert!(matches!(
        result,
        BlockAcceptance::MainChain { height: 1, .. }
    ));
    assert_eq!(chain.best_height(), 1);
}

#[test]
fn test_duplicate_block_ignored() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();
    let block = make_block(genesis_hash, 0, 1);
    chain.add_block(block.clone()).unwrap();
    let stored = chain.get_block_at_height(1).unwrap().clone();
    let result = chain.add_block(stored).unwrap();
    assert_eq!(result, BlockAcceptance::Duplicate);
}

#[test]
fn test_fork_block_stored() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();

    // Add block 1 to main chain
    let block1 = make_block(genesis_hash, 0, 1);
    chain.add_block(block1).unwrap();

    // Add a competing block 1 (fork)
    let mut fork_block = make_block(genesis_hash, 0, 1);
    fork_block.header.nonce = 999; // make it different
    let result = chain.add_block(fork_block).unwrap();
    assert!(matches!(result, BlockAcceptance::Fork { .. }));
    assert_eq!(chain.best_height(), 1); // main chain unchanged
}

#[test]
fn test_rollback_restores_utxo() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();
    let utxo_count_before = chain.utxo_set.len();

    let block = make_block(genesis_hash, 0, 1);
    chain.add_block(block).unwrap();
    assert!(chain.utxo_set.len() > utxo_count_before);

    // Roll back
    chain.rollback_one_block().unwrap();
    assert_eq!(chain.best_height(), 0);
    assert_eq!(chain.utxo_set.len(), utxo_count_before);
}

#[test]
fn test_transaction_index_tracks_main_chain_and_reorgs() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();
    let genesis_txid = chain.genesis_block().transactions[0].txid();
    assert_eq!(chain.get_transaction(&genesis_txid).unwrap().2, 0);

    // Main chain: genesis → A.
    let block_a = make_block(genesis_hash, 0, 1);
    let txid_a = block_a.transactions[0].txid();
    chain.add_block(block_a).unwrap();
    assert_eq!(chain.get_transaction(&txid_a).unwrap().2, 1);

    // Longer fork: genesis → B → C, with B using a distinct coinbase txid.
    let mut block_b = make_block(genesis_hash, 0, 1);
    block_b.header.nonce = 777;
    block_b.transactions[0].inputs[0].script_sig = vec![1, 42];
    block_b.header.merkle_root = block_b.compute_merkle_root();
    let txid_b = block_b.transactions[0].txid();
    let hash_b = block_b.hash();
    let b_modifier = block_b.header.stake_modifier;
    chain.add_block(block_b).unwrap();
    assert!(chain.get_transaction(&txid_b).is_none());

    let block_c = make_block(hash_b, b_modifier, 2);
    let txid_c = block_c.transactions[0].txid();
    chain.add_block(block_c).unwrap();

    assert!(chain.get_transaction(&txid_a).is_none());
    assert_eq!(chain.get_transaction(&txid_b).unwrap().2, 1);
    assert_eq!(chain.get_transaction(&txid_c).unwrap().2, 2);
}

#[test]
fn test_reorg_to_longer_fork() {
    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();

    // Main chain: genesis → A
    let block_a = make_block(genesis_hash, 0, 1);
    chain.add_block(block_a.clone()).unwrap();
    let tip_a = chain.best_hash().unwrap();

    // Fork: genesis → B (different nonce)
    let mut block_b = make_block(genesis_hash, 0, 1);
    block_b.header.nonce = 999;
    chain.add_block(block_b.clone()).unwrap();
    let hash_b = block_b.hash();

    // Fork extension: B → C (makes fork longer)
    let block_c = make_block(hash_b, block_b.header.stake_modifier, 2);
    let result = chain.add_block(block_c).unwrap();

    assert!(matches!(result, BlockAcceptance::Reorg { .. }));
    assert_eq!(chain.best_height(), 2);
    assert_ne!(chain.best_hash(), Some(tip_a));
}

#[test]
fn test_invalid_fork_reorg_restores_original_state() {
    let mut chain = Chain::new().unwrap();
    let genesis_hash = chain.best_hash().unwrap();
    let main = make_block(genesis_hash, 0, 1);
    chain.add_block(main).unwrap();
    let original_tip = chain.best_hash().unwrap();
    let original_supply = chain.total_supply();
    let original_utxos = chain.utxo_set.clone();

    let invalid_spend = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: [0x55; 32],
            prev_vout: 0,
            script_sig: vec![0x51],
            sequence: u32::MAX,
        }],
        outputs: vec![TxOutput {
            value: 1_000,
            script_pubkey: vec![0x51],
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    let mut fork = make_block(genesis_hash, 0, 1);
    fork.header.nonce = 991;
    fork.transactions.push(invalid_spend);
    fork.header.merkle_root = fork.compute_merkle_root();
    let fork_hash = fork.hash();
    let fork_modifier = fork.header.stake_modifier;
    assert!(matches!(
        chain.add_block(fork).unwrap(),
        BlockAcceptance::Fork { .. }
    ));

    let extension = make_block(fork_hash, fork_modifier, 2);
    assert!(chain.add_block(extension).is_err());
    assert_eq!(chain.best_hash(), Some(original_tip));
    assert_eq!(chain.best_height(), 1);
    assert_eq!(chain.total_supply(), original_supply);
    assert_eq!(chain.utxo_set, original_utxos);
}

#[test]
fn test_address_to_p2pkh_script() {
    let chain = Chain::new().expect("Chain init failed");
    let script = chain.address_to_p2pkh_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT");
    // Standard P2PKH: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
    assert_eq!(script.len(), 25);
    assert_eq!(&script[..3], &[0x76, 0xa9, 0x14]);
    assert_eq!(&script[23..], &[0x88, 0xac]);
}

#[test]
fn test_mint_to_address() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 9;
    let secret = SecretKey::from_slice(&key_bytes).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
    let address = addr.to_string();

    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_height = chain.best_height();

    let txid = chain
        .mint_to_address(&address, 100 * crate::consensus::COIN)
        .expect("mint should succeed");

    assert_eq!(chain.best_height(), genesis_height + 1);
    let utxos = chain.get_utxos_for_address(&address);
    assert_eq!(utxos.len(), 1);
    assert_eq!(utxos[0].value, 100 * crate::consensus::COIN);
    assert_eq!(utxos[0].txid, txid);
}

#[test]
fn test_mint_to_address_rejects_invalid() {
    let mut chain = Chain::new().expect("Chain init failed");
    assert!(chain.mint_to_address("not-an-address", 1000).is_err());
    assert!(chain
        .mint_to_address("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT", 0)
        .is_err());
}

/// Build a coinbase block paying to a specific P2PKH script.
fn make_coinbase_to_script(
    prev_hash: [u8; 32],
    prev_stake_modifier: u64,
    height: u32,
    timestamp: u32,
    script_pubkey: Vec<u8>,
    value: u64,
) -> Block {
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
    let mut block = Block {
        header: BlockHeader {
            version: 1,
            prev_block_hash: prev_hash,
            merkle_root: [0u8; 32],
            utxo_root: [0u8; 32],
            timestamp,
            bits: crate::genesis::GENESIS_BITS,
            nonce: height,
            stake_modifier: compute_stake_modifier(prev_stake_modifier, &prev_hash),
        },
        transactions,
    };
    block.header.merkle_root = block.compute_merkle_root();
    block
}

#[test]
fn test_pos_block_with_signed_coinstake_accepted() {
    use crate::staking::StakingEngine;
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    // Generate a staking key pair.
    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 42;
    let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
    let wif = key.to_wif(198);
    let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
    let address = addr.to_string();

    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();

    // Fund the staking address with a large coinbase UTXO at an old
    // timestamp so the coin is mature (age >= MIN_STAKE_AGE).
    let funding_ts = 1_700_000_001u32;
    let script = chain.address_to_p2pkh_script(&address);
    let funding_block = make_coinbase_to_script(
        genesis_hash,
        0,
        1,
        funding_ts,
        script.clone(),
        100 * crate::consensus::COIN,
    );
    chain.add_block(funding_block).unwrap();
    assert_eq!(chain.best_height(), 1);

    assert!(!chain.get_utxos_for_address(&address).is_empty());
    let utxos = chain.get_utxo_set().values().cloned().collect::<Vec<_>>();

    // Search for a timestamp whose kernel hash satisfies the target.
    let engine = StakingEngine::with_wif(address.clone(), wif);
    let prev_stake_modifier = chain
        .get_block_at_height(1)
        .map(|b| b.header.stake_modifier)
        .unwrap_or(0);
    let mut stake_block = None;
    let first_ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
    for ts in (first_ts..).take(10_000) {
        if let Some(block) = engine.build_stake_block(
            chain.best_hash().unwrap(),
            prev_stake_modifier,
            2,
            ts,
            utxos.clone(),
            vec![],
        ) {
            stake_block = Some(block);
            break;
        }
    }
    let stake_block = stake_block.expect("should find a valid stake kernel");

    // The coinstake input must be signed (scriptSig is a real P2PKH sig).
    let coinstake = &stake_block.transactions[0];
    assert_eq!(coinstake.tx_type, TxType::Coinstake);
    assert!(
        coinstake.inputs[0].script_sig.len() > 2,
        "coinstake input must carry a signature"
    );

    // The chain must accept the block: kernel check + script verification.
    let result = chain.add_block(stake_block).unwrap();
    assert!(matches!(
        result,
        BlockAcceptance::MainChain { height: 2, .. }
    ));
    assert_eq!(chain.best_height(), 2);
}

#[test]
fn test_fast_regtest_chain_accepts_fast_stake_age() {
    use crate::staking::StakingEngine;
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 43;
    let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
    let wif = key.to_wif(198);
    let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let address = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70).to_string();

    let mut fast_chain = Chain::new_regtest_fast().unwrap();
    let mut normal_chain = Chain::new_regtest().unwrap();
    let funding_ts = 1_700_000_001u32;
    let funding_block = make_coinbase_to_script(
        fast_chain.best_hash().unwrap(),
        0,
        1,
        funding_ts,
        fast_chain.address_to_p2pkh_script(&address),
        500 * crate::consensus::COIN,
    );
    fast_chain.add_block(funding_block.clone()).unwrap();
    normal_chain.add_block(funding_block).unwrap();

    let engine = StakingEngine::with_wif_fast(address, wif);
    let utxos = fast_chain
        .get_utxo_set()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let prev_modifier = fast_chain
        .get_block_at_height(1)
        .unwrap()
        .header
        .stake_modifier;
    let stake_block = (funding_ts + crate::consensus::REGTEST_FAST_MIN_STAKE_AGE as u32
        ..funding_ts + crate::consensus::MIN_STAKE_AGE as u32)
        .find_map(|timestamp| {
            engine.build_stake_block(
                fast_chain.best_hash().unwrap(),
                prev_modifier,
                2,
                timestamp,
                utxos.clone(),
                vec![],
            )
        })
        .expect("fast regtest should find a kernel before mainnet maturity");

    let error = normal_chain.add_block(stake_block.clone()).unwrap_err();
    assert!(error.to_string().contains("Stake age"));
    assert!(matches!(
        fast_chain.add_block(stake_block).unwrap(),
        BlockAcceptance::MainChain { height: 2, .. }
    ));
}

#[test]
fn test_pos_block_with_bad_kernel_rejected() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 7;
    let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
    let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
    let address = addr.to_string();

    let mut chain = Chain::new().expect("Chain init failed");
    let genesis_hash = chain.best_hash().unwrap();
    let funding_ts = 1_700_000_001u32;
    let script = chain.address_to_p2pkh_script(&address);
    let funding_block = make_coinbase_to_script(
        genesis_hash,
        0,
        1,
        funding_ts,
        script.clone(),
        100 * crate::consensus::COIN,
    );
    chain.add_block(funding_block).unwrap();
    let utxos = chain.get_utxos_for_address(&address);

    // Build a coinstake whose kernel does NOT satisfy the target by
    // forging it directly (bypassing the engine's kernel search).
    let utxo = utxos[0].clone();
    let prev_stake_modifier = chain
        .get_block_at_height(1)
        .map(|b| b.header.stake_modifier)
        .unwrap_or(0);
    let mut bad_ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
    while check_stake_kernel(prev_stake_modifier, &utxo, bad_ts) {
        bad_ts += 1;
    }
    let reward = compute_pos_reward(utxo.value, (bad_ts - utxo.timestamp) as u64);
    let coinstake = Transaction {
        version: 1,
        tx_type: TxType::Coinstake,
        inputs: vec![TxInput {
            prev_txid: utxo.txid,
            prev_vout: utxo.vout,
            script_sig: vec![0x51], // OP_TRUE — no real signature
            sequence: u32::MAX,
        }],
        outputs: vec![
            TxOutput {
                value: 0,
                script_pubkey: Vec::new(),
            },
            TxOutput {
                value: utxo.value + reward,
                script_pubkey: script.clone(),
            },
        ],
        lock_time: 2,
        claim_address: None,
        claim_signature: None,
    };

    let parent_hash = chain.best_hash().unwrap();
    let mut block = Block {
        header: BlockHeader {
            version: 2,
            prev_block_hash: parent_hash,
            merkle_root: [0u8; 32],
            utxo_root: [0u8; 32],
            timestamp: bad_ts,
            bits: crate::genesis::GENESIS_BITS,
            nonce: 0, // PoS block
            stake_modifier: compute_stake_modifier(prev_stake_modifier, &parent_hash),
        },
        transactions: vec![coinstake],
    };
    block.header.merkle_root = block.compute_merkle_root();

    // The block must be rejected: the kernel check fails.
    let result = chain.add_block(block);
    assert!(result.is_err(), "block with bad kernel must be rejected");
    assert_eq!(chain.best_height(), 1);
}

#[test]
fn test_legacy_claim_does_not_double_count_supply() {
    // A user legacy claim (claim_address = Some) is funded by the snapshot
    // already counted in genesis, so it must not add to the supply.
    let claim_tx = Transaction {
        version: 1,
        tx_type: TxType::LegacyClaim,
        inputs: vec![],
        outputs: vec![TxOutput {
            value: 500 * crate::consensus::COIN,
            script_pubkey: vec![0x76, 0xa9, 0x14],
        }],
        lock_time: 1,
        claim_address: Some("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string()),
        claim_signature: Some(vec![0u8; 65]),
    };
    assert_eq!(
        compute_supply_delta(&claim_tx, 0, claim_tx.total_output()),
        0
    );

    // The genesis distribution tx (claim_address = None) establishes the
    // initial supply and must still count.
    let genesis_dist = Transaction {
        claim_address: None,
        ..claim_tx.clone()
    };
    assert_eq!(
        compute_supply_delta(&genesis_dist, 0, genesis_dist.total_output()),
        genesis_dist.total_output()
    );

    // A coinbase mints its reward into the supply.
    let coinbase = Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![],
        outputs: vec![TxOutput {
            value: 1_000_000,
            script_pubkey: vec![0x76, 0xa9, 0x14],
        }],
        lock_time: 1,
        claim_address: None,
        claim_signature: None,
    };
    assert_eq!(
        compute_supply_delta(&coinbase, 0, coinbase.total_output()),
        1_000_000
    );
}

/// Full end-to-end test: coinbase → P2PKH spend → chain acceptance.
///
/// Exercises the complete pipeline:
///   keypair generation → address derivation → scriptPubKey construction
///   → coinbase minting → manual signing → sighash →
///   script engine verification → UTXO set update.
#[test]
fn test_p2pkh_spend_through_chain() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    // 1. Generate a keypair and derive the vTorrent address.
    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(&[0xAB; 32]).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
    let address = addr.to_string();

    // 2. Mint a coinbase UTXO to the address.
    let mut chain = Chain::new().expect("Chain init failed");
    chain
        .mint_to_address(&address, 10 * crate::consensus::COIN)
        .unwrap();
    let funded_hash = chain.best_hash().unwrap();
    assert_eq!(
        chain.best_height(),
        1,
        "mint must advance chain to height 1"
    );
    let utxos = chain.get_utxos_for_address(&address);
    assert_eq!(utxos.len(), 1, "must have exactly one funded UTXO");
    let utxo = &utxos[0];
    let spend_value = 5 * crate::consensus::COIN;

    // 3. Build the recipient address.
    let (_, recipient_addr) = {
        let s = Secp256k1::new();
        let sk = SecretKey::from_slice(&[0xCD; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&s, &sk);
        let a = vtorrent_core::address::Address::from_pubkey(&pk, true, 70);
        (sk, a.to_string())
    };

    let script_pubkey = chain.address_to_p2pkh_script(&address);

    // 4. Build the spending transaction manually.
    //    Height is encoded in the first tx's lock_time (chain is at height 1 after mint).
    let spend_height = 2u32;
    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: utxo.txid,
            prev_vout: utxo.vout,
            script_sig: Vec::new(), // filled below
            sequence: 0xffff_fffe,
        }],
        outputs: vec![TxOutput {
            value: spend_value,
            script_pubkey: chain.address_to_p2pkh_script(&recipient_addr),
        }],
        lock_time: spend_height,
        claim_address: None,
        claim_signature: None,
    };

    // 5. Sign the input over the UTXO's scriptPubKey.
    let sighash = tx.sighash(0, &script_pubkey);
    let msg = secp256k1::Message::from_digest(sighash);
    let sig = secp.sign_ecdsa(&msg, &secret);
    let mut der = sig.serialize_der().to_vec();
    der.push(0x01); // SIGHASH_ALL
    let pubkey_bytes = pubkey.serialize();

    // Build scriptSig: <len><sig><len><pubkey>
    let mut script_sig = Vec::with_capacity(1 + der.len() + 1 + pubkey_bytes.len());
    script_sig.push(der.len() as u8);
    script_sig.extend_from_slice(&der);
    script_sig.push(pubkey_bytes.len() as u8);
    script_sig.extend_from_slice(&pubkey_bytes);
    tx.inputs[0].script_sig = script_sig;

    // 6. Verify the scriptSig through the script engine directly.
    let env = vtorrent_script::ScriptEnv {
        tx_hash: tx.sighash(0, &script_pubkey),
        block_height: 2,
        block_time: 1_700_000_002,
        tx_lock_time: tx.lock_time,
        input_sequence: 0xffff_fffe,
        utxo_height: 0,
        utxo_timestamp: 0,
    };
    let mut engine = vtorrent_script::Engine::new(env);
    let sig_script = vtorrent_script::Script::from_bytes(tx.inputs[0].script_sig.clone()).unwrap();
    let pk_script = vtorrent_script::Script::from_bytes(script_pubkey.clone()).unwrap();
    engine.execute(&sig_script, &pk_script).unwrap();

    // 7. Wrap in a block and add to the chain — exercises full validation.
    //    Every block must start with a coinbase transaction.
    let coinbase = Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![TxInput {
            prev_txid: [0u8; 32],
            prev_vout: 0xffffffff,
            script_sig: vec![2u8], // height = 2
            sequence: 0xffffffff,
        }],
        outputs: vec![TxOutput {
            value: crate::consensus::COIN, // block reward
            script_pubkey: vec![
                0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0x88, 0xac,
            ],
        }],
        lock_time: spend_height,
        claim_address: None,
        claim_signature: None,
    };

    let funded_stake_modifier = chain
        .get_block_at_height(1)
        .map(|b| b.header.stake_modifier)
        .unwrap_or(0);
    let mut block = Block {
        header: BlockHeader {
            version: 1,
            prev_block_hash: funded_hash,
            merkle_root: [0u8; 32],
            utxo_root: [0u8; 32],
            timestamp: now_timestamp_u32() + 1,
            bits: crate::genesis::GENESIS_BITS,
            nonce: 42,
            stake_modifier: compute_stake_modifier(funded_stake_modifier, &funded_hash),
        },
        transactions: vec![coinbase, tx],
    };
    block.header.merkle_root = block.compute_merkle_root();

    let acceptance = chain.add_block(block).unwrap();
    assert!(
        matches!(acceptance, super::BlockAcceptance::MainChain { .. }),
        "P2PKH spend block must be accepted on main chain"
    );

    // 8. Verify UTXO set: old UTXO consumed, new UTXO created for recipient.
    let sender_utxos = chain.get_utxos_for_address(&address);
    assert_eq!(sender_utxos.len(), 0, "spent UTXO must be consumed");

    let recipient_utxos = chain.get_utxos_for_address(&recipient_addr);
    assert_eq!(recipient_utxos.len(), 1, "recipient must have one UTXO");
    assert_eq!(recipient_utxos[0].value, spend_value);
}

/// Multi-block staking: produce 3 consecutive PoS blocks and verify
/// chain state, UTXO set, and staking reward accumulation.
#[test]
fn test_multi_block_staking() {
    use crate::staking::StakingEngine;
    use secp256k1::{PublicKey, Secp256k1};

    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 99;
    let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
    let wif = key.to_wif(198);
    let secret = secp256k1::SecretKey::from_slice(key.as_bytes()).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
    let address = addr.to_string();

    let mut chain = Chain::new().unwrap();
    let genesis_hash = chain.best_hash().unwrap();

    // Fund the staking address with a coinbase at an old timestamp.
    let funding_ts = 1_700_000_001u32;
    let script = chain.address_to_p2pkh_script(&address);
    let funding_block = make_coinbase_to_script(
        genesis_hash,
        0,
        1,
        funding_ts,
        script,
        100 * crate::consensus::COIN,
    );
    chain.add_block(funding_block).unwrap();

    let engine = StakingEngine::with_wif(address.clone(), wif);
    let mut prev_modifier = chain.get_block_at_height(1).unwrap().header.stake_modifier;

    for expected_height in 2..=4 {
        assert!(
            !chain.get_utxos_for_address(&address).is_empty(),
            "must have UTXOs at height {}",
            expected_height
        );
        let utxos = chain.get_utxo_set().values().cloned().collect::<Vec<_>>();

        let mut stake_block = None;
        let first_ts = chain
            .get_block_at_height(expected_height - 1)
            .unwrap()
            .header
            .timestamp
            + crate::consensus::MIN_STAKE_AGE as u32;
        for ts in (first_ts..).take(100_000) {
            if let Some(block) = engine.build_stake_block(
                chain.best_hash().unwrap(),
                prev_modifier,
                expected_height,
                ts,
                utxos.clone(),
                vec![],
            ) {
                stake_block = Some(block);
                break;
            }
        }
        let block = stake_block.expect("should find stake kernel");
        prev_modifier = block.header.stake_modifier;
        let result = chain.add_block(block).unwrap();
        assert!(
            matches!(result, super::BlockAcceptance::MainChain { height, .. } if height == expected_height)
        );
    }

    assert_eq!(chain.best_height(), 4);
    // Staking address should still have UTXOs (stake return + rewards).
    let final_utxos = chain.get_utxos_for_address(&address);
    assert!(!final_utxos.is_empty());
}

/// PoS block with mempool transactions: verify pending txs are included
/// in the block assembled by the staking engine.
#[test]
fn test_pos_block_includes_mempool_txs() {
    use crate::staking::StakingEngine;
    use secp256k1::{PublicKey, Secp256k1};

    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 55;
    let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
    let wif = key.to_wif(198);
    let secret = secp256k1::SecretKey::from_slice(key.as_bytes()).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
    let address = addr.to_string();

    let mut chain = Chain::new().unwrap();
    let genesis_hash = chain.best_hash().unwrap();

    let funding_ts = 1_700_000_001u32;
    let script = chain.address_to_p2pkh_script(&address);
    let funding_block = make_coinbase_to_script(
        genesis_hash,
        0,
        1,
        funding_ts,
        script,
        100 * crate::consensus::COIN,
    );
    chain.add_block(funding_block).unwrap();
    assert_eq!(chain.best_height(), 1);

    let utxos = chain.get_utxos_for_address(&address);
    let engine = StakingEngine::with_wif(address.clone(), wif);
    let prev_modifier = chain.get_block_at_height(1).unwrap().header.stake_modifier;

    // Create a "mempool" transaction (a dummy tx that the engine should include).
    let dummy_tx = crate::block::Transaction {
        version: 1,
        tx_type: crate::block::TxType::Standard,
        inputs: vec![crate::block::TxInput {
            prev_txid: [0xff; 32],
            prev_vout: 0,
            script_sig: vec![],
            sequence: 0,
        }],
        outputs: vec![crate::block::TxOutput {
            value: 5000,
            script_pubkey: {
                let mut s = vec![0x76, 0xa9, 0x14];
                s.extend([0xaa; 20]);
                s
            },
        }],
        lock_time: 2,
        claim_address: None,
        claim_signature: None,
    };

    let mut stake_block = None;
    let first_ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
    for ts in (first_ts..).take(100_000) {
        if let Some(block) = engine.build_stake_block(
            chain.best_hash().unwrap(),
            prev_modifier,
            2,
            ts,
            utxos.clone(),
            vec![dummy_tx.clone()],
        ) {
            stake_block = Some(block);
            break;
        }
    }
    let block = stake_block.expect("should find stake kernel");
    // Block should have the coinstake + the mempool tx.
    assert!(
        block.transactions.len() >= 2,
        "block should include mempool txs, got {}",
        block.transactions.len()
    );
    // Verify the mempool tx is in the block.
    let has_mempool = block.transactions.iter().any(|tx| {
        tx.tx_type == crate::block::TxType::Standard && tx.outputs.iter().any(|o| o.value == 5000)
    });
    assert!(has_mempool, "mempool tx with 5000 sats must be in block");

    // Verify the coinstake is valid (first tx in block).
    assert_eq!(
        block.transactions[0].tx_type,
        crate::block::TxType::Coinstake
    );
}

/// A transaction with an invalid signature must be rejected by
/// verify_tx_scripts — the mempool admission gate that prevents
/// script-invalid txs from poisoning stakers' block templates.
#[test]
fn test_verify_tx_scripts_rejects_bad_signature() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 11;
    let secret = SecretKey::from_slice(&key_bytes).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);

    let mut chain = Chain::new().expect("Chain init failed");
    let funding_txid = chain
        .mint_to_address(&addr.to_string(), 100_000)
        .expect("mint should succeed");
    let funding_script = chain
        .get_utxo(&funding_txid, 0)
        .unwrap()
        .script_pubkey
        .clone();
    let height = chain.best_height();
    let timestamp = chain
        .get_block_at_height(height)
        .map(|b| b.header.timestamp)
        .unwrap_or(0);

    // Spend with a signature from the WRONG key.
    let mut wrong_key = [0u8; 32];
    wrong_key[31] = 12;
    let wrong_secret = SecretKey::from_slice(&wrong_key).unwrap();
    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: funding_txid,
            prev_vout: 0,
            script_sig: vec![],
            sequence: 0xffff_ffff,
        }],
        outputs: vec![TxOutput {
            value: 90_000,
            script_pubkey: vec![0x51],
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    let sighash = tx.sighash(0, &funding_script);
    let msg = secp256k1::Message::from_digest(sighash);
    let sig = secp.sign_ecdsa(&msg, &wrong_secret);
    let mut der = sig.serialize_der().to_vec();
    der.push(0x01); // SIGHASH_ALL
    let mut script_sig = Vec::new();
    script_sig.push(der.len() as u8);
    script_sig.extend_from_slice(&der);
    script_sig.push(pubkey.serialize().len() as u8);
    script_sig.extend_from_slice(&pubkey.serialize());
    tx.inputs[0].script_sig = script_sig;

    assert!(
        chain.verify_tx_scripts(&tx, height, timestamp).is_err(),
        "bad signature must fail script verification"
    );
}

/// A valid P2PKH spend passes verify_tx_scripts.
#[test]
fn test_verify_tx_scripts_accepts_valid_signature() {
    use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 12;
    let secret = SecretKey::from_slice(&key_bytes).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);

    let mut chain = Chain::new().expect("Chain init failed");
    let funding_txid = chain
        .mint_to_address(&addr.to_string(), 100_000)
        .expect("mint should succeed");
    let funding_script = chain
        .get_utxo(&funding_txid, 0)
        .unwrap()
        .script_pubkey
        .clone();
    let height = chain.best_height();
    let timestamp = chain
        .get_block_at_height(height)
        .map(|b| b.header.timestamp)
        .unwrap_or(0);

    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: funding_txid,
            prev_vout: 0,
            script_sig: vec![],
            sequence: 0xffff_ffff,
        }],
        outputs: vec![TxOutput {
            value: 90_000,
            script_pubkey: vec![0x51],
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    let sighash = tx.sighash(0, &funding_script);
    let msg = Message::from_digest(sighash);
    let sig = secp.sign_ecdsa(&msg, &secret);
    let mut der = sig.serialize_der().to_vec();
    der.push(0x01);
    let mut script_sig = Vec::new();
    script_sig.push(der.len() as u8);
    script_sig.extend_from_slice(&der);
    script_sig.push(pubkey.serialize().len() as u8);
    script_sig.extend_from_slice(&pubkey.serialize());
    tx.inputs[0].script_sig = script_sig;

    chain
        .verify_tx_scripts(&tx, height, timestamp)
        .expect("valid P2PKH spend must pass script verification");
}

// ─── Reorg deep-coverage tests (chain_reorg.rs paths) ────────────────────────

use secp256k1::{PublicKey, Secp256k1, SecretKey};

fn addr_from_seed(seed: u8) -> String {
    let secp = Secp256k1::new();
    let mut sk_bytes = [0u8; 32];
    sk_bytes[31] = seed;
    let sk = SecretKey::from_slice(&sk_bytes).unwrap();
    let pk = PublicKey::from_secret_key(&secp, &sk);
    vtorrent_core::address::Address::from_pubkey(&pk, true, 70).to_string()
}

fn chain_addr_script(address: &str) -> Vec<u8> {
    let addr = vtorrent_core::address::validate_p2pkh(address).unwrap();
    vtorrent_core::address::p2pkh_script_pubkey(&addr.hash)
}

fn signed_transfer(
    utxo: &crate::chain::Utxo,
    recipient: &str,
    value: u64,
    secp: &Secp256k1<secp256k1::All>,
    secret: &SecretKey,
    pubkey: &PublicKey,
) -> Transaction {
    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: utxo.txid,
            prev_vout: utxo.vout,
            script_sig: Vec::new(),
            sequence: 0xffff_fffe,
        }],
        outputs: vec![TxOutput {
            value,
            script_pubkey: chain_addr_script(recipient),
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    let sighash = tx.sighash(0, &utxo.script_pubkey);
    let msg = secp256k1::Message::from_digest(sighash);
    let sig = secp.sign_ecdsa(&msg, secret);
    let mut der = sig.serialize_der().to_vec();
    der.push(0x01);
    let pk_bytes = pubkey.serialize();
    let mut script_sig = Vec::with_capacity(2 + der.len() + pk_bytes.len());
    script_sig.push(der.len() as u8);
    script_sig.extend_from_slice(&der);
    script_sig.push(pk_bytes.len() as u8);
    script_sig.extend_from_slice(&pk_bytes);
    tx.inputs[0].script_sig = script_sig;
    tx
}

/// Build a block containing the given transactions on top of `prev_hash`.
/// The coinbase is prepended automatically; the caller may override the nonce
/// afterwards (recomputing merkle root is the caller's job).
fn block_with_txs_on(
    prev_modifier: u64,
    prev_hash: [u8; 32],
    height: u32,
    timestamp: u32,
    mut txs: Vec<Transaction>,
) -> Block {
    let coinbase = Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![TxInput {
            prev_txid: [0u8; 32],
            prev_vout: 0xffffffff,
            script_sig: vec![height as u8],
            sequence: 0xffffffff,
        }],
        outputs: vec![TxOutput {
            value: crate::consensus::COIN,
            script_pubkey: vec![
                0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0x88, 0xac,
            ],
        }],
        lock_time: height,
        claim_address: None,
        claim_signature: None,
    };
    txs.insert(0, coinbase);
    let mut block = Block {
        header: BlockHeader {
            version: 1,
            prev_block_hash: prev_hash,
            merkle_root: [0u8; 32],
            utxo_root: [0u8; 32],
            timestamp,
            bits: crate::genesis::GENESIS_BITS,
            nonce: height,
            stake_modifier: compute_stake_modifier(prev_modifier, &prev_hash),
        },
        transactions: txs,
    };
    block.header.merkle_root = block.compute_merkle_root();
    block
}

/// Two competing forks spending the same UTXO: the longer fork must win the
/// reorg, and the UTXO set must reflect the winning fork's spend.
#[test]
fn test_reorg_conflicting_spends_longer_fork_wins() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(&[0xAB; 32]).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let sender = {
        let pk = PublicKey::from_secret_key(&secp, &secret);
        vtorrent_core::address::Address::from_pubkey(&pk, true, 70).to_string()
    };
    let recipient_main = addr_from_seed(0xCD);
    let recipient_fork = addr_from_seed(0xEF);

    let mut chain = Chain::new().unwrap();
    chain
        .mint_to_address(&sender, 10 * crate::consensus::COIN)
        .unwrap();
    let funded_hash = chain.best_hash().unwrap();
    let funded_modifier = chain.get_block_at_height(1).unwrap().header.stake_modifier;
    let funded_ts = chain.get_block_at_height(1).unwrap().header.timestamp;
    let utxo = chain.get_utxos_for_address(&sender)[0].clone();

    // Main chain height 2: spends the minted UTXO to recipient_main.
    let main_spend = signed_transfer(
        &utxo,
        &recipient_main,
        5 * crate::consensus::COIN,
        &secp,
        &secret,
        &pubkey,
    );
    let main_block = block_with_txs_on(
        funded_modifier,
        funded_hash,
        2,
        funded_ts + 1,
        vec![main_spend],
    );
    chain.add_block(main_block).unwrap();
    assert!(chain.get_utxos_for_address(&sender).is_empty());
    assert_eq!(chain.get_utxos_for_address(&recipient_main).len(), 1);

    // Fork height 2 (same parent, different coinbase → different hash):
    // spends the SAME UTXO to recipient_fork. Stored as a fork.
    let fork_spend = signed_transfer(
        &utxo,
        &recipient_fork,
        4 * crate::consensus::COIN,
        &secp,
        &secret,
        &pubkey,
    );
    let mut fork_block = block_with_txs_on(
        funded_modifier,
        funded_hash,
        2,
        funded_ts + 2,
        vec![fork_spend],
    );
    fork_block.header.nonce = 777;
    fork_block.header.merkle_root = fork_block.compute_merkle_root();
    let fork_hash = fork_block.hash();
    let fork_modifier = fork_block.header.stake_modifier;
    let acceptance = chain.add_block(fork_block).unwrap();
    assert!(
        matches!(acceptance, super::BlockAcceptance::Fork { .. }),
        "same-height competing block must be stored as a fork"
    );
    assert_eq!(chain.best_height(), 2);
    assert!(chain.get_utxos_for_address(&recipient_fork).is_empty());

    // Fork extension at height 3 → the fork becomes the main chain (reorg).
    let ext_block = block_with_txs_on(fork_modifier, fork_hash, 3, funded_ts + 3, vec![]);
    let ext_acceptance = chain.add_block(ext_block).unwrap();
    assert!(
        matches!(ext_acceptance, super::BlockAcceptance::Reorg { .. }),
        "longer fork must trigger a reorg"
    );
    assert_eq!(chain.best_height(), 3);

    // The winning fork's spend is now authoritative.
    assert!(
        chain.get_utxos_for_address(&recipient_main).is_empty(),
        "main-chain spend must be rolled back"
    );
    let fork_utxos = chain.get_utxos_for_address(&recipient_fork);
    assert_eq!(fork_utxos.len(), 1, "fork spend must be live after reorg");
    assert_eq!(fork_utxos[0].value, 4 * crate::consensus::COIN);
    // The spent UTXO's replacement reflects the fork's output value.
    assert!(
        chain.get_utxo(&utxo.txid, utxo.vout).is_none(),
        "the disputed UTXO must remain spent after the reorg"
    );
    let _ = fork_hash;
}

/// Rolling back a block restores its spent inputs and removes its outputs —
/// verified through the public UTXO interface.
#[test]
fn test_rollback_restores_spent_input_and_removes_outputs() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(&[0xAB; 32]).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let sender = {
        let pk = PublicKey::from_secret_key(&secp, &secret);
        vtorrent_core::address::Address::from_pubkey(&pk, true, 70).to_string()
    };
    let recipient = addr_from_seed(0x11);

    let mut chain = Chain::new().unwrap();
    chain
        .mint_to_address(&sender, 10 * crate::consensus::COIN)
        .unwrap();
    let utxo_before = chain.get_utxos_for_address(&sender)[0].clone();

    let spend = signed_transfer(
        &utxo_before,
        &recipient,
        5 * crate::consensus::COIN,
        &secp,
        &secret,
        &pubkey,
    );
    let spend_txid = spend.txid();
    let funded_ts = chain.get_block_at_height(1).unwrap().header.timestamp;
    let block = block_with_txs_on(
        chain.get_block_at_height(1).unwrap().header.stake_modifier,
        chain.best_hash().unwrap(),
        2,
        funded_ts + 1,
        vec![spend],
    );
    chain.add_block(block).unwrap();
    assert!(chain.get_utxos_for_address(&sender).is_empty());
    assert!(chain.get_transaction(&spend_txid).is_some());

    chain.rollback_one_block().unwrap();

    // The spent input is back, the recipient output is gone, and the tx is
    // no longer indexed.
    let restored = chain.get_utxos_for_address(&sender);
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].txid, utxo_before.txid);
    assert!(chain.get_utxos_for_address(&recipient).is_empty());
    assert!(chain.get_transaction(&spend_txid).is_none());
    assert_eq!(chain.best_height(), 1);
}

#[test]
fn test_utxo_root_deterministic_sorted() {
    use crate::block::compute_utxo_root_sorted;
    use crate::consensus::COIN;
    let u1 = Utxo {
        txid: [1u8; 32],
        vout: 0,
        value: 100 * COIN,
        script_pubkey: vec![0x76, 0xa9, 0x14],
        height: 1,
        timestamp: 1_700_000_000,
    };
    let u2 = Utxo {
        txid: [2u8; 32],
        vout: 0,
        value: 200 * COIN,
        script_pubkey: vec![0x76, 0xa9, 0x14],
        height: 1,
        timestamp: 1_700_000_000,
    };
    let root_a = compute_utxo_root_sorted(&[u1.clone(), u2.clone()]);
    let root_b = compute_utxo_root_sorted(&[u2, u1]);
    assert_eq!(root_a, root_b, "root must be sorted canonical");
    assert_ne!(root_a, [0u8; 32]);
}

#[test]
fn test_genesis_utxo_root_nonzero() {
    let genesis = crate::genesis::create_genesis_block();
    assert_ne!(
        genesis.header.utxo_root, [0u8; 32],
        "genesis utxo_root must be non-zero"
    );
}

/// A double-spend of the same input within one block must be rejected and
/// leave the UTXO set untouched (journal rollback path).
#[test]
fn test_double_spend_within_block_rejected_and_rolled_back() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(&[0xAB; 32]).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let sender = {
        let pk = PublicKey::from_secret_key(&secp, &secret);
        vtorrent_core::address::Address::from_pubkey(&pk, true, 70).to_string()
    };
    let r1 = addr_from_seed(0x21);
    let r2 = addr_from_seed(0x22);

    let mut chain = Chain::new().unwrap();
    let genesis_hash = chain.best_hash().unwrap();
    chain
        .mint_to_address(&sender, 10 * crate::consensus::COIN)
        .unwrap();
    let utxo = chain.get_utxos_for_address(&sender)[0].clone();
    let supply_before = chain.total_supply();

    let tx1 = signed_transfer(
        &utxo,
        &r1,
        5 * crate::consensus::COIN,
        &secp,
        &secret,
        &pubkey,
    );
    let tx2 = signed_transfer(
        &utxo,
        &r2,
        6 * crate::consensus::COIN,
        &secp,
        &secret,
        &pubkey,
    );

    let funded_ts = chain.get_block_at_height(1).unwrap().header.timestamp;
    let block = block_with_txs_on(
        chain.get_block_at_height(1).unwrap().header.stake_modifier,
        genesis_hash,
        2,
        funded_ts + 1,
        vec![tx1, tx2],
    );
    assert!(chain.add_block(block).is_err());

    // Journal rollback must have restored the pre-block state exactly.
    assert_eq!(chain.best_height(), 1);
    assert_eq!(chain.total_supply(), supply_before);
    let sender_utxos = chain.get_utxos_for_address(&sender);
    assert_eq!(sender_utxos.len(), 1, "input UTXO must be restored");
    assert!(chain.get_utxos_for_address(&r1).is_empty());
    assert!(chain.get_utxos_for_address(&r2).is_empty());
}

#[test]
fn test_reorg_preserves_block_hash_identity() {
    let mut chain = Chain::new().unwrap();
    let genesis_hash = chain.best_hash().unwrap();
    let a1 = make_block(genesis_hash, 0, 1);
    chain.add_block(a1).unwrap();
    let mut b1 = make_block(genesis_hash, 0, 1);
    b1.header.nonce = 999;
    b1.header.merkle_root = b1.compute_merkle_root();
    let b1_hash = b1.hash();
    let fork_res = chain.add_block(b1.clone()).unwrap();
    assert!(matches!(fork_res, BlockAcceptance::Fork { .. }));
    let b2 = make_block(b1_hash, b1.header.stake_modifier, 2);
    let b2_hash = b2.hash();
    let reorg_res = chain.add_block(b2).unwrap();
    assert!(matches!(reorg_res, BlockAcceptance::Reorg { .. }));
    assert_eq!(chain.best_hash(), Some(b2_hash));
    assert_eq!(chain.get_block(&b1_hash).unwrap().hash(), b1_hash);
    assert_eq!(chain.get_block(&b2_hash).unwrap().hash(), b2_hash);
}
