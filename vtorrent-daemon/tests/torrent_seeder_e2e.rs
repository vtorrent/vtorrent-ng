//! Manual end-to-end test for the **inbound** torrent seeder.
//!
//! [`torrent_incentive_e2e`] proves the download/pay half: a daemon dials a peer
//! and pays it. This test proves the other half, which no unit test can cover:
//!
//! 1. the daemon starts its `--torrent-listen` seeder, completes a torrent it
//!    was given, and registers it for serving;
//! 2. a second, independent client **dials the daemon** and downloads every
//!    piece over the inbound path;
//! 3. the client verifies each piece against the metainfo, so the daemon is
//!    proven to serve real bytes, not just to accept connections;
//! 4. the daemon signs a bandwidth receipt that the client verifies, and the
//!    daemon credits the uploaded bytes to the incentive account;
//! 5. the client cannot request a torrent the daemon does not hold.
//!
//! Run it with a network namespace that has a public-looking address, because
//! the tracker/peer policy rejects loopback targets exactly as in production:
//!
//! ```text
//! VTORRENT_E2E_DAEMON_BIN=/path/to/vtorrent-daemon \
//! VTORRENT_E2E_DAEMON_SHA256=<sha256> \
//! unshare -rn sh -c '
//!   ip link set lo up
//!   ip addr add 8.8.8.8/32 dev lo
//!   exec target/debug/deps/torrent_seeder_e2e-<hash> --ignored --nocapture
//! '
//! ```

#![cfg(unix)]

use base64::Engine as _;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use vtorrent_core::receipt::BandwidthReceipt;
use vtorrent_torrent::metadata::{
    build_ut_vtr_address, build_ut_vtr_handshake, parse_ut_vtr_address,
};
use vtorrent_torrent::peer_wire::PeerMessage;

const COIN: u64 = 100_000_000;
const LEECHER_WIF: &str = "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS";
const LEECHER_ADDR: &str = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";
const PASS: &str = "torrent-seed-e2e-pass";
/// Address the independent client advertises; the daemon signs a receipt to it.
const CLIENT_ADDR: &str = "VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh";
const TORRENT_SIZE: usize = 6 * 1024 * 1024;
const PIECE_LENGTH: usize = 256 * 1024;
const PIECE_BLOCK: u32 = 16 * 1024;

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
        *byte = ((index * 17 + 11) % 251) as u8;
    }
    data
}

