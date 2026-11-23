//! Manual end-to-end test for the torrent incentive settlement path.
//!
//! This test runs the **exact daemon binary** named by
//! `VTORRENT_E2E_DAEMON_BIN` and requires its digest in
//! `VTORRENT_E2E_DAEMON_SHA256` (the candidate image's binary) against a
//! protocol-faithful seeder process. The seeder serves a real 12 MiB torrent
//! over the production peer-wire codec, advertises its VTR address through the
//! `ut_vtr` extension, and signs a real `BandwidthReceipt` after the daemon has
//! downloaded every byte. The daemon must then:
//!
//! 1. record and verify the receipt,
//! 2. settle the receipt-backed bytes into a debt,
//! 3. broadcast exactly one VTR payment to the seeder,
//! 4. persist the signed transaction and its txid, and
//! 5. restart without paying the same debt a second time.
//!
//! The daemon's tracker SSRF guard only accepts public tracker/peer addresses.
//! To exercise the unmodified binary, run the test inside a network namespace
//! with a public-looking address assigned to `lo`:
//!
//! ```text
//! unshare -rn sh -c '
//!   ip link set lo up
//!   ip addr add 8.8.8.8/32 dev lo
//!   exec target/debug/deps/torrent_incentive_e2e-<hash> --ignored --nocapture
//! '
//! ```
//!
//! The test is `#[ignore]`d so ordinary `cargo test --workspace` never needs
//! namespace privileges.

#![cfg(unix)]

use base64::Engine as _;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use vtorrent_core::address::Address;
use vtorrent_core::keys::PrivateKey;
use vtorrent_core::receipt::{BandwidthReceipt, SignedBandwidthReceipt};
use vtorrent_torrent::incentive::calculate_owed_for_bytes;
use vtorrent_torrent::metadata::{
    build_ut_vtr_address, build_ut_vtr_handshake, encode_receipt, parse_ut_vtr_address,
};
use vtorrent_torrent::peer_wire::PeerMessage;
use vtorrent_torrent::tracker::{AnnounceEvent, AnnounceRequest, HttpTracker};

const COIN: u64 = 100_000_000;
const LEECHER_WIF: &str = "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS";
const LEECHER_ADDR: &str = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";
const PASS: &str = "torrent-e2e-pass";
const TORRENT_SIZE: usize = 12 * 1024 * 1024;
const PIECE_LENGTH: usize = 256 * 1024;
const SEEDER_EPOCH: u64 = 7;
const SEEDER_KEY_BYTE: u8 = 0x5a;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn bencode_string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(value.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(value);
}

fn bencode_u64(out: &mut Vec<u8>, value: u64) {
    out.push(b'i');
    out.extend_from_slice(value.to_string().as_bytes());
    out.push(b'e');
}

fn deterministic_data() -> Vec<u8> {
    let mut data = vec![0u8; TORRENT_SIZE];
    for (index, byte) in data.iter_mut().enumerate() {
        *byte = ((index * 31 + 7) % 251) as u8;
    }
    data
}

fn build_torrent(announce: &str, data: &[u8]) -> (Vec<u8>, [u8; 20]) {
    let mut pieces = Vec::with_capacity(data.len().div_ceil(PIECE_LENGTH) * 20);
    for chunk in data.chunks(PIECE_LENGTH) {
        let mut hasher = Sha1::new();
        hasher.update(chunk);
        pieces.extend_from_slice(&hasher.finalize());
    }

    let mut info = Vec::new();
    info.push(b'd');
    bencode_string(&mut info, b"length");
    bencode_u64(&mut info, data.len() as u64);
    bencode_string(&mut info, b"name");
    bencode_string(&mut info, b"incentive-e2e.bin");
    bencode_string(&mut info, b"piece length");
    bencode_u64(&mut info, PIECE_LENGTH as u64);
    bencode_string(&mut info, b"pieces");
    bencode_string(&mut info, &pieces);
    info.push(b'e');

    let digest = Sha1::digest(&info);
    let mut info_hash = [0u8; 20];
    info_hash.copy_from_slice(&digest);

    let mut torrent = Vec::new();
    torrent.push(b'd');
    bencode_string(&mut torrent, b"announce");
    bencode_string(&mut torrent, announce.as_bytes());
    bencode_string(&mut torrent, b"info");
    torrent.extend_from_slice(&info);
    torrent.push(b'e');

    (torrent, info_hash)
}

