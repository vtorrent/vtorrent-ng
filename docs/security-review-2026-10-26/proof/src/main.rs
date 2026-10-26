use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::Hash;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::{block, BlockHash, Network, TxMerkleNode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use vtorrent_btc::{p2p::BtcPeer, utxo::Utxo, wallet::BtcWallet};

mod error {
    pub use vtorrent_btc::error::*;
}

mod sync {
    pub use vtorrent_btc::sync::{network_group, NetworkGroup};
}

#[allow(dead_code)]
mod audited_filters {
    include!("../../../../vtorrent-btc/src/filters.rs");

    pub fn growth_proof() {
        let mut store = FilterHeaderStore::default();
        for n in 1u32..=10_000 {
            let mut hash = [0; 32];
            hash[..4].copy_from_slice(&n.to_le_bytes());
            for peer in ["1.1.1.1:8333", "2.2.2.2:8333"] {
                let mut candidate = FilterHeaderStore::default();
                candidate.reconcile_checkpoints(hash, 0, &[]).unwrap();
                candidate
                    .apply_range(
                        0,
                        &[hash],
                        &[FilterHash::hash(b"filter")],
                        FilterHeader::all_zeros(),
                    )
                    .unwrap();
                store
                    .observe_candidate(peer.parse().unwrap(), candidate, 2)
                    .unwrap();
            }
        }
        assert_eq!(store.records.len(), 10_000);
        assert_eq!(store.candidates.len(), 10_000);
        println!("F04: records={}, candidate entries={} after 10,000 distinct tips; no retention applied", store.records.len(), store.candidates.len());
    }
}

async fn recv(stream: &mut TcpStream) -> Option<NetworkMessage> {
    let mut header = [0u8; 24];
    stream.read_exact(&mut header).await.ok()?;
    let len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    assert!(len <= 4 * 1024 * 1024);
    let mut bytes = header.to_vec();
    bytes.resize(24 + len, 0);
    stream.read_exact(&mut bytes[24..]).await.ok()?;
    let raw: RawNetworkMessage = deserialize(&bytes).ok()?;
    Some(raw.payload().clone())
}

async fn send(stream: &mut TcpStream, message: NetworkMessage) {
    stream
        .write_all(&serialize(&RawNetworkMessage::new(
            Network::Regtest.magic(),
            message,
        )))
        .await
        .unwrap();
}

async fn peer(
    response: Option<Vec<block::Header>>,
    requests: Arc<AtomicUsize>,
) -> (BtcPeer, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        while let Some(message) = recv(&mut stream).await {
            match message {
                NetworkMessage::Version(mut version) => {
                    version.services = bitcoin::p2p::ServiceFlags::COMPACT_FILTERS;
                    send(&mut stream, NetworkMessage::Version(version)).await;
                    send(&mut stream, NetworkMessage::Verack).await;
                }
                NetworkMessage::GetHeaders(_) => {
                    requests.fetch_add(1, Ordering::Relaxed);
                    match &response {
                        Some(headers) => {
                            send(&mut stream, NetworkMessage::Headers(headers.clone())).await
                        }
                        None => std::future::pending::<()>().await,
                    }
                }
                _ => {}
            }
        }
    });
    let connection = BtcPeer::connect_with_network(addr, Network::Regtest)
        .await
        .unwrap();
    (connection, server)
}

fn mine(parent: BlockHash, time: u32, marker: u8) -> block::Header {
    let genesis = bitcoin::blockdata::constants::genesis_block(Network::Regtest);
    let mut header = block::Header {
        version: block::Version::TWO,
        prev_blockhash: parent,
        merkle_root: TxMerkleNode::from_byte_array([marker; 32]),
        time,
        bits: genesis.header.bits,
        nonce: 0,
    };
    while header.validate_pow(header.target()).is_err() {
        header.nonce += 1;
    }
    header
}

async fn interrupted_reorg() {
    let genesis = bitcoin::blockdata::constants::genesis_block(Network::Regtest);
    let old = mine(genesis.block_hash(), genesis.header.time + 1, 1);
    let fork1 = mine(genesis.block_hash(), genesis.header.time + 2, 2);
    let fork2 = mine(fork1.block_hash(), genesis.header.time + 3, 3);
    let orphan = mine(
        BlockHash::from_byte_array([42; 32]),
        genesis.header.time + 4,
        4,
    );
    let mut wallet = BtcWallet::with_network([1; 64], Network::Regtest);
    wallet.add_header(&serialize(&old), 1).unwrap();
    let address = wallet.next_address().unwrap();
    wallet.add_utxo(Utxo {
        txid: hex::encode([9; 32]),
        vout: 0,
        value: 100_000,
        address,
        height: 1,
    });
    wallet.set_last_scanned_height(2);
    wallet.mark_synced();
    let (bad, bad_server) = peer(Some(vec![fork1, fork2, orphan]), Arc::default()).await;
    let (good, good_server) = peer(Some(vec![]), Arc::default()).await;
    let mut peers = vec![bad, good];
    let added = wallet.sync_with_peers(&mut peers).await.unwrap();
    assert_eq!(added, 0);
    assert_eq!(wallet.best_height(), 2);
    assert_eq!(wallet.balance(), 100_000);
    assert_eq!(wallet.last_scanned_height(), 2);
    println!("F01: higher-work fork accepted before malformed suffix; fallback returned Ok(0), old balance={}, cursor={}, height={}, synced={}", wallet.balance(), wallet.last_scanned_height(), wallet.best_height(), wallet.synced());
    bad_server.abort();
    good_server.abort();
}

async fn silent_peer() {
    let stalled_requests = Arc::new(AtomicUsize::new(0));
    let healthy_requests = Arc::new(AtomicUsize::new(0));
    let (silent, silent_server) = peer(None, stalled_requests.clone()).await;
    let (healthy, healthy_server) = peer(Some(vec![]), healthy_requests.clone()).await;
    let mut wallet = BtcWallet::with_network([2; 64], Network::Regtest);
    let mut peers = vec![silent, healthy];
    let outcome =
        tokio::time::timeout(Duration::from_secs(12), wallet.sync_with_peers(&mut peers)).await;
    assert!(outcome.is_err());
    assert_eq!(stalled_requests.load(Ordering::Relaxed), 1);
    assert_eq!(healthy_requests.load(Ordering::Relaxed), 0);
    println!("F02: header sync remained pending for 12 seconds; healthy fallback requests=0; outer proof timeout required");
    silent_server.abort();
    healthy_server.abort();
}

async fn configured_mainnet_peer() {
    let now = 1_800_000_000u64;
    let wallet = BtcWallet::with_network([3; 64], Network::Bitcoin);
    let htlc = vtorrent_btc::htlc::BtcHtlc {
        hash_lock: [42; 32],
        recipient: vtorrent_btc::keys::derive_address(&[3; 64], 0, Network::Bitcoin).unwrap(),
        refund_address: vtorrent_btc::keys::derive_address(&[4; 64], 0, Network::Bitcoin).unwrap(),
        expiry: (now + 86_400) as u32,
        amount: 100_000,
        network: Network::Bitcoin,
    };
    let error = wallet
        .verify_swap_funding(&htlc, [7; 32], &["127.0.0.1:1".parse().unwrap()], now)
        .await
        .unwrap_err();
    println!("F03: configured-only mainnet verifier returns {error}");
    assert!(error
        .to_string()
        .contains("requires 2 distinct compact-filter network groups"));
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    interrupted_reorg().await;
    silent_peer().await;
    configured_mainnet_peer().await;
    audited_filters::growth_proof();
    println!("All four review proofs reproduced the reported behavior using local inputs only.");
}
