//! Unified wallet activity: confirmed chain history plus pending mempool
//! transactions that touch the wallet's addresses.

use crate::chain::Chain;
use crate::mempool::Mempool;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    pub txid: String,
    pub status: String,
    pub tx_type: String,
    pub amount_satoshis: u64,
    pub timestamp: u32,
    pub block_height: u32,
    pub confirmations: u32,
    pub fee_satoshis: u64,
}

/// Merge confirmed history with pending mempool transactions.
pub fn wallet_activity(
    chain: &Chain,
    mempool: &Mempool,
    addresses: &[String],
    limit: usize,
    now: u32,
) -> Vec<ActivityEntry> {
    if limit == 0 || addresses.is_empty() {
        return Vec::new();
    }
    let scripts: HashSet<Vec<u8>> = addresses
        .iter()
        .map(|address| chain.address_to_p2pkh_script(address))
        .filter(|script| !script.is_empty())
        .collect();
    if scripts.is_empty() {
        return Vec::new();
    }

    let best = chain.best_height();
    let mut entries: Vec<ActivityEntry> = chain
        .get_recent_transactions_for_addresses(addresses, limit)
        .into_iter()
        .map(
            |(txid, height, timestamp, tx_type, amount, fee)| ActivityEntry {
                txid,
                status: "confirmed".to_string(),
                tx_type,
                amount_satoshis: amount,
                timestamp,
                block_height: height,
                confirmations: best.saturating_sub(height).saturating_add(1),
                fee_satoshis: fee,
            },
        )
        .collect();

    let confirmed_txids: HashSet<String> = entries.iter().map(|entry| entry.txid.clone()).collect();
    for tx in mempool.get_transactions() {
        let txid = hex::encode(tx.txid());
        if confirmed_txids.contains(&txid) {
            continue;
        }
        let received: u64 = tx
            .outputs
            .iter()
            .filter(|output| scripts.contains(&output.script_pubkey))
            .map(|output| output.value)
            .sum();
        let sent: u64 = tx
            .inputs
            .iter()
            .filter_map(|input| chain.get_utxo(&input.prev_txid, input.prev_vout))
            .filter(|utxo| scripts.contains(&utxo.script_pubkey))
            .map(|utxo| utxo.value)
            .sum();
        if received == 0 && sent == 0 {
            continue;
        }
        let (tx_type, amount) = if sent > received {
            ("send", sent - received)
        } else {
            ("receive", received - sent)
        };
        entries.push(ActivityEntry {
            txid,
            status: "pending".to_string(),
            tx_type: tx_type.to_string(),
            amount_satoshis: amount,
            timestamp: now,
            block_height: 0,
            confirmations: 0,
            fee_satoshis: pending_fee(chain, &tx),
        });
    }

    entries.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.status.cmp(&a.status))
    });
    entries.truncate(limit);
    entries
}

fn pending_fee(chain: &Chain, tx: &crate::block::Transaction) -> u64 {
    let mut input_total = 0u64;
    for input in &tx.inputs {
        let Some(utxo) = chain.get_utxo(&input.prev_txid, input.prev_vout) else {
            return 0;
        };
        input_total = input_total.saturating_add(utxo.value);
    }
    let output_total: u64 = tx.outputs.iter().map(|output| output.value).sum();
    input_total.saturating_sub(output_total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::COIN;

    #[test]
    fn pending_fee_uses_chain_inputs_and_outputs() {
        use crate::block::{Transaction, TxInput, TxOutput, TxType};
        let address = "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT";
        let mut chain = Chain::new_regtest().unwrap();
        chain.mint_to_address(address, 10 * COIN).unwrap();
        let utxo = chain
            .get_utxos_for_address(address)
            .into_iter()
            .next()
            .unwrap();
        let tx = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: utxo.txid,
                prev_vout: utxo.vout,
                script_sig: vec![1],
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value: 9 * COIN,
                script_pubkey: vec![0x51],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        assert_eq!(pending_fee(&chain, &tx), COIN);
    }

    #[test]
    fn confirmed_activity_includes_minted_outputs() {
        let address = "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT";
        let mut chain = Chain::new_regtest().unwrap();
        chain.mint_to_address(address, 10 * COIN).unwrap();
        let mempool = Mempool::new(10_000);
        let entries = wallet_activity(&chain, &mempool, &[address.to_string()], 10, 1_700_000_100);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "confirmed");
        assert_eq!(entries[0].amount_satoshis, 10 * COIN);
        assert_eq!(entries[0].confirmations, 1);
    }
}