fn full_bitfield(piece_count: usize) -> Vec<u8> {
    let mut bits = vec![0u8; piece_count.div_ceil(8)];
    for index in 0..piece_count {
        bits[index / 8] |= 0x80 >> (index % 8);
    }
    bits
}

fn seeder_key() -> PrivateKey {
    PrivateKey::from_bytes([SEEDER_KEY_BYTE; 32], true).expect("deterministic seeder key")
}

fn vtr_address(key: &PrivateKey) -> String {
    let pubkey = key.public_key().expect("seeder public key");
    Address::from_pubkey(&pubkey, key.is_compressed(), 70).to_string()
}

fn txid_bytes(txid: &str) -> Vec<u8> {
    hex::decode(txid).expect("hex txid")
}

fn value_is_txid(value: &Value, txid: &[u8]) -> bool {
    value.as_array().is_some_and(|bytes| {
        bytes.len() == txid.len()
            && bytes
                .iter()
                .zip(txid)
                .all(|(value, byte)| value.as_u64() == Some(u64::from(*byte)))
    })
}

fn journal_has_pending_txid(text: &str, txid: &str) -> bool {
    let bytes = txid_bytes(txid);
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|record| value_is_txid(&record["account"]["pending_payment"]["txid"], &bytes))
}

fn ledger_has_pending_debt(
    ledger: &Value,
    peer_address: &str,
    txid: &str,
    expected_satoshis: u64,
) -> bool {
    let bytes = txid_bytes(txid);
    ledger["debts"].as_array().is_some_and(|debts| {
        debts.iter().any(|debt| {
            debt["peer_address"].as_str() == Some(peer_address)
                && debt["unpaid_satoshis"].as_u64() == Some(expected_satoshis)
                && debt["pending_satoshis"].as_u64() == Some(expected_satoshis)
                && value_is_txid(&debt["pending_txid"], &bytes)
        })
    })
}

fn read_intents(path: &Path) -> Vec<Value> {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => {
            serde_json::from_slice(&bytes).expect("parse payment intents")
        }
        _ => Vec::new(),
    }
}

async fn write_message(stream: &mut TcpStream, message: &PeerMessage) -> Result<(), String> {
    stream
        .write_all(&message.encode())
        .await
        .map_err(|error| error.to_string())
}

async fn read_message(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    timeout: Duration,
) -> Result<PeerMessage, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some((message, consumed)) =
            PeerMessage::decode(buffer).map_err(|error| error.to_string())?
        {
            buffer.drain(..consumed);
            return Ok(message);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("timed out waiting for a peer message".into());
        }
        let mut chunk = [0u8; 8192];
        let read = tokio::time::timeout(remaining, stream.read(&mut chunk))
            .await
            .map_err(|_| "timed out waiting for a peer message".to_string())?
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("peer closed the connection".into());
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

struct SeederConfig {
    info_hash: [u8; 20],
    data: Arc<Vec<u8>>,
    key: PrivateKey,
    address: String,
    leecher_address: String,
}

#[derive(Debug)]
struct SeederOutcome {
    served_bytes: u64,
    leecher_address: String,
}

