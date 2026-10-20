use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use vtorrent_node::{
    block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType},
    chain::{Chain, Utxo},
    consensus::{compute_stake_modifier, COIN, MIN_STAKE_AGE},
    staking::StakingEngine,
};

fn fixture() -> (Chain, StakingEngine, Vec<Utxo>, SecretKey, Vec<u8>) {
    let sk = SecretKey::from_slice(&[7; 32]).unwrap();
    let pk = PublicKey::from_secret_key(&Secp256k1::new(), &sk);
    let script = vtorrent_script::standard::build_p2cs(
        &vtorrent_core::crypto::hash160(&pk.serialize()),
        &[9; 20],
        2_000_000_000,
    ).unwrap().as_bytes().to_vec();
    let mut chain = Chain::new_regtest_fast().unwrap();
    let genesis = chain.best_hash().unwrap();
    let tx = Transaction {
        version: 1, tx_type: TxType::Coinbase,
        inputs: vec![TxInput { prev_txid: [0;32], prev_vout: u32::MAX, script_sig: vec![1], sequence: u32::MAX }],
        outputs: vec![TxOutput { value: 1000 * COIN, script_pubkey: script.clone() }; 2],
        lock_time: 1, claim_address: None, claim_signature: None,
    };
    let mut funding = Block {
        header: BlockHeader { version: 1, prev_block_hash: genesis, merkle_root: [0;32], utxo_root: [0;32], timestamp: 1_700_000_001, bits: vtorrent_node::genesis::GENESIS_BITS, nonce: 1, stake_modifier: compute_stake_modifier(0, &genesis) },
        transactions: vec![tx],
    };
    funding.header.merkle_root = funding.compute_merkle_root();
    chain.add_block(funding).unwrap();
    let key = vtorrent_core::keys::PrivateKey::from_bytes([7;32], true).unwrap();
    let engine = StakingEngine::with_wif_fast("VMLVUkkn4hJ6Pex3w9RdmdU4BRUarszhHH".into(), key.to_wif(198));
    let utxos = chain.get_utxo_set().values().cloned().collect();
    (chain, engine, utxos, sk, script)
}

fn block(chain: &Chain, engine: &StakingEngine, utxos: &[Utxo]) -> Block {
    let prev = chain.get_block_at_height(1).unwrap();
    (1_700_000_001 + MIN_STAKE_AGE as u32..).take(100_000).find_map(|ts| {
        engine.build_stake_block(chain.best_hash().unwrap(), prev.header.stake_modifier, 2, ts, chain.total_staked(), utxos.to_vec(), vec![])
    }).unwrap()
}

fn hot_sign(tx: &mut Transaction, script: &[u8], sk: &SecretKey) {
    let secp = Secp256k1::new();
    let pk = PublicKey::from_secret_key(&secp, sk);
    let mut sig = secp.sign_ecdsa(&Message::from_digest(tx.sighash(0, script)), sk).serialize_der().to_vec();
    sig.push(1);
    let mut bytes = vec![sig.len() as u8];
    bytes.extend(sig);
    bytes.push(33);
    bytes.extend(pk.serialize());
    bytes.push(0x51);
    tx.inputs[0].script_sig = bytes;
}

fn recommit(chain: &Chain, block: &mut Block) {
    let mut post = chain.get_utxo_set().clone();
    for tx in &block.transactions {
        for input in &tx.inputs { post.remove(&(input.prev_txid, input.prev_vout)); }
        for (i, output) in tx.outputs.iter().enumerate() {
            if vtorrent_node::block::is_utxo_eligible(output, tx, 2) {
                post.insert((tx.txid(), i as u32), Utxo { txid: tx.txid(), vout: i as u32, value: output.value, script_pubkey: output.script_pubkey.clone(), height: 2, timestamp: block.header.timestamp });
            }
        }
    }
    block.header.merkle_root = block.compute_merkle_root();
    block.header.utxo_root = vtorrent_node::block::compute_utxo_root_sorted(&post.values().cloned().collect::<Vec<_>>());
}

