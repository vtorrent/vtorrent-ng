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
        if height == 6 && case == "spent" {
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

async fn serve(listener: TcpListener, blocks: Vec<Block>, case: &'static str) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let script = contract().build_script().unwrap().to_p2wsh();
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
                let common = req
                    .locator_hashes
                    .iter()
                    .find_map(|hash| blocks.iter().position(|b| b.block_hash() == *hash))
                    .unwrap_or(0);
                NetworkMessage::Headers(blocks[common + 1..].iter().map(|b| b.header).collect())
            }
            NetworkMessage::GetCFCheckpt(req) => NetworkMessage::CFCheckpt(CFCheckpt {
                filter_type: 0,
                stop_hash: req.stop_hash,
                filter_headers: vec![],
            }),
            NetworkMessage::GetCFHeaders(req) => {
                assert_eq!(req.start_height, 0);
                NetworkMessage::CFHeaders(CFHeaders {
                    filter_type: 0,
                    stop_hash: req.stop_hash,
                    previous_filter_header: FilterHeader::all_zeros(),
                    filter_hashes: filters
                        .iter()
                        .map(|f| bitcoin::bip158::FilterHash::hash(&f.content))
                        .collect(),
                })
            }
            NetworkMessage::GetCFilters(req) => {
                if case == "disconnect" {
                    break;
                }
                for height in req.start_height as usize..blocks.len() {
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
