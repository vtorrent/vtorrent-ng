use super::*;
use bitcoin::bip158::{BlockFilter, FilterHeader};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::p2p::message::RawNetworkMessage;
use bitcoin::p2p::message_filter::{CFCheckpt, CFHeaders, CFilter};
use bitcoin::{Amount, Block, OutPoint, ScriptBuf, Transaction, TxIn, TxOut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const NOW: u64 = 1_800_000_000;

fn contract() -> crate::htlc::BtcHtlc {
    crate::htlc::BtcHtlc {
        hash_lock: [42; 32],
        recipient: crate::keys::derive_address(&[1; 64], 0, bitcoin::Network::Regtest).unwrap(),
        refund_address: crate::keys::derive_address(&[2; 64], 0, bitcoin::Network::Regtest)
            .unwrap(),
        expiry: NOW as u32 + 86_400,
        amount: 100_000,
        network: bitcoin::Network::Regtest,
    }
}

fn chain(htlc: &crate::htlc::BtcHtlc, case: &str) -> (Vec<Block>, [u8; 32]) {
    let mut funding = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(bitcoin::Txid::from_byte_array([1; 32]), 0),
            ..Default::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(htlc.amount),
            script_pubkey: htlc.build_script().unwrap().to_p2wsh(),
        }],
    };
    match case {
        "wallet_spent" => {
            funding.output[0].script_pubkey = htlc
                .recipient
                .parse::<bitcoin::Address<_>>()
                .unwrap()
                .require_network(bitcoin::Network::Regtest)
                .unwrap()
                .script_pubkey();
        }
        "amount" => funding.output[0].value = Amount::from_sat(htlc.amount - 1),
        "vout" => funding.output.insert(
            0,
            TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::new(),
            },
        ),
        "expiry" | "recipient" | "refund" | "hash" => {
            let mut wrong = htlc.clone();
            match case {
                "expiry" => wrong.expiry += 1,
                "recipient" => wrong.recipient = wrong.refund_address.clone(),
                "refund" => wrong.refund_address = wrong.recipient.clone(),
                _ => wrong.hash_lock[0] ^= 1,
            }
            funding.output[0].script_pubkey = wrong.build_script().unwrap().to_p2wsh();
        }
        "coinbase" => funding.input[0].previous_output = OutPoint::null(),
        _ => {}
    }
    let txid = funding.compute_txid();
    let mut blocks = vec![bitcoin::blockdata::constants::genesis_block(
        bitcoin::Network::Regtest,
    )];
    let count = if case == "shallow" { 5 } else { 6 };
    for height in 1..=count {
        let mut coinbase = blocks[0].txdata[0].clone();
        coinbase.lock_time = bitcoin::absolute::LockTime::from_height(height).unwrap();
        let mut txdata = vec![coinbase];
        if height == 1 {
            txdata.push(funding.clone());
        }
        if height == 6 && matches!(case, "spent" | "wallet_spent") {
            txdata.push(Transaction {
                version: bitcoin::transaction::Version::TWO,
                lock_time: bitcoin::absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::new(txid, 0),
                    ..Default::default()
                }],
                output: vec![TxOut {
                    value: Amount::from_sat(99_000),
                    script_pubkey: ScriptBuf::new(),
                }],
            });
        }
        let mut block = Block {
            header: bitcoin::block::Header {
                version: bitcoin::block::Version::TWO,
                prev_blockhash: blocks.last().unwrap().block_hash(),
                merkle_root: bitcoin::TxMerkleNode::all_zeros(),
                time: NOW as u32 - if case == "chain_time" { 0 } else { 600 } + height,
                bits: blocks[0].header.bits,
                nonce: 0,
            },
            txdata,
        };
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        while block.header.validate_pow(block.header.target()).is_err() {
            block.header.nonce += 1;
        }
        blocks.push(block);
    }
    (
        blocks,
        if case == "missing" {
            [9; 32]
        } else {
            txid.to_byte_array()
        },
    )
}

