use super::{chain_reorg::UtxoChange, Chain, Utxo};
use crate::{
    block::{Block, BlockHeader},
    staking::spv_transaction_mirror,
};
use std::collections::BTreeMap;
use vtorrent_spv::{
    merkle::MerkleTree,
    stake::{SpvUtxo, StakeProof, UtxoInclusionProof},
};

pub(crate) struct StakeProofSnapshot {
    block: Block,
    parent: BlockHeader,
    utxos: BTreeMap<([u8; 32], u32), Utxo>,
    undo: Vec<Vec<UtxoChange>>,
}

impl Chain {
    /// Capture the retained canonical pre-state for reconstruction outside the chain lock.
    pub(crate) fn stake_proof_snapshot(&self, hash: &[u8; 32]) -> Option<StakeProofSnapshot> {
        let height = self.block_height(hash)?;
        if height == 0 || self.block_hash_at_height(height) != Some(*hash) {
            return None;
        }
        if !self
            .journals
            .iter()
            .any(|journal| journal.block_hash == *hash)
        {
            return None;
        }
        let block = self.get_block_owned(hash)?;
        if !block.header.is_pos() {
            return None;
        }
        let coinstake = block.transactions.first().filter(|tx| tx.is_coinstake())?;
        if coinstake.inputs.len() != 1 {
            return None;
        }
        let parent = self.get_header(&block.header.prev_block_hash)?.clone();
        let mut expected_height = self.best_height();
        let mut undo = Vec::new();
        for journal in self.journals.iter().rev() {
            if journal.height != expected_height {
                return None;
            }
            undo.push(journal.changes.clone());
            if journal.height == height {
                return Some(StakeProofSnapshot {
                    block,
                    parent,
                    utxos: self.utxo_set.clone(),
                    undo,
                });
            }
            expected_height = expected_height.checked_sub(1)?;
        }
        None
    }

    /// Bind a relayed proof to a validated canonical block and its exact stake input.
    pub(crate) fn stake_proof_matches(&self, hash: &[u8; 32], proof: &StakeProof) -> bool {
        let Some(height) = self.block_height(hash) else {
            return false;
        };
        if self.block_hash_at_height(height) != Some(*hash) {
            return false;
        }
        let Some(block) = self.get_block_owned(hash) else {
            return false;
        };
        let Some(parent) = self.get_header(&block.header.prev_block_hash) else {
            return false;
        };
        proof_matches_block(&block, parent, proof)
    }
}

impl StakeProofSnapshot {
    pub(crate) fn reconstruct(self) -> Option<StakeProof> {
        let Self {
            block,
            parent,
            mut utxos,
            undo,
        } = self;
        for changes in undo {
            for change in changes.into_iter().rev() {
                match change {
                    UtxoChange::Added { key } => {
                        utxos.remove(&key);
                    }
                    UtxoChange::Removed { key, utxo } => {
                        utxos.insert(key, utxo);
                    }
                }
            }
        }
        let coinstake = block.transactions.first()?;
        let input = coinstake.inputs.first()?;
        let key = (input.prev_txid, input.prev_vout);
        let utxo = utxos.get(&key)?;
        let index = utxos.keys().position(|k| *k == key)?;
        let leaves: Vec<_> = utxos.values().map(crate::block::hash_utxo).collect();
        let tree = MerkleTree::build(&leaves);
        if tree.root() != parent.utxo_root {
            return None;
        }
        let inclusion = tree.proof(index)?;
        let txids: Vec<_> = block.transactions.iter().map(|tx| tx.txid()).collect();
        let proof = StakeProof {
            coinstake: spv_transaction_mirror(coinstake),
            tx_merkle_proof: MerkleTree::build(&txids).proof(0)?,
            utxo: SpvUtxo {
                txid: utxo.txid,
                vout: utxo.vout,
                value: utxo.value,
                script_pubkey: utxo.script_pubkey.clone(),
                height: utxo.height,
                timestamp: utxo.timestamp,
            },
            utxo_proof: UtxoInclusionProof {
                leaf_index: index,
                siblings: inclusion.siblings,
                root: parent.utxo_root,
            },
            prev_stake_modifier: parent.stake_modifier,
        };
        proof_matches_block(&block, &parent, &proof).then_some(proof)
    }
}

