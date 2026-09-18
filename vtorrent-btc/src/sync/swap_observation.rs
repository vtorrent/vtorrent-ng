use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapAnchor {
    pub txid: [u8; 32],
    pub block_hash: [u8; 32],
    pub height: u32,
}

/// Committed transaction evidence from a bounded, fresh compact-filter scan.
#[derive(Debug, Clone)]
pub struct SwapScan {
    pub tip_hash: [u8; 32],
    pub tip_height: u32,
    pub scan_start: u32,
    pub funding: Option<SwapAnchor>,
    pub spend: Option<SwapAnchor>,
    pub invalid_funding: bool,
    pub coinbase: bool,
    pub invalidated_anchor: bool,
    /// The preimage revealed by the maker's claim, if the spend of the funding
    /// output carried a valid 32-byte preimage matching the HTLC hash lock.
    ///
    /// This is how a taker on a different node learns the secret: the maker
    /// reveals it in the BTC claim witness, and the taker's own scan observes
    /// it. `None` means the spend was not a hash-lock claim (e.g. a refund).
    pub preimage: Option<[u8; 32]>,
}

pub(super) struct SwapScanTracker {
    funding_txid: [u8; 32],
    script: bitcoin::ScriptBuf,
    amount: u64,
    hash_lock: [u8; 32],
    funding: Option<SwapAnchor>,
    spend: Option<SwapAnchor>,
    invalid_funding: bool,
    coinbase: bool,
    preimage: Option<[u8; 32]>,
}

impl SwapScanTracker {
    pub(super) fn new(htlc: &crate::htlc::BtcHtlc, funding_txid: [u8; 32]) -> Result<Self> {
        Ok(Self {
            funding_txid,
            script: htlc.build_script()?.to_p2wsh(),
            amount: htlc.amount,
            hash_lock: htlc.hash_lock,
            funding: None,
            spend: None,
            invalid_funding: false,
            coinbase: false,
            preimage: None,
        })
    }

    pub(super) fn record(&mut self, tx: &bitcoin::Transaction, height: u32, block_hash: [u8; 32]) {
        let txid = tx.compute_txid().to_byte_array();
        let anchor = SwapAnchor {
            txid,
            block_hash,
            height,
        };
        if txid == self.funding_txid {
            self.invalid_funding = !tx.output.first().is_some_and(|output| {
                output.value.to_sat() == self.amount && output.script_pubkey == self.script
            });
            self.coinbase = tx.is_coinbase();
            self.funding = Some(anchor.clone());
        }
        if tx.input.iter().any(|input| {
            input.previous_output.txid.to_byte_array() == self.funding_txid
                && input.previous_output.vout == 0
        }) {
            self.spend = Some(anchor);
            // A hash-lock claim reveals the preimage in the witness as a
            // 32-byte element whose SHA-256 equals the HTLC hash lock. This is
            // how a taker on another node learns the secret. A refund witness
            // has no such element, so this stays `None`.
            for item in tx.input.iter().flat_map(|input| input.witness.iter()) {
                if item.len() == 32 {
                    let candidate: [u8; 32] = item.try_into().expect("len checked");
                    if bitcoin::hashes::sha256::Hash::hash(&candidate).to_byte_array()
                        == self.hash_lock
                    {
                        self.preimage = Some(candidate);
                        break;
                    }
                }
            }
        }
    }
}

impl BtcSync {
    pub(super) fn finish_swap_scan(
        &self,
        start: u32,
        tip_hash: [u8; 32],
        scanned: usize,
    ) -> Result<SwapScan> {
        let chain = self.headers.lock();
        let tip = chain.best_height();
        if chain.best_hash() != Some(tip_hash)
            || tip.checked_sub(start).map(|n| u64::from(n) + 1) != Some(scanned as u64)
        {
            return Err(BtcError::Sync(
                "BTC contract scan is incomplete or its tip changed".into(),
            ));
        }
        let guard = self.swap_scan.lock();
        let scan = guard
            .as_ref()
            .ok_or_else(|| BtcError::Sync("Missing BTC scan tracker".into()))?;
        Ok(SwapScan {
            tip_hash,
            tip_height: tip,
            scan_start: start,
            funding: scan.funding.clone(),
            spend: scan.spend.clone(),
            invalid_funding: scan.invalid_funding,
            coinbase: scan.coinbase,
            invalidated_anchor: false,
            preimage: scan.preimage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker_with_hash_lock(hash_lock: [u8; 32]) -> SwapScanTracker {
        let htlc = crate::htlc::BtcHtlc {
            hash_lock,
            recipient: crate::keys::derive_address(&[1; 64], 0, bitcoin::Network::Regtest).unwrap(),
            refund_address: crate::keys::derive_address(&[2; 64], 0, bitcoin::Network::Regtest)
                .unwrap(),
            expiry: 1_800_086_400,
            amount: 100_000,
            network: bitcoin::Network::Regtest,
        };
        SwapScanTracker::new(&htlc, [9u8; 32]).unwrap()
    }

    fn spend_tx(witness_items: Vec<Vec<u8>>) -> bitcoin::Transaction {
        use bitcoin::hashes::Hash;
        let mut witness = bitcoin::Witness::new();
        for item in witness_items {
            witness.push(item);
        }
        bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint::new(
                    bitcoin::Txid::from_byte_array([9u8; 32]),
                    0,
                ),
                witness,
                ..Default::default()
            }],
            output: vec![],
        }
    }

    #[test]
    fn records_preimage_from_a_valid_claim_witness() {
        use bitcoin::hashes::Hash;
        let preimage = [7u8; 32];
        let hash_lock = bitcoin::hashes::sha256::Hash::hash(&preimage).to_byte_array();
        let mut tracker = tracker_with_hash_lock(hash_lock);
        // Witness shape of a claim: <sig> <pubkey> <preimage> <true> <script>.
        let tx = spend_tx(vec![
            vec![0x30; 71],
            vec![0x02; 33],
            preimage.to_vec(),
            vec![1],
            vec![0x63],
        ]);
        tracker.record(&tx, 100, [3u8; 32]);
        assert_eq!(tracker.preimage, Some(preimage));
        assert!(tracker.spend.is_some());
    }

    #[test]
    fn does_not_record_a_preimage_that_mismatches_the_hash_lock() {
        use bitcoin::hashes::Hash;
        let hash_lock = bitcoin::hashes::sha256::Hash::hash(&[7u8; 32]).to_byte_array();
        let mut tracker = tracker_with_hash_lock(hash_lock);
        // A 32-byte witness element that is not the preimage must be ignored.
        let tx = spend_tx(vec![vec![0x30; 71], vec![0x02; 33], vec![8u8; 32], vec![1]]);
        tracker.record(&tx, 100, [3u8; 32]);
        assert_eq!(tracker.preimage, None);
    }

    #[test]
    fn refund_witness_reveals_no_preimage() {
        use bitcoin::hashes::Hash;
        let hash_lock = bitcoin::hashes::sha256::Hash::hash(&[7u8; 32]).to_byte_array();
        let mut tracker = tracker_with_hash_lock(hash_lock);
        // Refund witness: <sig> <pubkey> <empty> <script> — no 32-byte element.
        let tx = spend_tx(vec![vec![0x30; 71], vec![0x02; 33], vec![], vec![0x63]]);
        tracker.record(&tx, 100, [3u8; 32]);
        assert_eq!(tracker.preimage, None);
        assert!(tracker.spend.is_some());
    }
}