async fn send(stream: &mut TcpStream, message: NetworkMessage) -> std::io::Result<()> {
    stream
        .write_all(&serialize(&RawNetworkMessage::new(
            bitcoin::Network::Regtest.magic(),
            message,
        )))
        .await
}

async fn recv(stream: &mut TcpStream) -> std::io::Result<NetworkMessage> {
    let mut header = [0; 24];
    stream.read_exact(&mut header).await?;
    let len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    assert!(len < 4_000_000);
    let mut raw = header.to_vec();
    raw.resize(24 + len, 0);
    stream.read_exact(&mut raw[24..]).await?;
    Ok(deserialize::<RawNetworkMessage>(&raw)
        .unwrap()
        .payload()
        .clone())
}

async fn stall_response(stream: &mut TcpStream, chatty: bool) {
    if !chatty {
        let _ = recv(stream).await;
        return;
    }
    while send(stream, NetworkMessage::Ping(1)).await.is_ok() {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn serve(listener: TcpListener, blocks: Vec<Block>, case: &'static str) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let script = if case.starts_with("wallet_") {
        contract()
            .recipient
            .parse::<bitcoin::Address<_>>()
            .unwrap()
            .require_network(bitcoin::Network::Regtest)
            .unwrap()
            .script_pubkey()
    } else {
        contract().build_script().unwrap().to_p2wsh()
    };
    let filters: Vec<_> = blocks
        .iter()
        .map(|b| BlockFilter::new_script_filter(b, |_| Ok(script.clone())).unwrap())
        .collect();
    while let Ok(message) = recv(&mut stream).await {
        let response = match message {
            NetworkMessage::Version(mut version) => {
                version.services |= bitcoin::p2p::ServiceFlags::COMPACT_FILTERS;
                send(&mut stream, NetworkMessage::Version(version))
                    .await
                    .unwrap();
                NetworkMessage::Verack
            }
            NetworkMessage::GetHeaders(req) => {
                if matches!(case, "silent_headers" | "chatty_headers") {
                    stall_response(&mut stream, case == "chatty_headers").await;
                    return;
                }
                if case == "header_disconnect" {
                    break;
                }
                let common = req
                    .locator_hashes
                    .iter()
                    .find_map(|hash| blocks.iter().position(|b| b.block_hash() == *hash))
                    .unwrap_or(0);
                let mut headers: Vec<_> = blocks[common + 1..].iter().map(|b| b.header).collect();
                if case == "malformed_suffix" {
                    let mut orphan = blocks.last().unwrap().header;
                    orphan.prev_blockhash = bitcoin::BlockHash::all_zeros();
                    headers.push(orphan);
                }
                NetworkMessage::Headers(headers)
            }
            NetworkMessage::GetCFCheckpt(req) => {
                if case == "checkpoint_chatty" {
                    stall_response(&mut stream, true).await;
                    return;
                }
                if case == "checkpoint_disconnect" {
                    break;
                }
                NetworkMessage::CFCheckpt(CFCheckpt {
                    filter_type: 0,
                    stop_hash: req.stop_hash,
                    filter_headers: vec![],
                })
            }
            NetworkMessage::GetCFHeaders(req) => {
                if case == "cfheaders_chatty" {
                    stall_response(&mut stream, true).await;
                    return;
                }
                let start = req.start_height as usize;
                let end = blocks
                    .iter()
                    .position(|block| block.block_hash() == req.stop_hash)
                    .unwrap();
                let previous = filters[..start]
                    .iter()
                    .fold(FilterHeader::all_zeros(), |header, filter| {
                        filter.filter_header(&header)
                    });
                NetworkMessage::CFHeaders(CFHeaders {
                    filter_type: 0,
                    stop_hash: req.stop_hash,
                    previous_filter_header: previous,
                    filter_hashes: filters[start..=end]
                        .iter()
                        .map(|f| bitcoin::bip158::FilterHash::hash(&f.content))
                        .collect(),
                })
            }
            NetworkMessage::GetCFilters(req) => {
                if case == "filters_chatty" {
                    stall_response(&mut stream, true).await;
                    return;
                }
                if case == "disconnect" {
                    break;
                }
                for height in req.start_height as usize..blocks.len() {
                    if case == "wallet_partial" && height > 1 {
                        break;
                    }
                    if case == "partial_disconnect" && height > 0 {
                        return;
                    }
                    let mut filter = filters[height].content.clone();
                    if case == "filter" {
                        filter.push(0);
                    }
                    if send(
                        &mut stream,
                        NetworkMessage::CFilter(CFilter {
                            filter_type: 0,
                            block_hash: blocks[height].block_hash(),
                            filter,
                        }),
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                }
                continue;
            }
            NetworkMessage::GetData(inventory) => {
                if case == "block_chatty" {
                    stall_response(&mut stream, true).await;
                    return;
                }
                let Inventory::WitnessBlock(hash) = inventory[0] else {
                    panic!("expected block request")
                };
                let mut block = blocks
                    .iter()
                    .find(|b| b.block_hash() == hash)
                    .unwrap()
                    .clone();
                if case == "merkle" {
                    block.txdata[1].output[0].value = Amount::ZERO;
                }
                if case == "wallet_partial" {
                    send(&mut stream, NetworkMessage::Block(block))
                        .await
                        .unwrap();
                    return;
                }
                NetworkMessage::Block(block)
            }
            _ => continue,
        };
        if send(&mut stream, response).await.is_err() {
            break;
        }
    }
}

#[tokio::test]
async fn header_sync_removes_failed_peer_and_uses_healthy_peer() {
    let (blocks, _) = chain(&contract(), "valid");
    let mut peers = Vec::new();
    let mut servers = Vec::new();
    for case in ["header_disconnect", "valid"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        servers.push(tokio::spawn(serve(listener, blocks.clone(), case)));
        peers.push(
            BtcPeer::connect_with_network(addr, bitcoin::Network::Regtest)
                .await
                .unwrap(),
        );
    }
    let healthy = peers[1].addr();
    let mut wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
    let added = wallet.sync_with_peers(&mut peers).await.unwrap();
    assert_eq!(added, 6);
    assert_eq!(wallet.best_height(), 6);
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].addr(), healthy);
    drop(peers);
    for server in servers {
        server.await.unwrap();
    }
}