fn build_torrent(announce: &str, data: &[u8]) -> (Vec<u8>, [u8; 20], Vec<[u8; 20]>) {
    let mut pieces: Vec<[u8; 20]> = Vec::with_capacity(data.len().div_ceil(PIECE_LENGTH));
    for chunk in data.chunks(PIECE_LENGTH) {
        let digest: [u8; 20] = Sha1::digest(chunk).into();
        pieces.push(digest);
    }
    let mut piece_bytes = Vec::with_capacity(pieces.len() * 20);
    for piece in &pieces {
        piece_bytes.extend_from_slice(piece);
    }

    let mut info = Vec::new();
    info.push(b'd');
    bencode_string(&mut info, b"length");
    bencode_u64(&mut info, data.len() as u64);
    bencode_string(&mut info, b"name");
    bencode_string(&mut info, b"seeder-e2e.bin");
    bencode_string(&mut info, b"piece length");
    bencode_u64(&mut info, PIECE_LENGTH as u64);
    bencode_string(&mut info, b"pieces");
    bencode_string(&mut info, &piece_bytes);
    info.push(b'e');

    let info_hash: [u8; 20] = Sha1::digest(&info).into();

    let mut torrent = Vec::new();
    torrent.push(b'd');
    bencode_string(&mut torrent, b"announce");
    bencode_string(&mut torrent, announce.as_bytes());
    bencode_string(&mut torrent, b"info");
    torrent.extend_from_slice(&info);
    torrent.push(b'e');

    (torrent, info_hash, pieces)
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
        let n = tokio::time::timeout(remaining, stream.read(&mut chunk))
            .await
            .map_err(|_| "timed out waiting for a peer message".to_string())?
            .map_err(|error| error.to_string())?;
        if n == 0 {
            return Err("peer closed the connection".into());
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
}

/// Write the torrent's data, ownership marker, and a full resume bitfield into
/// the daemon's download directory, exactly as a completed download leaves them.
fn stage_complete_torrent(data_dir: &Path, info_hash: [u8; 20], data: &[u8]) {
    let name = "seeder-e2e.bin";
    let downloads = data_dir.join("downloads");
    let base = downloads.join(name);
    std::fs::create_dir_all(&base).expect("create torrent dir");
    std::fs::write(
        base.join(".vtorrent-torrent"),
        format!("v1:{}\n", hex::encode(info_hash)),
    )
    .expect("write ownership marker");
    std::fs::write(base.join(name), data).expect("write torrent data");
    let piece_count = data.len().div_ceil(PIECE_LENGTH);
    let mut bits = vec![0u8; piece_count.div_ceil(8)];
    for index in 0..piece_count {
        bits[index / 8] |= 0x80 >> (index % 8);
    }
    std::fs::create_dir_all(&downloads).expect("create downloads dir");
    std::fs::write(
        downloads.join(format!("{}.vtorrent", hex::encode(info_hash))),
        bits,
    )
    .expect("write resume bitfield");
}

/// A tracker that hands the client the daemon's seeder address.
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
    async fn start(binary: &Path, data_dir: &Path, log_name: &str, torrent_port: u16) -> Self {
        let started = Instant::now();
        std::fs::create_dir_all(data_dir).expect("create data dir");
        let rpc_port = free_port().await;
        let mut p2p_port = free_port().await;
        while p2p_port == rpc_port || p2p_port == torrent_port {
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
                "--torrent-listen",
                "--torrent-listen-addr",
                &format!("0.0.0.0:{torrent_port}"),
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
                    "daemon exited during startup: {}",
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
        serde_json::from_str(&body).unwrap_or_else(|error| panic!("invalid JSON: {error}: {body}"))
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
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("invalid JSON: {error}: {text}"))
    }

    async fn stop(mut self) {
        let pid = self.child.id().expect("daemon pid");
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        let _ = tokio::time::timeout(Duration::from_secs(30), self.child.wait()).await;
    }
}

/// The independent client: dial the daemon and download every piece.
struct ClientOutcome {
    bytes: usize,
    verified_pieces: usize,
    peer_address: Option<String>,
    receipt: Option<BandwidthReceipt>,
}

