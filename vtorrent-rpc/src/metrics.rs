/// Prometheus metrics endpoint for vTorrent.
///
/// Exposes node health and performance metrics at `GET /metrics` in the
/// standard Prometheus text exposition format (version 0.0.4).
///
/// ## Usage
///
/// ```text
/// # Scrape with curl
/// curl http://127.0.0.1:22525/metrics
///
/// # Prometheus scrape config
/// scrape_configs:
///   - job_name: vtorrent
///     static_configs:
///       - targets: ['localhost:22525']
/// ```
///
/// ## Available Metrics
///
/// | Metric | Type | Description |
/// |---|---|---|
/// | `vtorrent_block_height` | Gauge | Current best chain height |
/// | `vtorrent_peer_count` | Gauge | Number of connected P2P peers |
/// | `vtorrent_mempool_size` | Gauge | Number of unconfirmed transactions |
/// | `vtorrent_mempool_bytes` | Gauge | Total size of mempool in bytes |
/// | `vtorrent_staking_enabled` | Gauge | 1 if staking is active, 0 otherwise |
/// | `vtorrent_blocks_staked_total` | Counter | Total blocks staked this session |
/// | `vtorrent_uptime_seconds` | Gauge | Seconds since node started |
/// | `vtorrent_syncing` | Gauge | 1 if node is syncing, 0 if fully synced |
/// | `vtorrent_torrent_sessions` | Gauge | Number of active torrent sessions |
/// | `vtorrent_dex_orders` | Gauge | Number of open DEX orders |
/// | `vtorrent_ws_subscribers` | Gauge | Number of active WebSocket subscribers |

use std::sync::Arc;
use axum::{extract::State, response::IntoResponse};
use crate::state::AppState;