#[tokio::test]
async fn interrupted_header_reorg_does_not_commit_partial_state() {
    let htlc = contract();
    let (fork, _) = chain(&htlc, "valid");
    let valid_fork = fork.clone();
    let mut old = fork[..2].to_vec();
    old[1].header.time += 100;
    old[1].header.nonce = 0;
    while old[1].header.validate_pow(old[1].header.target()).is_err() {
        old[1].header.nonce += 1;
    }
    let mut wallet = crate::wallet::BtcWallet::with_network([1; 64], htlc.network);
    wallet.add_header(&serialize(&old[1].header), 1).unwrap();
    wallet.add_utxo(Utxo {
        txid: hex::encode([9; 32]),
        vout: 0,
        value: 100_000,
        address: htlc.address().unwrap(),
        height: 1,
    });
    wallet.set_last_scanned_height(2);
    let mut peers = Vec::new();
    let mut servers = Vec::new();
    for (blocks, case) in [(fork, "malformed_suffix"), (old, "valid")] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        servers.push(tokio::spawn(serve(listener, blocks, case)));
        peers.push(
            BtcPeer::connect_with_network(addr, htlc.network)
                .await
                .unwrap(),
        );
    }
    wallet.sync_with_peers(&mut peers).await.unwrap();
    assert_eq!(wallet.best_height(), 1);
    assert_eq!(wallet.balance(), 100_000);
    assert_eq!(wallet.last_scanned_height(), 2);
    drop(peers);
    for server in servers {
        server.await.unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(listener, valid_fork, "valid"));
    let mut peer = BtcPeer::connect_with_network(addr, htlc.network)
        .await
        .unwrap();
    wallet.sync(&mut peer).await.unwrap();
    assert_eq!(wallet.best_height(), 6);
    assert_eq!(wallet.balance(), 0);
    assert_eq!(wallet.last_scanned_height(), 0);
    drop(peer);
    server.await.unwrap();
}

