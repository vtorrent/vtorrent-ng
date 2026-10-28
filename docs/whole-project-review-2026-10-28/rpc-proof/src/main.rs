use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bitcoin::{
    consensus::{deserialize, serialize},
    p2p::message::{NetworkMessage, RawNetworkMessage},
    Network,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;
use vtorrent_btc::{keys::derive_address, utxo::Utxo, wallet::BtcWallet};
use vtorrent_rpc::{server::build_router, state::AppState};

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut sender = Some(tx);
        loop {
            let mut header = [0u8; 24];
            if stream.read_exact(&mut header).await.is_err() {
                break;
            }
            let len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
            assert!(len < 1024 * 1024);
            let mut raw = header.to_vec();
            raw.resize(24 + len, 0);
            stream.read_exact(&mut raw[24..]).await.unwrap();
            let msg: RawNetworkMessage = deserialize(&raw).unwrap();
            match msg.payload() {
                NetworkMessage::Version(version) => {
                    for msg in [
                        NetworkMessage::Version(version.clone()),
                        NetworkMessage::Verack,
                    ] {
                        stream
                            .write_all(&serialize(&RawNetworkMessage::new(
                                Network::Regtest.magic(),
                                msg,
                            )))
                            .await
                            .unwrap();
                    }
                }
                NetworkMessage::Tx(transaction) => {
                    sender.take().unwrap().send(transaction.clone()).unwrap();
                    tokio::time::sleep(std::time::Duration::from_secs(11)).await;
                    break;
                }
                _ => {}
            }
        }
    });
    let seed = [21u8; 64];
    let recipient = derive_address(&[22u8; 64], 0, Network::Regtest).unwrap();
    let wallet = BtcWallet::with_network(seed, Network::Regtest);
    wallet.add_utxo(Utxo {
        txid: "09".repeat(32),
        vout: 0,
        value: 100_000,
        address: derive_address(&seed, 0, Network::Regtest).unwrap(),
        height: 1,
    });
    let state = AppState::new();
    assert!(state.rpc_api_key.is_none());
    *state.btc_wallet.write().await = Some(wallet);
    *state.btc_network.write().await = Network::Regtest;
    *state.btc_peer.write().await = Some(address.to_string());
    let app = build_router(state);
    let import = Request::post("/api/v1/wallet/import")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"wif":"WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS","passphrase":"proof-pass"})
                .to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(import).await.unwrap().status(),
        StatusCode::OK
    );
    let unlock = Request::post("/api/v1/wallet/unlock")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"passphrase":"proof-pass","timeout_secs":300}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(unlock).await.unwrap().status(),
        StatusCode::OK
    );
    let unauthenticated = Request::post("/api/v1/btc/send")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"to_address":recipient,"amount_satoshis":50000,"fee_satoshis":1000})
                .to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(unauthenticated).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let authenticated = Request::post("/api/v1/btc/send")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"to_address":recipient,"amount_satoshis":50000,"fee_satoshis":1000,"passphrase":"proof-pass"})
                .to_string(),
        ))
        .unwrap();
    let response = app.oneshot(authenticated).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let signed = rx.await.unwrap();
    assert!(!signed.input[0].witness.is_empty());
    println!("No-passphrase BTC send was rejected; authenticated send delivered a signed transaction to the local mock peer");
    peer.abort();
}
