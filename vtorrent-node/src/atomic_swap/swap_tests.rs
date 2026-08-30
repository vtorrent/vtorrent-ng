use super::*;

fn make_htlc() -> Htlc {
    let preimage = [42u8; 32];
    let hash_lock = sha256(&preimage);
    Htlc::new(
        hash_lock,
        "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
        "VU3QSqAqM7tP3QXZ8sT7v8sQSdAxUZvqdS".to_string(),
        DEFAULT_HTLC_LOCKTIME,
        100_000_000, // 1 VTR
    )
    .unwrap()
}

#[test]
fn test_htlc_creation() {
    let htlc = make_htlc();
    assert_eq!(htlc.amount, 100_000_000);
    assert!(!htlc.is_expired());
    assert!(htlc.seconds_until_expiry() > 0);
}

#[test]
fn test_htlc_script_not_empty() {
    let htlc = make_htlc();
    let script = htlc.build_script().unwrap();
    assert!(!script.is_empty());
    assert_eq!(script[0], 0x63); // OP_IF
    assert_eq!(*script.last().unwrap(), 0x68); // OP_ENDIF
}

#[test]
fn test_htlc_script_contains_hash_lock() {
    let preimage = [42u8; 32];
    let hash_lock = sha256(&preimage);
    let htlc = Htlc::new(
        hash_lock,
        "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
        "VU3QSqAqM7tP3QXZ8sT7v8sQSdAxUZvqdS".to_string(),
        DEFAULT_HTLC_LOCKTIME,
        100_000_000,
    )
    .unwrap();
    let script = htlc.build_script().unwrap();
    // hash_lock should appear in the script
    let script_hex = hex::encode(&script);
    let hash_hex = hex::encode(hash_lock);
    assert!(script_hex.contains(&hash_hex));
}

#[test]
fn test_htlc_wrong_preimage_rejected() {
    let htlc = make_htlc();
    let wrong_preimage = [99u8; 32];
    let result = htlc.build_claim_tx([0u8; 32], &wrong_preimage, &[0u8; 33], &[0u8; 71], 1000);
    assert!(result.is_err());
}

#[test]
fn test_htlc_refund_before_expiry_rejected() {
    let htlc = make_htlc();
    let result = htlc.build_refund_tx([0u8; 32], &[0u8; 33], &[0u8; 71], 1000);
    assert!(result.is_err());
}

#[test]
fn test_htlc_funding_tx_insufficient_input() {
    let htlc = make_htlc();
    let result = htlc.build_funding_tx([0u8; 32], 0, 50_000, 1000);
    assert!(result.is_err());
}

#[test]
fn test_htlc_funding_tx_valid() {
    let htlc = make_htlc();
    let result = htlc.build_funding_tx([1u8; 32], 0, 200_000_000, 10_000);
    assert!(result.is_ok());
    let tx = result.unwrap();
    assert_eq!(tx.tx_type, TxType::AtomicSwap);
    assert_eq!(tx.outputs[0].value, 100_000_000);
    assert_eq!(tx.outputs[1].value, 200_000_000 - 100_000_000 - 10_000);
}

#[test]
fn test_swap_order_creation() {
    let order = SwapOrder::new(
        "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
        1_000_000_000, // 10 VTR
        "BTC".to_string(),
        100_000, // 0.001 BTC
        DEFAULT_HTLC_LOCKTIME,
    );
    assert_eq!(order.status, OrderStatus::Open);
    assert!(order.rate() > 0.0);
    assert!(!order.is_expired());
}

#[test]
fn test_funding_reservation_records_htlc_transaction() {
    let mut book = SwapOrderBook::new();
    let order = SwapOrder::new(
        "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
        1_000_000,
        "BTC".to_string(),
        100_000,
        DEFAULT_HTLC_LOCKTIME,
    );
    let order_id = hex::encode(order.order_id);
    book.add_order(order);

    assert!(book.begin_funding(&order_id).is_some());
    assert!(!book.cancel_order(&order_id));
    let matched = book
        .fund_and_match_order(
            &order_id,
            "VU3QSqAqM7tP3QXZ8sT7v8sQSdAxUZvqdS".to_string(),
            [7u8; 32],
            [8u8; 32],
            [9u8; 32],
        )
        .expect("funding reservation should complete");
    assert_eq!(matched.order.status, OrderStatus::Matched);
    assert_eq!(matched.order.funding_txid, Some([9u8; 32]));
    assert_eq!(matched.order.hash_lock, Some([8u8; 32]));
}

#[test]
fn test_swap_order_rate() {
    let order = SwapOrder::new(
        "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
        1_000_000_000, // 10 VTR
        "BTC".to_string(),
        1_000_000, // 0.01 BTC
        DEFAULT_HTLC_LOCKTIME,
    );
    // rate = 1_000_000 / 1_000_000_000 = 0.001
    assert!((order.rate() - 0.001).abs() < 1e-9);
}

#[test]
fn test_swap_state_transitions() {
    let mut state = SwapState::new([1u8; 32], [2u8; 32]);
    assert_eq!(state.status, SwapStatus::Funding);

    state.vtr_funding_txid = Some([3u8; 32]);
    state.status = SwapStatus::VtrFunded;
    assert_eq!(state.status, SwapStatus::VtrFunded);

    state.btc_funding_txid = Some([4u8; 32]);
    state.status = SwapStatus::BtcFunded;
    assert_eq!(state.status, SwapStatus::BtcFunded);

    state.status = SwapStatus::Claimed;
    assert_eq!(state.status, SwapStatus::Claimed);
}

#[test]
fn test_order_announcement_roundtrip() {
    let order = SwapOrder::new(
        "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
        1_000_000_000,
        "BTC".to_string(),
        100_000,
        DEFAULT_HTLC_LOCKTIME,
    );
    let ann = OrderAnnouncement::from_order(&order);
    let json = serde_json::to_string(&ann).unwrap();
    let back: OrderAnnouncement = serde_json::from_str(&json).unwrap();
    assert_eq!(back.order_id, ann.order_id);
    assert_eq!(back.maker_address, ann.maker_address);
    assert_eq!(back.vtr_amount, ann.vtr_amount);
    assert_eq!(back.target_asset, ann.target_asset);
    assert_eq!(back.target_amount, ann.target_amount);
    assert_eq!(back.expiry, ann.expiry);
}

#[test]
fn test_order_announcement_excludes_preimage() {
    let mut order = SwapOrder::new(
        "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
        1_000_000_000,
        "BTC".to_string(),
        100_000,
        DEFAULT_HTLC_LOCKTIME,
    );
    order.preimage = Some([7u8; 32]);
    order.funding_txid = Some([9u8; 32]);
    let ann = OrderAnnouncement::from_order(&order);
    let json = serde_json::to_string(&ann).unwrap();
    assert!(!json.contains("preimage"));
    assert!(!json.contains("funding_txid"));
}

impl SwapOrder {
    fn is_expired(&self) -> bool {
        let now = now_timestamp_u32();
        now >= self.expiry
    }
}