#[tokio::test]
async fn restored_chain_cache_is_rebuilt_after_first_header_sync() {
    let path = std::env::temp_dir().join(format!(
        "btc-restored-cache-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let mut stored = UtxoSet::new();
    stored.add(Utxo {
        txid: hex::encode([9; 32]),
        vout: 0,
        value: 100_000,
        address: crate::keys::derive_address(&[1; 64], 0, bitcoin::Network::Regtest).unwrap(),
        height: 1,
    });
    stored.set_last_scanned_height(2);
    stored.set_next_index(1);
    stored.save(&path).unwrap();
    let mut wallet = crate::wallet::BtcWallet::with_persistence(
        [1; 64],
        bitcoin::Network::Regtest,
        path.clone(),
    )
    .unwrap();
    assert_eq!(wallet.balance(), 0);
    assert!(wallet.list_utxos().is_empty());
    wallet.add_utxo(Utxo {
        txid: hex::encode([9; 32]),
        vout: 0,
        value: 100_000,
        address: crate::keys::derive_address(&[1; 64], 0, bitcoin::Network::Regtest).unwrap(),
        height: 1,
    });
    assert_eq!(
        wallet.balance(),
        0,
        "manual insertion must not unlock restored outputs"
    );
    let recipient = crate::keys::derive_address(&[2; 64], 0, bitcoin::Network::Regtest).unwrap();
    assert!(
        wallet.send_to(&recipient, 1_000, 1_000).is_err(),
        "restored outputs must not be spendable before rescan"
    );
    let (blocks, _) = chain(&contract(), "valid");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(listener, blocks, "valid"));
    let mut peer = BtcPeer::connect_with_network(addr, bitcoin::Network::Regtest)
        .await
        .unwrap();
    wallet.sync(&mut peer).await.unwrap();
    assert_eq!(wallet.best_height(), 6);
    assert_eq!(
        wallet.balance(),
        0,
        "a restored cache has no persisted header lineage"
    );
    assert_eq!(wallet.last_scanned_height(), 0);
    assert_eq!(UtxoSet::load(&path).unwrap().next_index(), 1);
    assert_eq!(UtxoSet::load(&path).unwrap().last_scanned_height(), 0);
    let pending = crate::wallet::BtcWallet::with_persistence(
        [1; 64],
        bitcoin::Network::Regtest,
        path.clone(),
    )
    .unwrap();
    pending.add_utxo(Utxo {
        txid: hex::encode([12; 32]),
        vout: 0,
        value: 100_000,
        address: crate::keys::derive_address(&[1; 64], 0, bitcoin::Network::Regtest).unwrap(),
        height: 1,
    });
    assert!(
        pending.list_utxos().is_empty(),
        "a restart immediately after header sync must not unlock the wallet"
    );
    drop(pending);
    wallet.set_last_scanned_height(2);
    wallet.add_utxo(Utxo {
        txid: hex::encode([10; 32]),
        vout: 0,
        value: 100_000,
        address: crate::keys::derive_address(&[1; 64], 0, bitcoin::Network::Regtest).unwrap(),
        height: 1,
    });
    assert!(
        wallet.list_utxos().is_empty(),
        "a partial or manually advanced cursor must not unlock the wallet"
    );
    drop(peer);
    server.await.unwrap();
    drop(wallet);
    let mut wallet = crate::wallet::BtcWallet::with_persistence(
        [1; 64],
        bitcoin::Network::Regtest,
        path.clone(),
    )
    .unwrap();
    assert!(
        wallet.list_utxos().is_empty(),
        "restart during rescan must remain locked"
    );
    let (blocks, _) = chain(&contract(), "valid");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(listener, blocks, "valid"));
    let mut peer = BtcPeer::connect_with_network(addr, bitcoin::Network::Regtest)
        .await
        .unwrap();
    wallet.sync(&mut peer).await.unwrap();
    let scanned = wallet
        .scan_utxos_bip158_multi(std::slice::from_mut(&mut peer), 0)
        .await
        .unwrap();
    assert_eq!(scanned, 7);
    wallet.set_last_scanned_height(scanned as u32);
    wallet.add_utxo(Utxo {
        txid: hex::encode([11; 32]),
        vout: 0,
        value: 100_000,
        address: crate::keys::derive_address(&[1; 64], 0, bitcoin::Network::Regtest).unwrap(),
        height: 1,
    });
    assert_eq!(
        wallet.balance(),
        100_000,
        "a complete authenticated scan unlocks the wallet"
    );
    drop(peer);
    server.await.unwrap();
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn partial_rescan_keeps_later_spent_output_unavailable_across_restart() {
    let path = std::env::temp_dir().join(format!(
        "btc-partial-rescan-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut stored = UtxoSet::new();
    stored.set_last_scanned_height(2);
    stored.save(&path).unwrap();
    let mut wallet = crate::wallet::BtcWallet::with_persistence(
        [1; 64],
        bitcoin::Network::Regtest,
        path.clone(),
    )
    .unwrap();
    let (blocks, _) = chain(&contract(), "wallet_spent");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(listener, blocks.clone(), "wallet_partial"));
    let mut peer = BtcPeer::connect_with_network(addr, bitcoin::Network::Regtest)
        .await
        .unwrap();
    wallet.sync(&mut peer).await.unwrap();
    let scanned = wallet
        .scan_utxos_bip158_multi(std::slice::from_mut(&mut peer), 0)
        .await
        .unwrap();
    assert_eq!(scanned, 2);
    wallet.set_last_scanned_height(scanned as u32);
    assert_eq!(wallet.balance(), 0);
    assert!(wallet.list_utxos().is_empty());
    assert!(wallet.send_to(&contract().recipient, 1_000, 1_000).is_err());
    drop(peer);
    server.await.unwrap();
    drop(wallet);
    let mut wallet = crate::wallet::BtcWallet::with_persistence(
        [1; 64],
        bitcoin::Network::Regtest,
        path.clone(),
    )
    .unwrap();
    assert_eq!(wallet.balance(), 0);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(listener, blocks, "wallet_valid"));
    let mut peer = BtcPeer::connect_with_network(addr, bitcoin::Network::Regtest)
        .await
        .unwrap();
    wallet.sync(&mut peer).await.unwrap();
    let scanned = wallet
        .scan_utxos_bip158_multi(std::slice::from_mut(&mut peer), 0)
        .await
        .unwrap();
    wallet.set_last_scanned_height(scanned as u32);
    assert_eq!(
        wallet.balance(),
        0,
        "later block spent the discovered output"
    );
    assert!(wallet.list_utxos().is_empty());
    drop(peer);
    server.await.unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn scan_failover_case(failure: &'static str) {
    let htlc = contract();
    let (blocks, txid) = chain(&htlc, "valid");
    let headers = Arc::new(Mutex::new(HeaderChain::anchored(bitcoin::Network::Regtest)));
    for (height, block) in blocks.iter().enumerate().skip(1) {
        headers
            .lock()
            .add_header(&serialize(&block.header), height as u32)
            .unwrap();
    }
    let utxos = Arc::new(Mutex::new(UtxoSet::new()));
    let sync = BtcSync::new(
        headers,
        utxos.clone(),
        vec![htlc.address().unwrap()],
        htlc.network,
    );
    let mut peers = Vec::new();
    let mut servers = Vec::new();
    for case in [failure, "valid"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        servers.push(tokio::spawn(serve(listener, blocks.clone(), case)));
        peers.push(
            BtcPeer::connect_with_network(addr, htlc.network)
                .await
                .unwrap(),
        );
    }
    assert!(peers.iter().all(|peer| !peer.compact_filters_validated()));
    let scanned = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        sync.scan_utxos_bip158_multi(&mut peers, 0),
    )
    .await
    .expect("peer chatter bypassed the request deadline")
    .unwrap();
    assert_eq!(scanned, 7, "failure: {failure}");
    assert!(peers[1].compact_filters_validated());
    if matches!(
        failure,
        "checkpoint_disconnect" | "checkpoint_chatty" | "cfheaders_chatty"
    ) {
        assert!(!peers[0].compact_filters_validated());
    }
    {
        let utxos = utxos.lock();
        let outputs = utxos.list();
        assert_eq!(outputs.len(), 1, "failure: {failure}");
        assert_eq!(
            outputs[0].txid,
            bitcoin::Txid::from_byte_array(txid).to_string()
        );
        assert_eq!(outputs[0].value, htlc.amount);
    }
    drop(peers);
    for server in servers {
        server.await.unwrap();
    }
}

#[tokio::test]
async fn multi_peer_scan_fails_over_authentication_and_filter_downloads() {
    for failure in ["checkpoint_disconnect", "disconnect", "partial_disconnect"] {
        scan_failover_case(failure).await;
    }
}

#[tokio::test]
async fn peer_chatter_cannot_extend_filter_or_block_deadlines() {
    tokio::join!(
        scan_failover_case("checkpoint_chatty"),
        scan_failover_case("cfheaders_chatty"),
        scan_failover_case("filters_chatty"),
        scan_failover_case("block_chatty"),
    );
}

#[tokio::test]
async fn silent_and_chatty_header_peers_reach_healthy_fallback() {
    async fn check(case: &'static str) {
        let (blocks, _) = chain(&contract(), "valid");
        let mut peers = Vec::new();
        let mut servers = Vec::new();
        for behavior in [case, "valid"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            servers.push(tokio::spawn(serve(listener, blocks.clone(), behavior)));
            peers.push(
                BtcPeer::connect_with_network(addr, bitcoin::Network::Regtest)
                    .await
                    .unwrap(),
            );
        }
        let mut wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
        let added = tokio::time::timeout(
            std::time::Duration::from_secs(12),
            wallet.sync_with_peers(&mut peers),
        )
        .await
        .expect("header peer bypassed the request deadline")
        .unwrap();
        assert_eq!(added, 6);
        assert_eq!(peers.len(), 1);
        drop(peers);
        for server in servers {
            server.await.unwrap();
        }
    }
    tokio::join!(check("silent_headers"), check("chatty_headers"));
}

#[tokio::test]
async fn swap_verification_scans_committed_blocks_and_rejects_bad_funding() {
    for case in [
        "valid",
        "shallow",
        "spent",
        "amount",
        "vout",
        "expiry",
        "recipient",
        "refund",
        "hash",
        "missing",
        "coinbase",
        "filter",
        "merkle",
        "disconnect",
        "stale",
        "future",
        "chain_time",
        "deadline",
    ] {
        let mut htlc = contract();
        if case == "chain_time" {
            htlc.expiry = NOW as u32 + MIN_BTC_CLAIM_WINDOW + 1;
        }
        if case == "deadline" {
            htlc.expiry = NOW as u32 + MIN_BTC_CLAIM_WINDOW;
        }
        let (blocks, txid) = chain(&htlc, case);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, blocks, case));
        let wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
        // Persisted/display UTXOs must not satisfy the independent claim scan.
        wallet.add_utxo(Utxo {
            txid: bitcoin::Txid::from_byte_array(txid).to_string(),
            vout: 0,
            value: htlc.amount,
            address: htlc.address().unwrap(),
            height: 1,
        });
        let now = match case {
            "stale" => NOW + MAX_SWAP_TIP_AGE,
            "future" => NOW - MAX_SWAP_TIP_AGE - 601,
            _ => NOW,
        };
        let result = wallet.verify_swap_funding(&htlc, txid, &[addr], now).await;
        assert_eq!(result.is_ok(), case == "valid", "case {case}: {result:?}");
        assert_eq!(
            wallet.best_height(),
            0,
            "contract scans must not mutate wallet headers"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn mainnet_requires_distinct_peer_ips_before_scanning() {
    let mut htlc = contract();
    htlc.network = bitcoin::Network::Bitcoin;
    let sync = BtcSync::new(
        Arc::new(Mutex::new(HeaderChain::new())),
        Arc::new(Mutex::new(UtxoSet::new())),
        vec![],
        htlc.network,
    );
    let result = sync
        .verify_swap_funding(
            &htlc,
            [0; 32],
            &[
                "127.0.0.1:8333".parse().unwrap(),
                "127.0.0.1:8334".parse().unwrap(),
            ],
            NOW,
        )
        .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("requires 2 distinct"));
}

#[tokio::test]
async fn mainnet_rejects_same_network_group_peers() {
    // Two IPs in the same /16 are one operator; they must not count as two
    // independent peers, or a sybil could eclipse the scan (T-eclipse).
    let mut htlc = contract();
    htlc.network = bitcoin::Network::Bitcoin;
    let sync = BtcSync::new(
        Arc::new(Mutex::new(HeaderChain::new())),
        Arc::new(Mutex::new(UtxoSet::new())),
        vec![],
        htlc.network,
    );
    let result = sync
        .verify_swap_funding(
            &htlc,
            [0; 32],
            &[
                "203.0.113.10:8333".parse().unwrap(),
                "203.0.114.20:8333".parse().unwrap(),
            ],
            NOW,
        )
        .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("distinct compact-filter network groups"),
        "same-/16 peers must be rejected"
    );
}

