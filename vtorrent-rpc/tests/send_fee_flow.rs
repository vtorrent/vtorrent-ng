use vtorrent_node::chain::Chain;
use vtorrent_node::mempool::Mempool;
use vtorrent_wallet::tx_builder::{TxBuilder, MIN_ABSOLUTE_FEE_SATS};

#[test]
fn send_path_fee_meets_relay_floor() {
    let mut chain = Chain::new().unwrap();
    let addr = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";
    chain.mint_to_address(addr, 50_000_000_000).unwrap();

    let utxos = chain.get_utxos_for_address(addr);
    let wif = "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS";
    let tx = TxBuilder::new()
        .recipient("VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh", 100_000_000)
        .change_address(addr)
        .fee_rate(1)
        .min_absolute_fee(MIN_ABSOLUTE_FEE_SATS)
        .sign_with_wif(wif)
        .build(&utxos)
        .unwrap();

    let real_fee = chain.compute_tx_fee(&tx).expect("inputs must resolve");
    println!("real fee: {}", real_fee);
    assert!(
        real_fee >= 1000,
        "actual fee {} below relay floor",
        real_fee
    );

    let mut mp = Mempool::new(10_000);
    mp.add_transaction_with_fee(tx, real_fee).unwrap();
}
