#[test]
fn min_absolute_fee_enforced() {
    use vtorrent_node::chain::Utxo;
    use vtorrent_wallet::tx_builder::TxBuilder;
    let script = vec![0x51u8];
    let utxo = Utxo {
        txid: [9u8; 32],
        vout: 0,
        value: 50_000_000_000,
        script_pubkey: script,
        timestamp: 0,
        height: 1,
    };
    let wif = "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS";
    let tx = TxBuilder::new()
        .recipient("VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh", 100_000_000)
        .change_address("VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k")
        .fee_rate(1)
        .min_absolute_fee(vtorrent_wallet::tx_builder::MIN_ABSOLUTE_FEE_SATS)
        .sign_with_wif(wif)
        .build(&[utxo])
        .unwrap();
    let out_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
    println!("actual fee: {}", 50_000_000_000 - out_sum);
    assert!((50_000_000_000 - out_sum) >= 1000);
}
