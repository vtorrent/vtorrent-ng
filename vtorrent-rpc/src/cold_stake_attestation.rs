//! Build node-signed attestations for offline cold-stake signing.

use vtorrent_core::attestation::{FundingAttestation, ATTESTATION_VERSION};
use vtorrent_node::chain::Chain;
use vtorrent_node::mempool::Mempool;

pub const DEFAULT_MIN_CONFIRMATIONS: u32 = 6;
pub const ATTESTATION_TTL_SECONDS: u64 = 3_600;

#[derive(Debug, PartialEq, Eq)]
pub enum AttestationRefusal {
    UnknownTransaction,
    NotConfirmed,
    MissingOutput,
    NotUnspent,
    MempoolSpend,
    TooFewConfirmations { confirmations: u32, required: u32 },
}

pub fn build_attestation(
    chain: &Chain,
    mempool: &Mempool,
    txid: [u8; 32],
    vout: u32,
    network: &str,
    now: u64,
    min_confirmations: u32,
) -> Result<FundingAttestation, AttestationRefusal> {
    let (tx, block_hash, height) = chain.get_transaction(&txid).ok_or_else(|| {
        if mempool.get_transaction(&txid).is_some() {
            AttestationRefusal::NotConfirmed
        } else {
            AttestationRefusal::UnknownTransaction
        }
    })?;
    let output = tx
        .outputs
        .get(vout as usize)
        .ok_or(AttestationRefusal::MissingOutput)?;
    let confirmations = chain.best_height().saturating_sub(height) + 1;
    if chain.get_utxo(&txid, vout).is_none() {
        return Err(AttestationRefusal::NotUnspent);
    }
    if mempool.spender_of(&txid, vout).is_some() {
        return Err(AttestationRefusal::MempoolSpend);
    }
    if confirmations < min_confirmations {
        return Err(AttestationRefusal::TooFewConfirmations {
            confirmations,
            required: min_confirmations,
        });
    }
    Ok(FundingAttestation {
        version: ATTESTATION_VERSION,
        network: network.to_owned(),
        txid: hex::encode(txid),
        vout,
        value_satoshis: output.value,
        script_pubkey: hex::encode(&output.script_pubkey),
        block_hash: hex::encode(block_hash),
        block_height: height,
        confirmations,
        tip_hash: hex::encode(
            chain
                .best_hash()
                .ok_or(AttestationRefusal::UnknownTransaction)?,
        ),
        tip_height: chain.best_height(),
        mempool_spend: false,
        issued_at: now,
        expires_at: now + ATTESTATION_TTL_SECONDS,
        attestor_pubkey: String::new(),
        signature: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtorrent_node::block::{Transaction, TxInput, TxOutput, TxType};

    const ADDRESS: &str = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";

    fn funded_chain() -> (Chain, Mempool, [u8; 32], Vec<u8>) {
        let mut chain = Chain::new_regtest().unwrap();
        let txid = chain.mint_to_address(ADDRESS, 100_000_000).unwrap();
        for _ in 0..5 {
            chain.mint_to_address(ADDRESS, 1).unwrap();
        }
        let script = chain.get_transaction(&txid).unwrap().0.outputs[0]
            .script_pubkey
            .clone();
        (chain, Mempool::new(1_000), txid, script)
    }

    #[test]
    fn refuses_an_unknown_transaction() {
        let (chain, mempool, _, _) = funded_chain();
        assert_eq!(
            build_attestation(&chain, &mempool, [9; 32], 0, "vtorrent-regtest", 10, 6),
            Err(AttestationRefusal::UnknownTransaction)
        );
    }

    #[test]
    fn refuses_an_output_index_that_does_not_exist() {
        let (chain, mempool, txid, _) = funded_chain();
        assert_eq!(
            build_attestation(&chain, &mempool, txid, 9, "vtorrent-regtest", 10, 6),
            Err(AttestationRefusal::MissingOutput)
        );
    }

    #[test]
    fn refuses_too_few_confirmations() {
        let mut chain = Chain::new_regtest().unwrap();
        let txid = chain.mint_to_address(ADDRESS, 100_000_000).unwrap();
        let error = build_attestation(
            &chain,
            &Mempool::new(1_000),
            txid,
            0,
            "vtorrent-regtest",
            10,
            6,
        )
        .unwrap_err();
        assert_eq!(
            error,
            AttestationRefusal::TooFewConfirmations {
                confirmations: 1,
                required: 6
            }
        );
    }

    #[test]
    fn signs_a_confirmed_unspent_outpoint() {
        let (chain, mempool, txid, script) = funded_chain();
        let attestation =
            build_attestation(&chain, &mempool, txid, 0, "vtorrent-regtest", 10, 6).unwrap();
        assert_eq!(attestation.version, ATTESTATION_VERSION);
        assert_eq!(attestation.txid, hex::encode(txid));
        assert_eq!(attestation.value_satoshis, 100_000_000);
        assert_eq!(attestation.script_pubkey, hex::encode(script));
        assert_eq!(attestation.network, "vtorrent-regtest");
        assert_eq!(attestation.block_height, 1);
        assert_eq!(attestation.confirmations, 6);
        assert_eq!(attestation.tip_height, chain.best_height());
        assert!(!attestation.mempool_spend);
        assert_eq!(attestation.issued_at, 10);
        assert_eq!(attestation.expires_at, 10 + ATTESTATION_TTL_SECONDS);
        assert!(attestation.signature.is_empty());
    }

    #[test]
    fn refuses_a_mempool_spend_of_the_outpoint() {
        let (chain, mut mempool, txid, _) = funded_chain();
        let spend = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: txid,
                prev_vout: 0,
                script_sig: vec![1],
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value: 99_000_000,
                script_pubkey: vec![0x51],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        mempool.add_transaction_with_fee(spend, 1_000_000).unwrap();
        assert_eq!(
            build_attestation(&chain, &mempool, txid, 0, "vtorrent-regtest", 10, 6),
            Err(AttestationRefusal::MempoolSpend)
        );
    }

    #[test]
    fn refuses_an_output_that_is_not_in_the_unspent_set() {
        let mut base = Chain::new_regtest().unwrap();
        base.mint_to_address(ADDRESS, 100_000_000).unwrap();
        let mut block = base.get_block_at_height(1).unwrap().clone();
        // An OP_RETURN output never enters the UTXO set, so the transaction is
        // known but no outpoint exists to sign against.
        block.transactions[0].outputs[0].script_pubkey = vec![0x6a];
        let txid = block.transactions[0].txid();
        block.header.merkle_root = block.compute_merkle_root();
        let mut chain = Chain::new_regtest().unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(
            build_attestation(
                &chain,
                &Mempool::new(1_000),
                txid,
                0,
                "vtorrent-regtest",
                10,
                0
            ),
            Err(AttestationRefusal::NotUnspent)
        );
    }
}