#[test]
fn network_groups_bucket_by_prefix() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    // Same /16 -> same group.
    assert_eq!(
        network_group(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))),
        network_group(IpAddr::V4(Ipv4Addr::new(203, 0, 114, 2)))
    );
    // Different /16 -> different group.
    assert_ne!(
        network_group(IpAddr::V4(Ipv4Addr::new(203, 0, 1, 1))),
        network_group(IpAddr::V4(Ipv4Addr::new(203, 1, 1, 1)))
    );
    // IPv6 buckets by /32.
    assert_eq!(
        network_group(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1))),
        network_group(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2)))
    );
    assert_ne!(
        network_group(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1))),
        network_group(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb9, 0, 0, 0, 0, 0, 1)))
    );
    // v4 and v6 never share a group.
    assert_ne!(
        network_group(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))),
        network_group(IpAddr::V6(Ipv6Addr::UNSPECIFIED))
    );
}

#[test]
fn incomplete_scan_or_changed_tip_cannot_authorize_claim() {
    let htlc = contract();
    let (blocks, txid) = chain(&htlc, "valid");
    let headers = Arc::new(Mutex::new(HeaderChain::anchored(bitcoin::Network::Regtest)));
    let sync = BtcSync::new(
        headers.clone(),
        Arc::new(Mutex::new(UtxoSet::new())),
        vec![htlc.address().unwrap()],
        htlc.network,
    );
    for (height, block) in blocks.iter().enumerate().skip(1) {
        headers
            .lock()
            .add_header(&serialize(&block.header), height as u32)
            .unwrap();
        for tx in &block.txdata {
            sync.record_matching_outputs(tx, height as u32);
        }
    }
    let tip = blocks.last().unwrap().block_hash().to_byte_array();
    assert!(sync.verify_scanned_swap(&htlc, txid, 1, tip, 6).is_ok());
    assert!(sync.verify_scanned_swap(&htlc, txid, 1, tip, 5).is_err());
    assert!(sync
        .verify_scanned_swap(&htlc, txid, 1, [0; 32], 6)
        .is_err());
}

