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
    let result = chain.add_block(block).unwrap();
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

    let utxos = chain.get_utxos_for_address(&address);
    assert!(!utxos.is_empty(), "staking address must have a UTXO");

    // Search for a timestamp whose kernel hash satisfies the target.
    let engine = StakingEngine::with_wif(address.clone(), wif);
    let prev_stake_modifier = chain
        .get_block_at_height(1)
        .map(|b| b.header.stake_modifier)
        .unwrap_or(0);
    let mut stake_block = None;
    let mut ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
    for _ in 0..10_000 {
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
        ts += 1;
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
        let utxos = chain.get_utxos_for_address(&address);
        assert!(
            !utxos.is_empty(),
            "must have UTXOs at height {}",
            expected_height
        );

        let mut stake_block = None;
        let mut ts = chain
            .get_block_at_height(expected_height - 1)
            .unwrap()
            .header
            .timestamp
            + crate::consensus::MIN_STAKE_AGE as u32;
        for _ in 0..100_000 {
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
            ts += 1;
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
    let mut ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
    for _ in 0..100_000 {
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
        ts += 1;
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