fn proof_matches_block(block: &Block, parent: &BlockHeader, proof: &StakeProof) -> bool {
    let Some(tx) = block.transactions.first().filter(|tx| tx.is_coinstake()) else {
        return false;
    };
    let Some(input) = tx.inputs.first() else {
        return false;
    };
    let txid = tx.txid();
    block.header.is_pos()
        && tx.inputs.len() == 1
        && proof.coinstake.txid() == txid
        && proof.tx_merkle_proof.txid == txid
        && proof.tx_merkle_proof.index == 0
        && proof.tx_merkle_proof.root == block.header.merkle_root
        && proof
            .tx_merkle_proof
            .verify(&block.header.merkle_root)
            .is_ok()
        && proof.utxo.txid == input.prev_txid
        && proof.utxo.vout == input.prev_vout
        && proof.prev_stake_modifier == parent.stake_modifier
        && proof
            .utxo_proof
            .verify(
                &parent.utxo_root,
                &vtorrent_spv::stake::hash_utxo(&proof.utxo),
            )
            .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        block::{BlockHeader, Transaction, TxInput, TxOutput, TxType},
        consensus::{compute_stake_modifier, COIN, MIN_STAKE_AGE},
        staking::StakingEngine,
    };

    fn fixture() -> (Chain, Block, StakeProof, Vec<super::super::Utxo>) {
        let mut chain = Chain::new_regtest().unwrap();
        let key = vtorrent_core::keys::PrivateKey::from_bytes([7; 32], true).unwrap();
        let secp = secp256k1::Secp256k1::new();
        let pk = secp256k1::PublicKey::from_secret_key(
            &secp,
            &secp256k1::SecretKey::from_slice(key.as_bytes()).unwrap(),
        );
        let address = vtorrent_core::address::Address::from_pubkey(&pk, true, 70);
        let script = address.p2pkh_script_pubkey();
        let parent = chain.best_hash().unwrap();
        let tx = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![TxInput {
                prev_txid: [0; 32],
                prev_vout: u32::MAX,
                script_sig: vec![1],
                sequence: u32::MAX,
            }],
            outputs: vec![
                TxOutput {
                    value: 1000 * COIN,
                    script_pubkey: script.clone(),
                },
                TxOutput {
                    value: 1,
                    script_pubkey: script,
                },
            ],
            lock_time: 1,
            claim_address: None,
            claim_signature: None,
        };
        let timestamp = 1_700_000_001;
        let mut post = chain.utxo_set.clone();
        for (vout, out) in tx.outputs.iter().enumerate() {
            post.insert(
                (tx.txid(), vout as u32),
                super::super::Utxo {
                    txid: tx.txid(),
                    vout: vout as u32,
                    value: out.value,
                    script_pubkey: out.script_pubkey.clone(),
                    height: 1,
                    timestamp,
                },
            );
        }
        let mut funding = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: parent,
                merkle_root: [0; 32],
                utxo_root: crate::block::compute_utxo_root_ordered(post.values()),
                timestamp,
                bits: crate::genesis::GENESIS_BITS,
                nonce: 1,
                stake_modifier: compute_stake_modifier(0, &parent),
            },
            transactions: vec![tx],
        };
        funding.header.merkle_root = funding.compute_merkle_root();
        chain.add_block(funding).unwrap();
        let pre: Vec<_> = chain.utxo_set.values().cloned().collect();
        let dust = pre.iter().find(|u| u.height == 1 && u.value == 1).unwrap();
        let mut payment = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: dust.txid,
                prev_vout: dust.vout,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value: 1,
                script_pubkey: dust.script_pubkey.clone(),
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let secret = secp256k1::SecretKey::from_slice(key.as_bytes()).unwrap();
        let mut sig = secp
            .sign_ecdsa(
                &secp256k1::Message::from_digest(payment.sighash(0, &dust.script_pubkey)),
                &secret,
            )
            .serialize_der()
            .to_vec();
        sig.push(1);
        let mut input_script = vtorrent_script::Script::new();
        input_script.push_data(&sig).unwrap();
        input_script.push_data(&pk.serialize()).unwrap();
        payment.inputs[0].script_sig = input_script.as_bytes().to_vec();
        let mut engine = StakingEngine::with_wif(address.to_string(), key.to_wif(198));
        engine.set_consensus_params(chain.params());
        let parent = chain.best_hash().unwrap();
        let modifier = chain.get_header(&parent).unwrap().stake_modifier;
        let (block, proof) = (timestamp + MIN_STAKE_AGE as u32 + 1..)
            .take(1000)
            .find_map(|ts| {
                engine.build_stake_block_with_proof(
                    parent,
                    modifier,
                    2,
                    ts,
                    chain.total_staked(),
                    pre.clone(),
                    vec![payment.clone()],
                )
            })
            .unwrap();
        chain.add_block(block.clone()).unwrap();
        (chain, block, proof, pre)
    }

    #[test]
    fn reconstructs_exact_proof_without_mutating_chain_or_requiring_parent_body() {
        let (mut chain, block, expected, _) = fixture();
        let before = chain.utxo_set.clone();
        assert_eq!(
            chain
                .stake_proof_snapshot(&block.hash())
                .unwrap()
                .reconstruct(),
            Some(expected.clone())
        );
        assert_eq!(chain.utxo_set, before);
        chain.blocks.remove(&block.header.prev_block_hash);
        assert!(chain.stake_proof_matches(&block.hash(), &expected));
        assert_eq!(
            chain
                .stake_proof_snapshot(&block.hash())
                .unwrap()
                .reconstruct(),
            Some(expected)
        );
    }

    #[test]
    fn rejects_a_valid_membership_path_for_the_wrong_staking_coin() {
        let (chain, block, mut proof, pre) = fixture();
        let index = pre
            .iter()
            .position(|u| u.height == 1 && u.value == 1)
            .unwrap();
        let other = &pre[index];
        let leaves: Vec<_> = pre.iter().map(crate::block::hash_utxo).collect();
        let tree = MerkleTree::build(&leaves);
        proof.utxo = SpvUtxo {
            txid: other.txid,
            vout: other.vout,
            value: other.value,
            script_pubkey: other.script_pubkey.clone(),
            height: other.height,
            timestamp: other.timestamp,
        };
        proof.utxo_proof = UtxoInclusionProof {
            leaf_index: index,
            siblings: tree.proof(index).unwrap().siblings,
            root: tree.root(),
        };
        assert!(proof
            .utxo_proof
            .verify(&tree.root(), &vtorrent_spv::stake::hash_utxo(&proof.utxo))
            .is_ok());
        assert!(!chain.stake_proof_matches(&block.hash(), &proof));
    }

    #[test]
    fn rejects_unbound_merkle_fields_and_stake_modifier() {
        let (chain, block, proof, _) = fixture();
        let mut bad = proof.clone();
        let txids: Vec<_> = block.transactions.iter().map(|tx| tx.txid()).collect();
        bad.tx_merkle_proof = MerkleTree::build(&txids).proof(1).unwrap();
        assert!(bad
            .tx_merkle_proof
            .verify(&block.header.merkle_root)
            .is_ok());
        assert!(!chain.stake_proof_matches(&block.hash(), &bad));
        let mut bad = proof.clone();
        bad.tx_merkle_proof.index = 1;
        assert!(!chain.stake_proof_matches(&block.hash(), &bad));
        let mut bad = proof;
        bad.prev_stake_modifier ^= 1;
        assert!(!chain.stake_proof_matches(&block.hash(), &bad));
    }

    #[test]
    fn reconstruction_fails_closed_outside_the_undo_window() {
        let (mut chain, block, proof, _) = fixture();
        chain.set_max_reorg_depth(1);
        let owner = vtorrent_core::address::Address::from_hash160(&[42; 20], 70).unwrap();
        chain.mint_to_address(&owner.to_string(), 1).unwrap();
        assert!(chain.stake_proof_snapshot(&block.hash()).is_none());
        assert!(chain.stake_proof_matches(&block.hash(), &proof));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reorg_during_reconstruction_rejects_result_and_evicts_cached_orphan() {
        let (chain, old, original, _) = fixture();
        let mut fork = Chain::new_regtest().unwrap();
        fork.add_block(chain.get_block_owned(&old.header.prev_block_hash).unwrap())
            .unwrap();
        let private = vtorrent_core::keys::PrivateKey::from_bytes([7; 32], true).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(
            &secp256k1::Secp256k1::new(),
            &secp256k1::SecretKey::from_slice(private.as_bytes()).unwrap(),
        );
        let address = vtorrent_core::address::Address::from_pubkey(&pk, true, 70);
        let mut engine = StakingEngine::with_wif(address.to_string(), private.to_wif(198));
        let mut blocks = Vec::new();
        let mut winning_proof = None;
        for _ in 0..2 {
            let parent = fork.get_header(&fork.best_hash().unwrap()).unwrap();
            let first = (parent.timestamp + MIN_STAKE_AGE as u32 + 1).max(old.header.timestamp + 1);
            engine.set_consensus_params(fork.params());
            let (block, proof) = (first..first + 1000)
                .find_map(|ts| {
                    engine.build_stake_block_with_proof(
                        fork.best_hash().unwrap(),
                        parent.stake_modifier,
                        fork.best_height() + 1,
                        ts,
                        fork.total_staked(),
                        fork.get_utxo_set().values().cloned().collect(),
                        vec![],
                    )
                })
                .unwrap();
            fork.add_block(block.clone()).unwrap();
            blocks.push(block);
            winning_proof = Some(proof);
        }
        let node = crate::node::Node::new_with_chain(
            crate::node::NodeConfig {
                regtest: true,
                testnet: true,
                listen_addr: "127.0.0.1:0".into(),
                ..Default::default()
            },
            chain,
        )
        .unwrap();
        node.cache_stake_proof(old.hash(), original.clone()).await;
        let snapshot = node
            .chain
            .lock()
            .await
            .stake_proof_snapshot(&old.hash())
            .unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            paused.recv().unwrap();
            snapshot.reconstruct().unwrap()
        });
        ready.await.unwrap();
        {
            let mut chain = node.chain.lock().await;
            chain.add_block(blocks[0].clone()).unwrap();
            assert!(matches!(
                chain.add_block(blocks[1].clone()).unwrap(),
                super::super::BlockAcceptance::Reorg { .. }
            ));
            assert_eq!(chain.best_hash(), fork.best_hash());
        }
        resume.send(()).unwrap();
        let stale = worker.await.unwrap();
        assert_eq!(stale, original);
        assert!(
            node.cache_canonical_stake_proof(old.hash(), stale)
                .await
                .is_none(),
            "a reconstruction from the abandoned branch must not be served"
        );
        assert!(!node.stake_proofs.read().await.contains_key(&old.hash()));
        assert!(node
            .cache_canonical_stake_proof(blocks[1].hash(), winning_proof.unwrap())
            .await
            .is_some());
    }

    #[tokio::test]
    async fn invalid_relay_does_not_evict_a_valid_canonical_cached_proof() {
        let (chain, block, proof, _) = fixture();
        let node =
            crate::node::Node::new_with_chain(crate::node::NodeConfig::default(), chain).unwrap();
        assert!(node
            .cache_canonical_stake_proof(block.hash(), proof.clone())
            .await
            .is_some());
        let mut bad = proof.clone();
        bad.prev_stake_modifier ^= 1;
        assert!(node
            .cache_canonical_stake_proof(block.hash(), bad)
            .await
            .is_none());
        assert_eq!(
            node.stake_proofs.read().await.get(&block.hash()),
            Some(&proof),
            "an invalid relay must preserve the valid canonical cached proof"
        );
    }
}
