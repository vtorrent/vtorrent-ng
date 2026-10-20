use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use tower::ServiceExt;

#[tokio::test]
async fn audit_alert_dependencies_are_exported() {
    let router = vtorrent_rpc::server::build_router(vtorrent_rpc::state::AppState::new());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    for name in [
        "vtorrent_best_peer_height",
        "vtorrent_process_rss_bytes",
        "vtorrent_btc_synced",
    ] {
        assert!(
            body.lines()
                .any(|line| line.starts_with(&format!("{name} "))),
            "alert dependency {name} is not exported"
        );
    }
}

#[test]
fn audit_blocktxn_envelope_fits_the_wire_limit() {
    use vtorrent_node::block::{Transaction, TxInput, TxOutput, TxType};
    let transaction = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: [0x42; 32],
            prev_vout: 0,
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        outputs: (0..1_000)
            .map(|_| TxOutput {
                value: 1,
                script_pubkey: vec![0x51; 520],
            })
            .collect(),
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    vtorrent_node::consensus::validate_transaction(&transaction).unwrap();
    assert!(transaction.serialized_size() < vtorrent_node::consensus::MAX_BLOCK_SIZE);
    let raw = serde_json::to_vec(&transaction).unwrap();
    assert!(raw.len() < 4 * 1024 * 1024);
    let response = vtorrent_p2p::message::BlockTxnMsg {
        block_hash: [0; 32],
        transactions: vec![raw],
    };
    let payload = serde_json::to_vec(&response).unwrap();
    assert!(
        payload.len() <= vtorrent_p2p::message::MAX_PAYLOAD_SIZE as usize,
        "inner-budget-compliant response expands to {} bytes on wire",
        payload.len()
    );
}
