use crate::handlers::*;
use crate::metrics::metrics_handler;
use crate::ratelimit::ip_rate_limit;
use crate::state::AppState;
use crate::ws::ws_handler;
use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

/// Default RPC port — same as legacy vTorrent RPC port + 1.
pub const DEFAULT_RPC_PORT: u16 = 22525;

/// Constant-time string comparison to avoid timing side-channels on the API key.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Reject requests without a valid `X-API-Key` header when a key is configured.
///
/// When `AppState::rpc_api_key` is `None` (auth disabled), requests pass
/// through unchanged — this keeps the standalone/test mode and the read-only
/// info endpoints working without a key.
async fn require_api_key(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, axum::http::StatusCode> {
    if let Some(expected) = &state.rpc_api_key {
        let provided = request
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !constant_time_eq(provided, expected) {
            return Err(axum::http::StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(request).await)
}

/// Semaphore-backed concurrency limiter state: caps the number of simultaneous
/// requests hitting protected (wallet/staking/DEX) endpoints.  This prevents
/// resource exhaustion and slows brute-force passphrase guessing.
type ConcurrencyLimiter = tokio::sync::Semaphore;

/// Reject requests when too many protected endpoints are in flight.
///
/// Uses a bounded semaphore (5 permits).  When all permits are held, new
/// requests receive HTTP 429 (Too Many Requests).  This is not a per-IP rate
/// limiter — it's a global concurrency cap that prevents the daemon from
/// being overwhelmed and slows brute-force attacks.
async fn rate_limit(
    State(state): State<Arc<ConcurrencyLimiter>>,
    request: Request,
    next: Next,
) -> Result<Response, axum::http::StatusCode> {
    match state.try_acquire_owned() {
        Ok(_permit) => Ok(next.run(request).await),
        Err(_) => Err(axum::http::StatusCode::TOO_MANY_REQUESTS),
    }
}

/// Build the Axum router with all API routes.
pub fn build_router(state: AppState) -> Router {
    let state = Arc::new(state);
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost".parse().unwrap(),
            "http://127.0.0.1".parse().unwrap(),
            "tauri://localhost".parse().unwrap(),
            "http://tauri.localhost".parse().unwrap(),
        ])
        .allow_methods(Any)
        .allow_headers(Any);

    let auth = middleware::from_fn_with_state(Arc::clone(&state), require_api_key);
    let limiter: Arc<ConcurrencyLimiter> = Arc::new(tokio::sync::Semaphore::new(5));
    let rate = middleware::from_fn_with_state(Arc::clone(&limiter), rate_limit);

    let protected = Router::new()
        .route("/api/v1/wallet/import", post(import_wallet))
        .route("/api/v1/wallet/send", post(send_vtr))
        .route("/api/v1/wallet/unlock", post(unlock_wallet))
        .route("/api/v1/wallet/lock", post(lock_wallet))
        .route("/api/v1/staking/start", post(start_staking))
        .route("/api/v1/staking/stop", post(stop_staking))
        .route("/api/v1/torrent/add", post(add_torrent))
        .route("/api/v1/torrent/:id", delete(remove_torrent))
        .route("/api/v1/dex/order", post(place_dex_order))
        .route("/api/v1/dex/order/:id", delete(cancel_dex_order))
        .route("/api/v1/dex/match", post(match_dex_order))
        .route("/api/v1/swap/btc-fund", post(btc_fund))
        .route("/api/v1/swap/vtr-claim", post(vtr_claim))
        .route("/api/v1/swap/btc-claim", post(btc_claim))
        .route("/api/v1/swap/refund", post(swap_refund))
        .route("/api/v1/blockchain/broadcast", post(broadcast_transaction))
        .route("/api/v1/claim/submit", post(submit_claim))
        .route("/api/v1/spv/headers", post(add_spv_headers))
        .route("/api/v1/btc/send", post(send_btc))
        .layer(auth)
        .layer(rate);

    let public_routes = Router::new()
        .route("/api/v1/wallet/balance", get(get_balance))
        .route("/api/v1/wallet/addresses", get(get_addresses))
        .route("/api/v1/wallet/utxos", get(get_wallet_utxos))
        .route("/api/v1/wallet/transactions", get(get_transactions))
        .route("/api/v1/blockchain/utxo/:txid/:vout", get(get_txout))
        .route("/api/v1/staking/status", get(get_staking_status))
        .route("/api/v1/torrent/sessions", get(list_torrent_sessions))
        .route("/api/v1/dex/orders", get(get_dex_orders))
        .route("/api/v1/claim/check", post(check_claim))
        .route("/api/v1/faucet", post(faucet))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state.rate_limiter),
            ip_rate_limit,
        ));

    Router::new()
        .route("/api/v1/info", get(get_node_info))
        .route("/api/v1/blockchain/height", get(get_block_height))
        .route(
            "/api/v1/blockchain/block/height/:height",
            get(get_block_by_height),
        )
        .route("/api/v1/blockchain/block/:hash", get(get_block_by_hash))
        .route("/api/v1/blockchain/tx/:txid", get(get_transaction_by_id))
        .route("/api/v1/mempool", get(get_mempool))
        .route("/api/v1/fee/estimate", get(get_fee_estimate))
        .route("/api/v1/btc/status", get(get_btc_status))
        .route("/api/v1/btc/address", get(get_btc_address))
        .route(
            "/api/v1/debug/order/:id/preimage",
            get(debug_order_preimage),
        )
        .route("/api/v1/debug/mocktime", post(debug_mocktime))
        .route("/api/v1/spv/status", get(get_spv_status))
        .route("/api/v1/peers", get(get_peers))
        .route("/ws", get(ws_handler))
        .route("/metrics", get(metrics_handler))
        .merge(public_routes)
        .merge(protected)
        .layer(cors)
        .with_state(state)
}

/// Start the RPC server on the given address.
pub async fn start_server(
    bind_addr: &str,
    state: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!("vTorrent RPC server listening on {}", bind_addr);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod server_tests;
