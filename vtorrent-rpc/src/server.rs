use std::sync::Arc;
use axum::{
    routing::{delete, get, post},
    Router,
};
use tower_http::cors::{Any, CorsLayer};
use crate::handlers::*;
use crate::state::AppState;
use crate::ws::ws_handler;
use crate::metrics::metrics_handler;

/// Default RPC port — same as legacy vTorrent RPC port + 1.
pub const DEFAULT_RPC_PORT: u16 = 22525;

/// Build the Axum router with all API routes.
pub fn build_router(state: AppState) -> Router {
    let state = Arc::new(state);
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // Node info
        .route("/api/v1/info", get(get_node_info))
        // Blockchain
        .route("/api/v1/blockchain/height", get(get_block_height))
        .route("/api/v1/blockchain/block/:hash", get(get_block_by_hash))
        .route("/api/v1/mempool", get(get_mempool))
        // Wallet
        .route("/api/v1/wallet/balance", get(get_balance))
        .route("/api/v1/wallet/addresses", get(get_addresses))
        .route("/api/v1/wallet/send", post(send_vtr))
        .route("/api/v1/wallet/unlock", post(unlock_wallet))
        .route("/api/v1/wallet/lock", post(lock_wallet))
        // Staking
        .route("/api/v1/staking/status", get(get_staking_status))
        .route("/api/v1/staking/start", post(start_staking))
        .route("/api/v1/staking/stop", post(stop_staking))
        // Torrent
        .route("/api/v1/torrent/sessions", get(list_torrent_sessions))
        .route("/api/v1/torrent/add", post(add_torrent))
        .route("/api/v1/torrent/:id", delete(remove_torrent))
        // DEX
        .route("/api/v1/dex/orders", get(get_dex_orders))
        .route("/api/v1/dex/order", post(place_dex_order))
        .route("/api/v1/dex/order/:id", delete(cancel_dex_order))
        // Legacy claim
        .route("/api/v1/claim/check", post(check_claim))
        .route("/api/v1/claim/submit", post(submit_claim))
        // WebSocket event stream
        .route("/ws", get(ws_handler))
        // Prometheus metrics
        .route("/metrics", get(metrics_handler))
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
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt; // for `oneshot`

    async fn get(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    async fn post_json(app: axum::Router, uri: &str, payload: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn test_get_node_info() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/info").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["network"], "vtorrent-mainnet");
        assert!(body["block_height"].is_number());
    }

    #[tokio::test]
    async fn test_get_block_height() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/blockchain/height").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["height"].is_number());
    }

    #[tokio::test]
    async fn test_get_mempool() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/mempool").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["count"], 0);
    }

    #[tokio::test]
    async fn test_wallet_lock_unlock() {
        let app = build_router(AppState::new());
        let (status, body) = post_json(
            app,
            "/api/v1/wallet/unlock",
            serde_json::json!({ "passphrase": "testpassphrase", "timeout_secs": 300 }),
        ).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
    }

    #[tokio::test]
    async fn test_send_requires_unlock() {
        let app = build_router(AppState::new());
        let (status, _body) = post_json(
            app,
            "/api/v1/wallet/send",
            serde_json::json!({
                "to_address": "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT",
                "amount_satoshis": 1000000,
                "passphrase": "test"
            }),
        ).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_staking_status() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/staking/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], false);
    }

    #[tokio::test]
    async fn test_list_torrent_sessions_empty() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/torrent/sessions").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_dex_orders_empty() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/dex/orders").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_block_not_found() {
        let app = build_router(AppState::new());
        let (status, _body) = get(
            app,
            "/api/v1/blockchain/block/deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