async fn serve_peer(
    mut stream: TcpStream,
    config: SeederConfig,
    data_complete: oneshot::Sender<()>,
    send_receipt: oneshot::Receiver<()>,
) -> Result<SeederOutcome, String> {
    let mut handshake = [0u8; 68];
    eprintln!("seeder: waiting for the daemon handshake");
    stream
        .read_exact(&mut handshake)
        .await
        .map_err(|error| error.to_string())?;
    eprintln!("seeder: read the daemon handshake");
    let peer_id = match PeerMessage::decode_handshake(&handshake)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "incomplete handshake".to_string())?
        .0
    {
        PeerMessage::Handshake {
            info_hash, peer_id, ..
        } if info_hash == config.info_hash => peer_id,
        other => return Err(format!("unexpected handshake: {other:?}")),
    };
    eprintln!("seeder: accepted daemon peer {}", hex::encode(peer_id));

    write_message(
        &mut stream,
        &PeerMessage::Handshake {
            info_hash: config.info_hash,
            peer_id: [0x53; 20],
            reserved: [0, 0, 0, 0, 0, 0x10, 0, 0],
        },
    )
    .await?;
    eprintln!("seeder: sent handshake");

    // Real clients (including vTorrent's own seeder) announce their pieces
    // immediately after the handshake, *before* the extension handshake. The
    // engine must not lose this message while it looks for the extension
    // handshake, or it will never learn that the peer has any piece.
    let piece_count = config.data.len().div_ceil(PIECE_LENGTH);
    write_message(
        &mut stream,
        &PeerMessage::Bitfield {
            bits: full_bitfield(piece_count),
        },
    )
    .await?;
    write_message(&mut stream, &PeerMessage::Unchoke).await?;
    write_message(
        &mut stream,
        &PeerMessage::Extended {
            id: 0,
            payload: build_ut_vtr_handshake(1),
        },
    )
    .await?;
    write_message(
        &mut stream,
        &PeerMessage::Extended {
            id: 1,
            payload: build_ut_vtr_address(&config.address),
        },
    )
    .await?;
    eprintln!("seeder: sent bitfield, unchoke, extension handshake, address");

    let mut buffer = Vec::new();
    let mut requested = HashSet::new();
    let mut served = 0usize;
    let mut leecher_address = None;
    eprintln!("seeder: waiting for daemon peer messages");
    while served < config.data.len() {
        let message = read_message(&mut stream, &mut buffer, Duration::from_secs(120)).await?;
        match message {
            PeerMessage::Extended { id: 1, payload } => {
                let address = parse_ut_vtr_address(&payload).map_err(|error| error.to_string())?;
                eprintln!("seeder: received daemon VTR address {address}");
                leecher_address = Some(address);
            }
            PeerMessage::Request {
                index,
                begin,
                length,
            } => {
                if length == 0 {
                    continue;
                }
                let piece_start = index as usize * PIECE_LENGTH;
                if piece_start >= config.data.len() {
                    continue;
                }
                let piece_end = (piece_start + PIECE_LENGTH).min(config.data.len());
                let start = piece_start + begin as usize;
                if start >= piece_end {
                    continue;
                }
                let end = (start + length as usize).min(piece_end);
                let data = config.data[start..end].to_vec();
                let key = (index, begin, data.len() as u32);
                if requested.insert(key) {
                    served = served.saturating_add(data.len());
                }
                write_message(&mut stream, &PeerMessage::Piece { index, begin, data }).await?;
            }
            other => eprintln!("seeder: received {other:?}"),
        }
    }

    tracing::info!(
        served_bytes = served,
        "seeder finished sending all torrent bytes"
    );
    let _ = data_complete.send(());
    tokio::time::timeout(Duration::from_secs(60), send_receipt)
        .await
        .map_err(|_| "timed out waiting to send the bandwidth receipt".to_string())?
        .map_err(|_| "receipt authorization channel closed".to_string())?;

    let leecher_address = leecher_address.ok_or_else(|| "no leecher VTR address".to_string())?;
    if leecher_address != config.leecher_address {
        return Err(format!(
            "leecher advertised {leecher_address}, expected {}",
            config.leecher_address
        ));
    }

    let receipt = BandwidthReceipt {
        info_hash: config.info_hash,
        window_start: 0,
        window_end: now_secs(),
        uploaded_by_me: served as u64,
        downloaded_by_me: 0,
        peer: leecher_address.clone(),
        signer: config.address.clone(),
        settlement_id: now_secs(),
        epoch: SEEDER_EPOCH,
    };
    let signature = receipt
        .sign(&config.key)
        .map_err(|error| error.to_string())?;
    let pubkey = config
        .key
        .public_key()
        .map_err(|error| error.to_string())?
        .serialize()
        .to_vec();
    let envelope = SignedBandwidthReceipt {
        receipt,
        pubkey,
        signature,
    };
    write_message(
        &mut stream,
        &PeerMessage::Extended {
            id: 2,
            payload: encode_receipt(&envelope),
        },
    )
    .await?;
    tracing::info!(
        served_bytes = served,
        signer = %config.address,
        "seeder sent the signed bandwidth receipt"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;

    Ok(SeederOutcome {
        served_bytes: served as u64,
        leecher_address,
    })
}