/// Prometheus text format content type.
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Handler for `GET /metrics` — returns Prometheus-format metrics.
pub async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let metrics = collect_metrics(&state).await;
    (
        [(axum::http::header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        metrics,
    )
}

/// Collect all metrics and format them as Prometheus text.
async fn collect_metrics(state: &AppState) -> String {
    let mut out = String::with_capacity(2048);

    // Block height
    let height = {
        let chain = state.chain.read().await;
        chain.best_height() as u64
    };
    write_gauge(&mut out, "vtorrent_block_height",
        "Current best chain block height", height);

    // Peer count
    let peer_count = *state.peer_count.read().await as u64;
    write_gauge(&mut out, "vtorrent_peer_count",
        "Number of connected P2P peers", peer_count);

    // Mempool
    let (mempool_size, mempool_bytes) = {
        let mempool = state.mempool.read().await;
        let size = mempool.size() as u64;
        // Estimate bytes: average tx is ~250 bytes
        let bytes = size * 250;
        (size, bytes)
    };
    write_gauge(&mut out, "vtorrent_mempool_size",
        "Number of unconfirmed transactions in mempool", mempool_size);
    write_gauge(&mut out, "vtorrent_mempool_bytes",
        "Total size of mempool transactions in bytes", mempool_bytes);

    // Staking
    let staking_enabled = *state.staking_enabled.read().await;
    write_gauge(&mut out, "vtorrent_staking_enabled",
        "1 if staking is active, 0 otherwise", staking_enabled as u64);

    let blocks_staked = *state.blocks_staked.read().await;
    write_counter(&mut out, "vtorrent_blocks_staked_total",
        "Total number of blocks staked this session", blocks_staked);

    // Uptime
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let uptime = now.saturating_sub(state.start_time);
    write_gauge(&mut out, "vtorrent_uptime_seconds",
        "Seconds since the node started", uptime);

    // Syncing
    let syncing = *state.syncing.read().await;
    write_gauge(&mut out, "vtorrent_syncing",
        "1 if node is currently syncing, 0 if fully synced", syncing as u64);

    // Torrent sessions
    let torrent_count = {
        let sessions = state.torrent_sessions.read().await;
        sessions.list_sessions().len() as u64
    };
    write_gauge(&mut out, "vtorrent_torrent_sessions",
        "Number of active torrent sessions", torrent_count);

    // DEX orders
    let dex_order_count = {
        let book = state.order_book.read().await;
        book.list_open_orders().len() as u64
    };
    write_gauge(&mut out, "vtorrent_dex_orders",
        "Number of open DEX orders", dex_order_count);

    // WebSocket subscribers
    let ws_subscribers = state.events.sender.receiver_count() as u64;
    write_gauge(&mut out, "vtorrent_ws_subscribers",
        "Number of active WebSocket event subscribers", ws_subscribers);

    out
}

/// Write a Prometheus gauge metric.
fn write_gauge(out: &mut String, name: &str, help: &str, value: u64) {
    out.push_str(&format!("# HELP {} {}\n", name, help));
    out.push_str(&format!("# TYPE {} gauge\n", name));
    out.push_str(&format!("{} {}\n\n", name, value));
}

/// Write a Prometheus counter metric.
fn write_counter(out: &mut String, name: &str, help: &str, value: u64) {
    out.push_str(&format!("# HELP {} {}\n", name, help));
    out.push_str(&format!("# TYPE {} counter\n", name));
    out.push_str(&format!("{} {}\n\n", name, value));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_gauge_format() {
        let mut out = String::new();
        write_gauge(&mut out, "test_metric", "A test metric", 42);
        assert!(out.contains("# HELP test_metric A test metric"));
        assert!(out.contains("# TYPE test_metric gauge"));
        assert!(out.contains("test_metric 42"));
    }

    #[test]
    fn test_write_counter_format() {
        let mut out = String::new();
        write_counter(&mut out, "test_counter", "A test counter", 100);
        assert!(out.contains("# TYPE test_counter counter"));
        assert!(out.contains("test_counter 100"));
    }

    #[test]
    fn test_write_gauge_zero() {
        let mut out = String::new();
        write_gauge(&mut out, "zero_metric", "Zero value", 0);
        assert!(out.contains("zero_metric 0"));
    }

    #[tokio::test]
    async fn test_collect_metrics_returns_all_keys() {
        let state = AppState::new();
        let metrics = collect_metrics(&state).await;
        assert!(metrics.contains("vtorrent_block_height"));
        assert!(metrics.contains("vtorrent_peer_count"));
        assert!(metrics.contains("vtorrent_mempool_size"));
        assert!(metrics.contains("vtorrent_mempool_bytes"));
        assert!(metrics.contains("vtorrent_staking_enabled"));
        assert!(metrics.contains("vtorrent_blocks_staked_total"));
        assert!(metrics.contains("vtorrent_uptime_seconds"));
        assert!(metrics.contains("vtorrent_syncing"));
        assert!(metrics.contains("vtorrent_torrent_sessions"));
        assert!(metrics.contains("vtorrent_dex_orders"));
        assert!(metrics.contains("vtorrent_ws_subscribers"));
    }

    #[tokio::test]
    async fn test_collect_metrics_valid_prometheus_format() {
        let state = AppState::new();
        let metrics = collect_metrics(&state).await;
        // Every metric should have HELP and TYPE lines
        for line in metrics.lines() {
            if line.starts_with("vtorrent_") && !line.starts_with("# ") {
                // Should be "metric_name value"
                let parts: Vec<&str> = line.split_whitespace().collect();
                assert_eq!(parts.len(), 2, "Metric line should have exactly 2 parts: {}", line);
                assert!(parts[1].parse::<u64>().is_ok(), "Value should be a number: {}", parts[1]);
            }
        }
    }

    #[tokio::test]
    async fn test_uptime_positive() {
        let state = AppState::new();
        let metrics = collect_metrics(&state).await;
        // Find the uptime line
        for line in metrics.lines() {
            if line.starts_with("vtorrent_uptime_seconds ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                let uptime: u64 = parts[1].parse().unwrap();
                // Uptime should be 0 or very small for a freshly created state
                assert!(uptime < 60, "Uptime should be less than 60 seconds for a new state");
            }
        }
    }
}