#[tokio::test]
async fn settlement_scan_keeps_spent_outputs_and_allows_expired_contracts() {
    for case in [
        "valid",
        "spent",
        "shallow",
        "coinbase",
        "amount",
        "missing",
        "expired",
        "filter",
        "merkle",
        "disconnect",
    ] {
        let mut htlc = contract();
        if case == "expired" {
            htlc.expiry = NOW as u32 - 1;
        }
        let (blocks, txid) = chain(&htlc, case);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, blocks, case));
        let wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
        let result = wallet.observe_swap(&htlc, txid, &[addr], NOW, &[]).await;
        if matches!(case, "filter" | "merkle" | "disconnect") {
            assert!(result.is_err(), "{case}");
        } else {
            let scan = result.unwrap();
            assert_eq!(scan.funding.is_some(), case != "missing", "{case}");
            assert_eq!(scan.spend.is_some(), case == "spent", "{case}");
            assert_eq!(scan.invalid_funding, case == "amount", "{case}");
            assert_eq!(scan.coinbase, case == "coinbase", "{case}");
            assert_eq!(scan.tip_height, if case == "shallow" { 5 } else { 6 });
            assert_eq!(wallet.balance(), 0);
        }
        server.await.unwrap();
    }
}

fn extend_empty(blocks: &mut Vec<Block>, target: u32) {
    while blocks.len() <= target as usize {
        let height = blocks.len() as u32;
        let mut block = blocks.last().unwrap().clone();
        block.txdata.truncate(1);
        block.txdata[0].lock_time = bitcoin::absolute::LockTime::from_height(height).unwrap();
        block.header.prev_blockhash = blocks.last().unwrap().block_hash();
        block.header.time += 1;
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        block.header.nonce = 0;
        while block.header.validate_pow(block.header.target()).is_err() {
            block.header.nonce += 1;
        }
        blocks.push(block);
    }
}