async fn download_from_seeder(
    seeder: SocketAddr,
    info_hash: [u8; 20],
    pieces: &[[u8; 20]],
    total_size: usize,
) -> ClientOutcome {
    let peer_id = [0x77u8; 20];
    let mut stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(seeder))
        .await
        .expect("connect to the daemon seeder timed out")
        .expect("connect to the daemon seeder");
    write_message(
        &mut stream,
        &PeerMessage::Handshake {
            info_hash,
            peer_id,
            reserved: [0, 0, 0, 0, 0, 0x10, 0, 0],
        },
    )
    .await
    .expect("send handshake");

    // The handshake is a fixed 68-byte frame without a length prefix, so it
    // must be decoded with `decode_handshake`, not the framed decoder.
    let mut handshake = [0u8; 68];
    tokio::time::timeout(Duration::from_secs(20), stream.read_exact(&mut handshake))
        .await
        .expect("seeder handshake reply timed out")
        .expect("seeder handshake reply");
    match PeerMessage::decode_handshake(&handshake)
        .expect("decode seeder handshake")
        .expect("complete seeder handshake")
        .0
    {
        PeerMessage::Handshake {
            info_hash: remote_hash,
            ..
        } => assert_eq!(remote_hash, info_hash, "seeder echoed the wrong torrent"),
        other => panic!("expected a handshake, got {other:?}"),
    }
    let mut buffer = Vec::new();

    write_message(
        &mut stream,
        &PeerMessage::Extended {
            id: 0,
            payload: build_ut_vtr_handshake(1),
        },
    )
    .await
    .expect("send extension handshake");
    // Identify ourselves so the seeder knows which wallet to sign a receipt
    // for. Without this it would upload bytes nobody can pay for.
    write_message(
        &mut stream,
        &PeerMessage::Extended {
            id: 1,
            payload: build_ut_vtr_address(CLIENT_ADDR),
        },
    )
    .await
    .expect("send client address");

    let mut have = vec![false; pieces.len()];
    let mut received = vec![0usize; pieces.len()];
    let mut data = vec![0u8; total_size];
    let mut peer_address = None;
    let mut receipt = None;
    let mut last_receipt_seen = false;
    let mut bytes = 0usize;

    // Request the first block of every piece, then keep requesting as replies
    // arrive, so a single connection drives the whole download.
    for index in 0..pieces.len() {
        write_message(
            &mut stream,
            &PeerMessage::Request {
                index: index as u32,
                begin: 0,
                length: PIECE_BLOCK,
            },
        )
        .await
        .expect("send request");
    }

    let deadline = Instant::now() + Duration::from_secs(120);
    while have.iter().any(|done| !done) {
        if Instant::now() >= deadline {
            panic!("inbound download did not finish in time");
        }
        let message = read_message(&mut stream, &mut buffer, Duration::from_secs(30))
            .await
            .expect("seeder message");
        match message {
            PeerMessage::Extended { id: 0, payload } => {
                eprintln!(
                    "client: extension handshake {:?}",
                    String::from_utf8_lossy(&payload)
                );
            }
            PeerMessage::Extended { id: 1, payload } => {
                peer_address = Some(parse_ut_vtr_address(&payload).expect("seeder address"));
            }
            PeerMessage::Extended { payload, .. } => {
                if let Ok(envelope) = vtorrent_torrent::metadata::decode_receipt(&payload) {
                    assert!(
                        envelope.verify().expect("verify receipt"),
                        "the daemon sent an invalid receipt"
                    );
                    receipt = Some(envelope.receipt);
                    last_receipt_seen = true;
                }
            }
            PeerMessage::Bitfield { bits } => {
                for (index, byte) in bits.iter().enumerate() {
                    for bit in 0..8 {
                        let piece = index * 8 + bit;
                        if piece < have.len() && (byte & (0x80 >> bit)) != 0 {
                            eprintln!("client: seeder has piece {piece}");
                        }
                    }
                }
            }
            PeerMessage::Unchoke => {}
            PeerMessage::Piece {
                index,
                begin,
                data: block,
            } => {
                let index = index as usize;
                assert!(index < pieces.len(), "seeder sent an out-of-range piece");
                assert!(!have[index], "seeder sent a piece twice");
                // `begin` is the offset within the piece, not within the file.
                let piece_start = index * PIECE_LENGTH;
                let offset = piece_start + begin as usize;
                let piece_len = (total_size - piece_start).min(PIECE_LENGTH);
                assert!(
                    begin as usize + block.len() <= piece_len,
                    "seeder sent a block past the end of piece {index}"
                );
                data[offset..offset + block.len()].copy_from_slice(&block);
                received[index] += block.len();
                bytes += block.len();
                if received[index] >= piece_len {
                    let digest: [u8; 20] =
                        Sha1::digest(&data[piece_start..piece_start + piece_len]).into();
                    assert_eq!(digest, pieces[index], "piece {index} failed verification");
                    have[index] = true;
                } else if !have[index] {
                    write_message(
                        &mut stream,
                        &PeerMessage::Request {
                            index: index as u32,
                            begin: received[index] as u32,
                            length: (piece_len - received[index]).min(PIECE_BLOCK as usize) as u32,
                        },
                    )
                    .await
                    .expect("send follow-up request");
                }
            }
            PeerMessage::KeepAlive | PeerMessage::Interested | PeerMessage::NotInterested => {}
            other => eprintln!("client: ignoring {other:?}"),
        }
    }
    let _ = last_receipt_seen;

    // Receipts are cumulative and rate-limited on our side; keep the connection
    // open until one covers the whole transfer.
    let receipt_deadline = Instant::now() + Duration::from_secs(45);
    while receipt
        .as_ref()
        .is_none_or(|receipt| receipt.uploaded_by_me < TORRENT_SIZE as u64)
    {
        if Instant::now() >= receipt_deadline {
            break;
        }
        match tokio::time::timeout(
            Duration::from_secs(5),
            read_message(&mut stream, &mut buffer, Duration::from_secs(5)),
        )
        .await
        {
            Ok(Ok(PeerMessage::Extended { payload, .. })) => {
                if let Ok(envelope) = vtorrent_torrent::metadata::decode_receipt(&payload) {
                    assert!(
                        envelope.verify().expect("verify receipt"),
                        "the daemon sent an invalid receipt"
                    );
                    receipt = Some(envelope.receipt);
                }
            }
            Ok(Ok(PeerMessage::KeepAlive)) | Err(_) => {}
            Ok(Ok(other)) => eprintln!("client: ignoring {other:?} while waiting"),
            Ok(Err(_)) => break,
        }
    }

    ClientOutcome {
        bytes,
        verified_pieces: have.len(),
        peer_address,
        receipt,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual inbound seeder E2E; requires a netns with VTORRENT_E2E_PUBLIC_IP assigned to lo"]
async fn seeder_serves_a_verified_torrent_to_an_inbound_peer() {
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

    let workdir =
        PathBuf::from(std::env::var("VTORRENT_E2E_WORKDIR").unwrap_or_else(|_| {
            format!("/mnt/temp/vtorrent-ng-e2e/seed-run-{}", std::process::id())
        }));
    std::fs::create_dir_all(&workdir).expect("create workdir");
    let data_dir = workdir.join("node");

    let torrent_port = free_port().await;
    let data = deterministic_data();
    let (tracker_addr, _tracker) = start_tracker(public_ip, torrent_port).await;
    let announce = format!("http://{tracker_addr}/announce");
    let (torrent, info_hash, pieces) = build_torrent(&announce, &data);
    eprintln!(
        "torrent {} ({} pieces, seeder port {torrent_port})",
        hex::encode(info_hash),
        pieces.len()
    );

    // Place the completed torrent in the daemon's download directory first.
    // This is the state a real node reaches after finishing a download, and it
    // also exercises the engine's resume-bitfield verification on the way to
    // registering the torrent for seeding.
    stage_complete_torrent(&data_dir, info_hash, &data);

    let daemon = Daemon::start(&daemon_bin, &data_dir, "seeder.log", torrent_port).await;

    // Fund and unlock the wallet so the daemon can sign receipts.
    daemon
        .post(
            "/api/v1/faucet",
            json!({"address": LEECHER_ADDR, "amount_satoshis": 200u64 * COIN}),
        )
        .await;
    daemon
        .post(
            "/api/v1/wallet/import",
            json!({"wif": LEECHER_WIF, "passphrase": PASS, "overwrite": false}),
        )
        .await;
    daemon
        .post(
            "/api/v1/wallet/unlock",
            json!({"passphrase": PASS, "timeout_secs": 0}),
        )
        .await;

    let settings = daemon.get("/api/v1/torrent/settings").await;
    assert_eq!(
        settings["seeding"], true,
        "the seeder should be enabled: {settings}"
    );

    // Give the daemon the torrent. Its data is already complete on disk, so the
    // engine verifies the resume bitfield, finishes immediately, and the RPC
    // layer registers it for inbound seeding. This is the real completion path.
    let added = daemon
        .post(
            "/api/v1/torrent/add",
            json!({
                "source": base64::engine::general_purpose::STANDARD.encode(&torrent),
                "source_type": "file",
                "wallet_address": LEECHER_ADDR,
            }),
        )
        .await;
    let session_id = added["session_id"].as_str().expect("session id").to_owned();

    let started = Instant::now();
    loop {
        let settings = daemon.get("/api/v1/torrent/settings").await;
        if settings["seeded_torrents"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the daemon never registered the completed torrent: {settings}; {}",
            daemon.logs()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let seeder_addr = SocketAddr::new(IpAddr::V4(public_ip), torrent_port);
    let outcome = download_from_seeder(seeder_addr, info_hash, &pieces, TORRENT_SIZE).await;
    assert_eq!(outcome.bytes, TORRENT_SIZE, "did not download every byte");
    assert_eq!(outcome.verified_pieces, pieces.len());
    assert_eq!(
        outcome.peer_address.as_deref(),
        Some(LEECHER_ADDR),
        "the seeder must advertise the wallet address it signs with"
    );
    eprintln!(
        "downloaded and verified {} bytes over {} pieces",
        outcome.bytes, outcome.verified_pieces
    );

    // Wait for the daemon to finish its own copy, register it, and credit the
    // uploaded bytes.
    let started = Instant::now();
    let sessions = loop {
        let sessions = daemon.get("/api/v1/torrent/sessions").await;
        let uploaded = sessions
            .as_array()
            .and_then(|sessions| {
                sessions
                    .iter()
                    .find(|session| session["id"].as_str() == Some(session_id.as_str()))
            })
            .and_then(|session| session["uploaded_bytes"].as_u64())
            .unwrap_or(0);
        if uploaded >= TORRENT_SIZE as u64 {
            break sessions;
        }
        assert!(
            started.elapsed() < Duration::from_secs(180),
            "the daemon never credited the upload: {sessions}; {}",
            daemon.logs()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let uploaded = sessions
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["id"].as_str() == Some(session_id.as_str()))
        })
        .and_then(|session| session["uploaded_bytes"].as_u64())
        .expect("uploaded bytes");
    assert_eq!(uploaded, TORRENT_SIZE as u64, "unexpected upload total");

    let receipt = outcome
        .receipt
        .expect("the daemon must sign a receipt for the bytes it uploaded");
    assert_eq!(receipt.uploaded_by_me, TORRENT_SIZE as u64);
    eprintln!(
        "daemon signed a receipt for {} bytes addressed to {}",
        receipt.uploaded_by_me, receipt.peer
    );

    // Unknown torrents must not be served.
    let mut rejected = Vec::new();
    let unknown_hash = [0x5au8; 20];
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = TcpStream::connect(seeder_addr).await.expect("connect");
        write_message(
            &mut stream,
            &PeerMessage::Handshake {
                info_hash: unknown_hash,
                peer_id: [0x66u8; 20],
                reserved: [0, 0, 0, 0, 0, 0x10, 0, 0],
            },
        )
        .await
        .expect("handshake");
        let mut buffer = Vec::new();
        read_message(&mut stream, &mut buffer, Duration::from_secs(5)).await
    })
    .await;
    match result {
        // Closed without a handshake reply is the expected refusal.
        Ok(Err(_)) | Err(_) => {}
        Ok(Ok(message)) => rejected.push(format!("{message:?}")),
    }
    assert!(
        rejected.is_empty(),
        "the seeder answered an unknown torrent: {rejected:?}"
    );

    Daemon::stop(daemon).await;
    eprintln!("SEEDER E2E PASS");
    eprintln!("data_dir={}", data_dir.display());
    eprintln!("receipt_signer={}", receipt.signer);
    eprintln!("receipt_peer={}", receipt.peer);
    eprintln!("receipt_bytes={}", receipt.uploaded_by_me);
    eprintln!("now={}", now_secs());
}
