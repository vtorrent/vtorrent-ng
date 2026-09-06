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
}

pub(super) struct SwapScanTracker {
    funding_txid: [u8; 32],
    script: bitcoin::ScriptBuf,
    amount: u64,
    funding: Option<SwapAnchor>,
    spend: Option<SwapAnchor>,
    invalid_funding: bool,
    coinbase: bool,
}

impl SwapScanTracker {
    pub(super) fn new(htlc: &crate::htlc::BtcHtlc, funding_txid: [u8; 32]) -> Result<Self> {
        Ok(Self {
            funding_txid,
            script: htlc.build_script()?.to_p2wsh(),
            amount: htlc.amount,
            funding: None,
            spend: None,
            invalid_funding: false,
            coinbase: false,
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
        })
    }
}