async fn start_tracker(
    ip: Ipv4Addr,
    seeder_port: u16,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind((IpAddr::V4(ip), 0))
        .await
        .expect("bind tracker");
    let address = listener.local_addr().expect("tracker address");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                eprintln!("tracker received an announce");

                let mut body = Vec::new();
                body.push(b'd');
                bencode_string(&mut body, b"interval");
                bencode_u64(&mut body, 60);
                bencode_string(&mut body, b"peers");
                let mut peers = ip.octets().to_vec();
                peers.extend_from_slice(&seeder_port.to_be_bytes());
                bencode_string(&mut body, &peers);
                body.push(b'e');

                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
                eprintln!("tracker sent a {} byte response", body.len());
            });
        }
    });
    (address, handle)
}

async fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("free port");
    listener.local_addr().expect("free port address").port()
}

struct Daemon {
    child: Child,
    rpc: String,
    log: PathBuf,
    client: reqwest::Client,
}

impl Daemon {
    async fn start(binary: &Path, data_dir: &Path, log_name: &str) -> Self {
        let started = Instant::now();
        std::fs::create_dir_all(data_dir).expect("create data dir");
        let rpc_port = free_port().await;
        let mut p2p_port = free_port().await;
        while p2p_port == rpc_port {
            p2p_port = free_port().await;
        }
        let log_path = data_dir.join(log_name);
        let log = std::fs::File::create(&log_path).expect("create daemon log");

        let mut command = Command::new(binary);
        command
            .args([
                "--regtest",
                "--testnet",
                "--isolated",
                "--no-dht",
                "--rpc-addr",
                &format!("127.0.0.1:{rpc_port}"),
                "--listen",
                &format!("127.0.0.1:{p2p_port}"),
                "--public-addr",
                &format!("127.0.0.1:{p2p_port}"),
                "--data-dir",
                data_dir.to_str().expect("utf-8 data dir"),
                "--log-level",
                "vtorrent_daemon=debug,vtorrent_node=warn,vtorrent_store=warn,vtorrent_torrent=debug",
            ])
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("NO_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .env_remove("no_proxy")
            .env_remove("VTORRENT_RPC_API_KEY")
            .env_remove("VTORRENT_BTC_SEED")
            .env_remove("VTORRENT_STAKING_WIF")
            .env_remove("RUST_LOG")
            .stdout(Stdio::from(log.try_clone().expect("clone log")))
            .stderr(Stdio::from(log))
            .kill_on_drop(true);

        let child = command.spawn().expect("spawn daemon");
        let mut daemon = Self {
            child,
            rpc: format!("http://127.0.0.1:{rpc_port}"),
            log: log_path,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("rpc client"),
        };

        tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                assert!(
                    daemon.child.try_wait().expect("daemon status").is_none(),
                    "daemon exited during startup after {:?}: {}",
                    started.elapsed(),
                    daemon.logs()
                );
                if let Ok(response) = daemon
                    .client
                    .get(format!("{}/api/v1/info", daemon.rpc))
                    .send()
                    .await
                {
                    if response.status().is_success() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "daemon startup timed out after {:?}: {}",
                started.elapsed(),
                daemon.logs()
            )
        });
        eprintln!(
            "daemon ready in {:?} (rpc {})",
            started.elapsed(),
            daemon.rpc
        );
        daemon
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    async fn get(&self, path: &str) -> Value {
        let response = self
            .client
            .get(format!("{}{path}", self.rpc))
            .send()
            .await
            .unwrap_or_else(|error| panic!("GET {path} failed: {error}; {}", self.logs()));
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "GET {path}: {status} {body}; {}",
            self.logs()
        );
        serde_json::from_str(&body)
            .unwrap_or_else(|error| panic!("GET {path}: invalid JSON {error}: {body}"))
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let response = self
            .client
            .post(format!("{}{path}", self.rpc))
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|error| panic!("POST {path} failed: {error}; {}", self.logs()));
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "POST {path}: {status} {text}; {}",
            self.logs()
        );
        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("POST {path}: invalid JSON {error}: {text}"))
    }

    async fn stop(mut self) {
        let pid = self.child.id().expect("daemon pid");
        let status = StdCommand::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .expect("send SIGTERM");
        assert!(status.success(), "kill -TERM failed");
        let status = tokio::time::timeout(Duration::from_secs(30), self.child.wait())
            .await
            .unwrap_or_else(|_| panic!("daemon did not exit after SIGTERM: {}", self.logs()))
            .expect("wait for daemon");
        assert!(
            status.success(),
            "daemon exited with {status} after SIGTERM: {}",
            self.logs()
        );
    }
}