#[tokio::test]
async fn settlement_scan_follows_higher_work_fork_and_invalidates_old_spend() {
    let htlc = contract();
    let (mut blocks, txid) = chain(&htlc, "spent");
    extend_empty(&mut blocks, 11);
    let mut fork = blocks[..6].to_vec();
    extend_empty(&mut fork, 12);
    let wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
    let mut previous = Vec::new();
    for (branch, reorg) in [(blocks, false), (fork, true)] {
        let expected_tip = branch.last().unwrap().block_hash().to_byte_array();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, branch, "valid"));
        let scan = wallet
            .observe_swap(&htlc, txid, &[addr], NOW, &previous)
            .await
            .unwrap();
        assert_eq!(scan.tip_hash, expected_tip);
        assert_eq!(scan.invalidated_anchor, reorg);
        assert_eq!(scan.spend.is_none(), reorg);
        assert_eq!(scan.funding.as_ref().unwrap().height, 1);
        assert_eq!(wallet.best_height(), 0);
        previous = [scan.funding, scan.spend].into_iter().flatten().collect();
        server.await.unwrap();
    }
}

#[test]
fn incomplete_settlement_scan_cannot_publish_evidence() {
    let htlc = contract();
    let (blocks, txid) = chain(&htlc, "valid");
    let headers = Arc::new(Mutex::new(HeaderChain::anchored(bitcoin::Network::Regtest)));
    let sync = BtcSync::new(
        headers.clone(),
        Arc::new(Mutex::new(UtxoSet::new())),
        vec![htlc.address().unwrap()],
        htlc.network,
    );
    *sync.swap_scan.lock() = Some(swap_observation::SwapScanTracker::new(&htlc, txid).unwrap());
    for (height, block) in blocks.iter().enumerate().skip(1) {
        headers
            .lock()
            .add_header(&serialize(&block.header), height as u32)
            .unwrap();
    }
    let tip = blocks.last().unwrap().block_hash().to_byte_array();
    assert!(sync.finish_swap_scan(1, tip, 5).is_err());
    assert!(sync.finish_swap_scan(1, [0; 32], 6).is_err());
}
