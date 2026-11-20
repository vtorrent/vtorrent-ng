//! Background services shared by the daemon and the desktop node.

use crate::state::AppState;
use std::time::Duration;

/// Start the VTR and BTC swap reconcilers.
///
/// Both are required: the VTR reconciler tracks the local HTLC, while the BTC
/// reconciler observes the counterparty's claim, records the revealed
/// preimage, and drives the taker's automatic VTR claim. Starting only one
/// leaves a takers' funds dependent on a manual RPC call.
pub fn spawn_swap_services(state: AppState) -> Vec<tokio::task::JoinHandle<()>> {
    vec![
        tokio::spawn(crate::swap_reconciliation::run_reconciler(state.clone())),
        tokio::spawn(crate::btc_reconciliation::run_btc_reconciler(state)),
    ]
}

/// Keep the in-memory SPV header view aligned with the canonical VTR chain.
pub fn spawn_spv_sync(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = crate::spv_sync::sync_spv_from_chain(&state).await {
                tracing::warn!("SPV header sync failed: {}", error);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starts_both_swap_reconcilers() {
        let handles = spawn_swap_services(AppState::new());
        assert_eq!(
            handles.len(),
            2,
            "both the VTR and BTC swap reconcilers must run"
        );
        for handle in handles {
            handle.abort();
        }
    }
}