async fn wait_for_balance(daemon: &Daemon, minimum: u64) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let balance = daemon.get("/api/v1/wallet/balance").await;
            if balance["confirmed"].as_u64().unwrap_or(0) >= minimum {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("wallet balance never reached {minimum}: {}", daemon.logs()));
}

async fn wait_for_download(daemon: &Daemon, session_id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let sessions = daemon.get("/api/v1/torrent/sessions").await;
            if let Some(session) = sessions.as_array().and_then(|sessions| {
                sessions
                    .iter()
                    .find(|session| session["id"].as_str() == Some(session_id))
            }) {
                if session["downloaded_bytes"].as_u64().unwrap_or(0) >= TORRENT_SIZE as u64 {
                    return session.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "torrent did not complete; sessions: {}; log:\n{}",
            futures_blocking(daemon.rpc.clone(), "/api/v1/torrent/sessions"),
            daemon.logs()
        )
    })
}

async fn wait_for_payment(daemon: &Daemon, seeder_script_hex: &str) -> (String, Value) {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let mempool = daemon.get("/api/v1/mempool").await;
            for txid in mempool["txids"].as_array().into_iter().flatten() {
                let txid = txid.as_str().expect("txid string").to_owned();
                let tx = daemon.get(&format!("/api/v1/blockchain/tx/{txid}")).await;
                let pays_seeder = tx["outputs"].as_array().is_some_and(|outputs| {
                    outputs
                        .iter()
                        .any(|output| output["script_pubkey"].as_str() == Some(seeder_script_hex))
                });
                if pays_seeder {
                    return (txid, tx);
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "incentive payment never appeared; mempool: {}; sessions: {}; log:\n{}",
            futures_blocking(daemon.rpc.clone(), "/api/v1/mempool"),
            futures_blocking(daemon.rpc.clone(), "/api/v1/torrent/sessions"),
            daemon.logs()
        )
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual two-process torrent incentive E2E; requires a netns with VTORRENT_E2E_PUBLIC_IP assigned to lo"]
async fn torrent_incentive_receipt_settles_pays_and_restart_does_not_repay() {
    let public_ip: Ipv4Addr = std::env::var("VTORRENT_E2E_PUBLIC_IP")
        .unwrap_or_else(|_| "8.8.8.8".to_string())
        .parse()
        .expect("IPv4 VTORRENT_E2E_PUBLIC_IP");
    let daemon_bin = PathBuf::from(
        std::env::var("VTORRENT_E2E_DAEMON_BIN")
            .expect("set VTORRENT_E2E_DAEMON_BIN to the exact candidate daemon binary"),
    );
    assert!(
        daemon_bin.is_file(),
        "daemon binary not found: {}",
        daemon_bin.display()
    );
    let expected = std::env::var("VTORRENT_E2E_DAEMON_SHA256")
        .expect("set VTORRENT_E2E_DAEMON_SHA256 to the candidate binary's sha256");
    let digest = sha2::Sha256::digest(std::fs::read(&daemon_bin).expect("read daemon binary"));
    assert_eq!(
        hex::encode(digest),
        expected.to_ascii_lowercase(),
        "daemon binary does not match the expected candidate image"
    );

    let workdir = PathBuf::from(
        std::env::var("VTORRENT_E2E_WORKDIR")
            .unwrap_or_else(|_| format!("/mnt/temp/vtorrent-ng-e2e/run-{}", std::process::id())),
    );
    std::fs::create_dir_all(&workdir).expect("create workdir");
    let data_dir = workdir.join("node");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let seeder_key = seeder_key();
    let seeder_address = vtr_address(&seeder_key);
    let seeder_address_for_task = seeder_address.clone();
    let seeder_script_hex = hex::encode(
        Address::parse(&seeder_address)
            .expect("seeder address")
            .p2pkh_script_pubkey(),
    );
    eprintln!("seeder VTR address: {seeder_address}");

    let data = Arc::new(deterministic_data());
    let seeder_listener = TcpListener::bind((IpAddr::V4(public_ip), 0))
        .await
        .expect("bind seeder");
    let seeder_addr = seeder_listener.local_addr().expect("seeder address");
    let (tracker_addr, _tracker) = start_tracker(public_ip, seeder_addr.port()).await;
    let announce = format!("http://{tracker_addr}/announce");
    let (torrent, info_hash) = build_torrent(&announce, &data);
    eprintln!(
        "torrent info hash: {} ({} bytes, tracker {tracker_addr}, seeder {seeder_addr})",
        hex::encode(info_hash),
        data.len()
    );

    let probe = HttpTracker::new()
        .expect("probe tracker client")
        .announce(&AnnounceRequest {
            tracker_url: announce.clone(),
            info_hash,
            peer_id: [0x42; 20],
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: TORRENT_SIZE as u64,
            event: AnnounceEvent::Started,
            num_want: 50,
        })
        .await;
    eprintln!("tracker probe: {probe:?}");
    let probe = probe.expect("production tracker client must reach the test tracker");
    assert_eq!(
        probe.peers,
        vec![vtorrent_torrent::tracker::TrackerPeer {
            ip: public_ip.to_string(),
            port: seeder_addr.port(),
            peer_id: None,
        }],
        "tracker probe returned the wrong peers"
    );

    let (data_complete_tx, data_complete_rx) = oneshot::channel();
    let (send_receipt_tx, send_receipt_rx) = oneshot::channel();
    let mut seeder = tokio::spawn(async move {
        let (stream, _) = tokio::time::timeout(Duration::from_secs(120), seeder_listener.accept())
            .await
            .map_err(|_| "timed out accepting the daemon torrent connection".to_string())?
            .map_err(|error| error.to_string())?;
        serve_peer(
            stream,
            SeederConfig {
                info_hash,
                data,
                key: seeder_key,
                address: seeder_address_for_task,
                leecher_address: LEECHER_ADDR.to_string(),
            },
            data_complete_tx,
            send_receipt_rx,
        )
        .await
    });

    let daemon = Daemon::start(&daemon_bin, &data_dir, "first.log").await;

    let faucet = daemon
        .post(
            "/api/v1/faucet",
            json!({"address": LEECHER_ADDR, "amount_satoshis": 200u64 * COIN}),
        )
        .await;
    assert_eq!(
        faucet["block_height"].as_u64(),
        Some(1),
        "unexpected faucet response: {faucet}"
    );
    let imported = daemon
        .post(
            "/api/v1/wallet/import",
            json!({"wif": LEECHER_WIF, "passphrase": PASS, "overwrite": false}),
        )
        .await;
    assert_eq!(
        imported["address"].as_str(),
        Some(LEECHER_ADDR),
        "unexpected wallet import response: {imported}"
    );
    daemon
        .post(
            "/api/v1/wallet/unlock",
            json!({"passphrase": PASS, "timeout_secs": 0}),
        )
        .await;
    wait_for_balance(&daemon, 200 * COIN).await;

    let source = base64::engine::general_purpose::STANDARD.encode(&torrent);
    let added = daemon
        .post(
            "/api/v1/torrent/add",
            json!({
                "source": source,
                "source_type": "file",
                "wallet_address": LEECHER_ADDR,
            }),
        )
        .await;
    let session_id = added["session_id"]
        .as_str()
        .expect("torrent session id")
        .to_owned();
    assert_eq!(
        added["info_hash"].as_str(),
        Some(hex::encode(info_hash).as_str()),
        "unexpected torrent add response: {added}"
    );
    eprintln!("torrent session: {session_id}");

    match tokio::time::timeout(Duration::from_secs(180), data_complete_rx).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            let error = (&mut seeder).await;
            panic!(
                "seeder ended before data completion: {error:?}; log:\n{}",
                daemon.logs()
            );
        }
        Err(_) => {
            panic!(
                "seeder never finished serving the torrent; sessions: {}; log:\n{}",
                futures_blocking(daemon.rpc.clone(), "/api/v1/torrent/sessions"),
                daemon.logs()
            )
        }
    }
    let session = wait_for_download(&daemon, &session_id).await;
    eprintln!(
        "daemon credited {} bytes (state {})",
        session["downloaded_bytes"], session["state"]
    );

    send_receipt_tx
        .send(())
        .expect("send receipt authorization");

    let (payment_txid, payment_tx) = wait_for_payment(&daemon, &seeder_script_hex).await;
    let expected_satoshis = calculate_owed_for_bytes(TORRENT_SIZE as u64);
    let paid_satoshis = payment_tx["outputs"]
        .as_array()
        .expect("payment outputs")
        .iter()
        .find(|output| output["script_pubkey"].as_str() == Some(seeder_script_hex.as_str()))
        .and_then(|output| output["value_satoshis"].as_u64())
        .expect("seeder payment output");
    assert_eq!(
        paid_satoshis, expected_satoshis,
        "payment amount mismatch: {payment_tx}"
    );
    assert_eq!(
        payment_tx["block_height"],
        Value::Null,
        "the payment must still be in the mempool for the restart test"
    );
    eprintln!(
        "observed incentive payment {payment_txid} for {paid_satoshis} satoshis in the mempool"
    );

    let journal_path = data_dir.join("incentive-journal.jsonl");
    let intent_path = data_dir.join("incentive-payment-intents.json");
    let ledger_path = data_dir.join("incentive-ledger.json");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let journal = std::fs::read_to_string(&journal_path).unwrap_or_default();
            if journal_has_pending_txid(&journal, &payment_txid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "journal never recorded pending txid {payment_txid}; log:\n{}",
            daemon.logs()
        )
    });

    let first_log = daemon.logs();
    assert_eq!(
        first_log
            .matches(&format!("Incentive payment {payment_txid} sent"))
            .count(),
        1,
        "expected exactly one payment broadcast; log:\n{first_log}"
    );

    daemon.stop().await;

    let intents = read_intents(&intent_path);
    assert!(
        intents.is_empty() || intents.len() == 1,
        "unexpected intents: {intents:?}"
    );
    if let Some(intent) = intents.first() {
        assert_eq!(
            intent["peer_address"].as_str(),
            Some(seeder_address.as_str())
        );
        assert_eq!(intent["amount_satoshis"].as_u64(), Some(expected_satoshis));
        assert!(
            value_is_txid(&intent["txid"], &txid_bytes(&payment_txid)),
            "intent txid mismatch: {}",
            intent["txid"]
        );
    }

    let restarted = Daemon::start(&daemon_bin, &data_dir, "restart.log").await;
    restarted
        .post(
            "/api/v1/wallet/unlock",
            json!({"passphrase": PASS, "timeout_secs": 0}),
        )
        .await;

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let ledger: Value =
                serde_json::from_slice(&std::fs::read(&ledger_path).unwrap_or_default())
                    .unwrap_or(Value::Null);
            if ledger_has_pending_debt(&ledger, &seeder_address, &payment_txid, expected_satoshis) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "restart never recovered the pending debt; ledger: {}; log:\n{}",
            std::fs::read_to_string(&ledger_path).unwrap_or_default(),
            restarted.logs()
        )
    });

    for round in 0..3 {
        let mempool = restarted.get("/api/v1/mempool").await;
        let txids: Vec<&str> = mempool["txids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            txids,
            vec![payment_txid.as_str()],
            "restart round {round} changed the mempool: {mempool}; log:\n{}",
            restarted.logs()
        );
        let tx = restarted
            .get(&format!("/api/v1/blockchain/tx/{payment_txid}"))
            .await;
        let still_paid = tx["outputs"].as_array().is_some_and(|outputs| {
            outputs.iter().any(|output| {
                output["script_pubkey"].as_str() == Some(seeder_script_hex.as_str())
                    && output["value_satoshis"].as_u64() == Some(expected_satoshis)
            })
        });
        assert!(still_paid, "payment disappeared after restart: {tx}");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    let sessions = restarted.get("/api/v1/torrent/sessions").await;
    assert_eq!(
        sessions.as_array().map(Vec::len),
        Some(0),
        "torrent sessions must not survive a restart: {sessions}"
    );

    let restart_log = restarted.logs();
    assert_eq!(
        restart_log.matches("Incentive payment ").count(),
        0,
        "restart attempted a second payment; log:\n{restart_log}"
    );

    let ledger: Value =
        serde_json::from_slice(&std::fs::read(&ledger_path).expect("read ledger")).expect("ledger");
    let debt = ledger["debts"]
        .as_array()
        .and_then(|debts| {
            debts
                .iter()
                .find(|debt| debt["peer_address"].as_str() == Some(seeder_address.as_str()))
        })
        .expect("recovered debt");
    assert_eq!(debt["unpaid_satoshis"].as_u64(), Some(expected_satoshis));
    assert_eq!(debt["pending_satoshis"].as_u64(), Some(expected_satoshis));
    assert!(value_is_txid(
        &debt["pending_txid"],
        &txid_bytes(&payment_txid)
    ));
    assert_eq!(
        ledger["emitted_total"].as_u64(),
        Some(expected_satoshis),
        "budget must reflect exactly one payment: {ledger}"
    );

    let journal = std::fs::read_to_string(&journal_path).expect("read journal");
    assert!(
        journal_has_pending_txid(&journal, &payment_txid),
        "journal lost the pending txid after restart:\n{journal}"
    );

    let intents = read_intents(&intent_path);
    assert!(
        intents.is_empty() || intents.len() == 1,
        "unexpected intents after restart: {intents:?}"
    );
    if let Some(intent) = intents.first() {
        assert!(value_is_txid(&intent["txid"], &txid_bytes(&payment_txid)));
    }

    let seeder_outcome = seeder.await.expect("seeder task");
    let seeder_outcome = seeder_outcome.expect("seeder result");
    assert_eq!(seeder_outcome.served_bytes, TORRENT_SIZE as u64);
    assert_eq!(seeder_outcome.leecher_address, LEECHER_ADDR);

    restarted.stop().await;

    eprintln!("E2E PASS");
    eprintln!("data_dir={}", data_dir.display());
    eprintln!("seeder_address={seeder_address}");
    eprintln!("payment_txid={payment_txid}");
    eprintln!("payment_satoshis={expected_satoshis}");
}

/// Best-effort synchronous fetch used only for panic diagnostics.
fn futures_blocking(rpc: String, path: &str) -> String {
    let output = StdCommand::new("curl")
        .args(["-sS", "--max-time", "5", &format!("{rpc}{path}")])
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(error) => format!("<curl failed: {error}>"),
    }
}
