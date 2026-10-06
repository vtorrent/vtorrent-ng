//! Unified earnings summary: staking rewards + torrent incentives + swap P&L.
//!
//! Read-only aggregation over the existing subsystem data (no new chain logic,
//! no second source of truth). See `docs/earnings-view-design.md`.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::RpcResult;
use crate::state::AppState;

/// Torrent incentives are accounted in satoshis; staking rewards are satoshis.
/// Everything is reported in satoshis and formatted to VTR at the edge.
const COIN: u64 = 100_000_000;

#[derive(Debug, Deserialize)]
pub struct EarningsQuery {
    /// Window: `24h`, `7d`, `30d`, or `all` (default `30d`).
    #[serde(default)]
    pub window: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EarningsSummary {
    pub window: String,
    pub staking: StakingEarnings,
    pub torrents: TorrentEarnings,
    pub swaps: SwapEarnings,
    /// Total across all categories, in satoshis.
    pub total_earned_sats: u64,
    pub as_of_height: u64,
}

#[derive(Debug, Serialize)]
pub struct StakingEarnings {
    pub rewards_sats: u64,
    pub blocks: u64,
}

#[derive(Debug, Serialize)]
pub struct TorrentEarnings {
    pub earned_sats: u64,
    pub paid_sats: u64,
    pub sessions: u64,
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct SwapEarnings {
    pub completed: u64,
    pub open: u64,
}

/// Parse a window string into a max age in seconds (`None` = all time).
fn window_secs(window: &str) -> Option<u64> {
    match window {
        "24h" => Some(24 * 3600),
        "7d" => Some(7 * 24 * 3600),
        "30d" => Some(30 * 24 * 3600),
        _ => None, // "all" or unknown → no time bound
    }
}

/// GET /api/v1/earnings/summary
pub async fn get_earnings_summary(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<EarningsQuery>,
) -> RpcResult<Json<EarningsSummary>> {
    let window = query.window.unwrap_or_else(|| "30d".to_string());
    let max_age = window_secs(&window);
    let now = vtorrent_core::time::now_secs();

    // ── Staking: reuse the same coinstake scan as /staking/rewards ────────────
    let (staking, as_of_height) = {
        let chain = state.chain.lock().await;
        let tip = chain.best_height();
        let mut rewards_sats: u64 = 0;
        let mut blocks: u64 = 0;
        let mut height = tip;
        let mut scanned: u32 = 0;
        // Bound the scan; a full address index is the proper long-term fix.
        const MAX_SCAN_BLOCKS: u32 = 200_000;
        while scanned < MAX_SCAN_BLOCKS {
            let Some(block) = chain.get_block_at_height(height) else {
                break;
            };
            scanned += 1;
            if let Some(age) = max_age {
                if now.saturating_sub(block.header.timestamp as u64) > age {
                    break;
                }
            }
            if let Some(coinstake) = block
                .transactions
                .iter()
                .find(|tx| tx.tx_type == vtorrent_node::block::TxType::Coinstake)
            {
                if let Some(reward_sats) = super::staking::coinstake_reward(&chain, coinstake) {
                    rewards_sats = rewards_sats.saturating_add(reward_sats);
                    blocks += 1;
                }
            }
            if height == 0 {
                break;
            }
            height -= 1;
        }
        (
            StakingEarnings {
                rewards_sats,
                blocks,
            },
            tip as u64,
        )
    };

    // ── Torrents: sum the per-session incentive summaries ─────────────────────
    let torrents = {
        let sessions = state.torrent_sessions.read().await;
        let mut earned = 0u64;
        let mut paid = 0u64;
        let mut uploaded = 0u64;
        let mut downloaded = 0u64;
        let mut count = 0u64;
        for s in sessions.list_sessions() {
            let summary = s.incentive_summary();
            earned = earned.saturating_add(summary.total_earned_satoshis);
            paid = paid.saturating_add(summary.total_paid_satoshis);
            uploaded = uploaded.saturating_add(summary.total_bytes_uploaded);
            downloaded = downloaded.saturating_add(summary.total_bytes_downloaded);
            count += 1;
        }
        TorrentEarnings {
            earned_sats: earned,
            paid_sats: paid,
            sessions: count,
            uploaded_bytes: uploaded,
            downloaded_bytes: downloaded,
        }
    };

    // ── Swaps: count completed vs open orders ─────────────────────────────────
    let swaps = {
        use vtorrent_node::atomic_swap::OrderStatus;
        let book = state.order_book.read().await;
        let mut completed = 0u64;
        let mut open = 0u64;
        for order in book.list_orders() {
            match order.status {
                OrderStatus::Completed => completed += 1,
                OrderStatus::Open
                | OrderStatus::Funding
                | OrderStatus::Matched
                | OrderStatus::InProgress => open += 1,
                OrderStatus::Cancelled => {}
            }
        }
        SwapEarnings { completed, open }
    };

    // Total earned = staking rewards + net torrent earnings (earned - paid).
    let torrent_net = torrents.earned_sats.saturating_sub(torrents.paid_sats);
    let total_earned_sats = staking.rewards_sats.saturating_add(torrent_net);

    Ok(Json(EarningsSummary {
        window,
        staking,
        torrents,
        swaps,
        total_earned_sats,
        as_of_height,
    }))
}

/// Format satoshis as a VTR string (8 dp) — shared by tests and the UI.
pub fn sats_to_vtr(sats: u64) -> String {
    format!("{}.{:08}", sats / COIN, sats % COIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_parsing() {
        assert_eq!(window_secs("24h"), Some(86_400));
        assert_eq!(window_secs("7d"), Some(604_800));
        assert_eq!(window_secs("30d"), Some(2_592_000));
        assert_eq!(window_secs("all"), None);
        assert_eq!(window_secs("garbage"), None);
    }

    #[test]
    fn sats_formatting_is_exact() {
        assert_eq!(sats_to_vtr(0), "0.00000000");
        assert_eq!(sats_to_vtr(1), "0.00000001");
        assert_eq!(sats_to_vtr(100_000_000), "1.00000000");
        assert_eq!(sats_to_vtr(150_000_000), "1.50000000");
        assert_eq!(sats_to_vtr(123_456_789), "1.23456789");
    }
}