#[test]
fn hot_key_ordinary_transaction_is_rejected() {
    let (mut chain, engine, utxos, sk, script) = fixture();
    let mut candidate = block(&chain, &engine, &utxos);
    let staked = &candidate.transactions[0].inputs[0];
    let victim = utxos.iter().find(|u| u.script_pubkey == script && (u.txid, u.vout) != (staked.prev_txid, staked.prev_vout)).unwrap();
    let destination = vtorrent_script::standard::build_p2pkh(&[0x42;20]).unwrap().as_bytes().to_vec();
    let mut theft = Transaction { version: 1, tx_type: TxType::Standard,
        inputs: vec![TxInput { prev_txid: victim.txid, prev_vout: victim.vout, script_sig: vec![], sequence: u32::MAX }],
        outputs: vec![TxOutput { value: victim.value - 10_000, script_pubkey: destination.clone() }],
        lock_time: 0, claim_address: None, claim_signature: None };
    hot_sign(&mut theft, &script, &sk);
    candidate.transactions.push(theft);
    recommit(&chain, &mut candidate);
    assert!(
        chain.add_block(candidate).is_err(),
        "the hot staking key must not spend delegated P2CS principal"
    );
}

#[test]
fn hot_key_coinstake_burn_is_rejected() {
    let (mut chain, engine, utxos, sk, script) = fixture();
    let mut candidate = block(&chain, &engine, &utxos);
    candidate.transactions[0].outputs[1].value = 1;
    hot_sign(&mut candidate.transactions[0], &script, &sk);
    recommit(&chain, &mut candidate);
    assert!(
        chain.add_block(candidate).is_err(),
        "a coinstake must not burn delegated P2CS principal"
    );
}

#[test]
fn protocol_three_notfound_round_trips() {
    use vtorrent_p2p::message::{encode_for_peer, decode_for_peer, InvMsg, InvItem, InvType, PROTOCOL_VERSION};
    let message = InvMsg { items: vec![InvItem { inv_type: InvType::Block, hash: [42;32] }] };
    let bytes = encode_for_peer(&message, PROTOCOL_VERSION);
    assert!(decode_for_peer::<InvMsg>(&bytes, PROTOCOL_VERSION).is_ok());
    assert!(serde_json::from_slice::<InvMsg>(&bytes).is_err(), "JSON-only decoding is the bug");
}

#[test]
fn retained_scan_floor_can_lose_filter_checkpoint_prefix() {
    use bitcoin::{block::{Header, Version}, hashes::Hash, BlockHash, TxMerkleNode, Network};
    let mut chain = vtorrent_btc::headers::HeaderChain::anchored(Network::Regtest);
    for height in 1..=6 {
        let parent = chain.get(&chain.best_hash().unwrap()).unwrap();
        let mut header = Header { version: Version::ONE, prev_blockhash: BlockHash::from_byte_array(chain.best_hash().unwrap()), merkle_root: TxMerkleNode::all_zeros(), time: parent.header.time + 1, bits: parent.header.bits, nonce: 0 };
        while header.validate_pow(header.target()).is_err() { header.nonce += 1; }
        chain.add_header(&bitcoin::consensus::serialize(&header), height).unwrap();
    }
    chain.set_retention(2);
    let start = chain.retained_floor();
    assert_eq!(start, 4);
    let checkpoint = start.saturating_sub(1) / 1000 * 1000;
    let request_start = if checkpoint > 0 { checkpoint + 1 } else { 0 };
    assert_ne!(chain.hashes_from(request_start).len(), (chain.best_height() - request_start + 1) as usize,
        "authenticate_filter_headers rejects the missing prefix");
    assert!(chain.hashes_from(0).len() < 7, "an earlier birthday does not restore pruned headers");
}

#[tokio::test]
async fn desktop_event_channel_drops_block_under_transaction_event_pressure() {
    use std::sync::Arc;
    use vtorrent_node::events::{channel, NodeEvent};
    let (tx, mut rx) = channel(1024);
    tx.send(Arc::new(NodeEvent::NewBlock { height: 1, hash: [1;32], tx_count: 1, timestamp: 0, size_bytes: 0,
        block: Arc::new(vtorrent_node::genesis::create_genesis_block()), utxos_added: vec![], utxos_removed: vec![], claimed_addresses: vec![] })).unwrap();
    for _ in 0..1024 {
        tx.send(Arc::new(NodeEvent::TxUnconfirmed { txid: [2;32], fee_sats: 0, size_bytes: 0 })).unwrap();
    }
    assert!(matches!(rx.recv().await, Err(tokio::sync::broadcast::error::RecvError::Lagged(1))));
    drop(tx);
    while let Ok(event) = rx.recv().await { assert!(!matches!(&*event, NodeEvent::NewBlock { .. })); }
}

#[test]
fn desktop_store_open_requires_parent_directory() {
    let root = tempfile::tempdir().unwrap();
    assert!(vtorrent_store::BlockStore::open(root.path().join("not-created").join("chain.db")).is_err());
}
