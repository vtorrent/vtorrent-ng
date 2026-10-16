use super::*;
use vtorrent_p2p::{
    message::{
        decode_for_peer, encode_for_peer, GetProofMsg, NetMessage, ProofMsg, PROTOCOL_VERSION,
    },
    peer::PeerEvent,
    peer_manager::PeerManager,
};
use vtorrent_spv::stake::StakeProof;

const PROOFS: usize = 16;

fn cpu_ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
}

fn distribution(mut values: Vec<u64>) -> Value {
    assert!(!values.is_empty());
    values.sort_unstable();
    json!({ "count": values.len(), "min_us": values[0],
        "median_us": values[values.len() / 2],
        "p95_us": values[(values.len() * 95).div_ceil(100) - 1],
        "max_us": values.last().unwrap(), "samples_us": values })
}

async fn worker(
    address: String,
    targets: Vec<([u8; 32], StakeProof)>,
    ready: Arc<tokio::sync::Barrier>,
) -> Vec<u64> {
    let mut probe = PeerManager::new_testnet(0, "127.0.0.1:0");
    probe.connect(&address).await.unwrap();
    let address = address.parse().unwrap();
    loop {
        if probe.process_events().await.into_iter().any(|event| {
            if let PeerEvent::HandshakeComplete { version, .. } = event {
                assert_eq!(version.version, PROTOCOL_VERSION);
                true
            } else {
                false
            }
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    ready.wait().await;
    ready.wait().await;
    let mut latencies = Vec::new();
    for (hash, expected) in targets {
        let started = Instant::now();
        probe
            .send_to(
                address,
                NetMessage::new(
                    "getproof",
                    encode_for_peer(&GetProofMsg { block_hash: hash }, PROTOCOL_VERSION),
                ),
            )
            .await;
        loop {
            let mut received = false;
            for event in probe.process_events().await {
                match event {
                    PeerEvent::Message { msg, .. } if msg.command_str() == "proof" => {
                        let response: ProofMsg =
                            decode_for_peer(&msg.payload, PROTOCOL_VERSION).unwrap();
                        assert_eq!(response.block_hash, hash);
                        let actual: StakeProof = vtorrent_p2p::message::decode_v2(
                            &response.proof.expect("requested proof missing"),
                        )
                        .unwrap();
                        assert_eq!(actual, expected);
                        received = true;
                    }
                    PeerEvent::Disconnected { .. } => panic!("proof worker disconnected"),
                    _ => {}
                }
            }
            if received {
                latencies.push(started.elapsed().as_micros() as u64);
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    probe.disconnect(address).await;
    latencies
}

async fn wave(
    daemon: &Daemon,
    proofs: &[([u8; 32], StakeProof)],
    peers: usize,
    tip: String,
) -> Value {
    let ready = Arc::new(tokio::sync::Barrier::new(peers + 1));
    let pid = daemon.child.id().unwrap();
    let mut workers = Vec::new();
    for peer in 0..peers {
        let targets = proofs.iter().skip(peer).step_by(peers).cloned().collect();
        workers.push(tokio::spawn(worker(
            daemon.p2p.clone(),
            targets,
            ready.clone(),
        )));
    }
    tokio::time::timeout(Duration::from_secs(20), ready.wait())
        .await
        .expect("proof worker handshakes timed out");
    let cpu_before = cpu_ticks(pid);
    let started = Instant::now();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let done = done.clone();
        let client = daemon.client.clone();
        let url = format!("{}/api/v1/info", daemon.rpc);
        tokio::spawn(async move {
            let mut samples = Vec::new();
            let mut peak_rss = rss(pid);
            while !done.load(Ordering::Relaxed) {
                let request = Instant::now();
                let response = client.get(&url).send().await.unwrap();
                assert!(response.status().is_success());
                let body: Value = response.json().await.unwrap();
                assert_eq!(body["best_block_hash"], tip);
                samples.push(request.elapsed().as_micros() as u64);
                peak_rss = peak_rss.max(rss(pid));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            (samples, peak_rss)
        })
    };
    ready.wait().await;
    let mut samples = Vec::new();
    for task in workers {
        samples.extend(
            tokio::time::timeout(Duration::from_secs(30), task)
                .await
                .expect("proof request wave timed out")
                .unwrap(),
        );
    }
    let elapsed = started.elapsed().as_micros() as u64;
    done.store(true, Ordering::Relaxed);
    let (rpc, peak_rss) = sampler.await.unwrap();
    assert_eq!(samples.len(), PROOFS);
    json!({ "elapsed_us": elapsed, "proofs": distribution(samples),
        "rpc_info": distribution(rpc), "errors": 0,
        "daemon_cpu_ticks": cpu_ticks(pid) - cpu_before,
        "observed_peak_rss_kib": peak_rss })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual release-mode proof request cost experiment"]
async fn canonical_proof_request_cost() {
    assert!(
        !cfg!(debug_assertions),
        "run this experiment with --release"
    );
    let _scenario = RECOVERY_SCENARIO.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let fixture = fixture(true);
    let mut blocks = fixture.winning;
    let mut expected = replay(&blocks);
    let mut proofs = fixture.proofs;
    while expected.best_height() < (PROOFS + 2) as u32 {
        let (block, proof) = pos_block(&expected, 2, vec![]);
        proofs.insert(block.hash(), proof);
        expected.add_block(block.clone()).unwrap();
        blocks.push(block);
    }
    let targets: Vec<_> = blocks
        .iter()
        .skip(2)
        .map(|b| (b.hash(), proofs[&b.hash()].clone()))
        .collect();
    assert_eq!(targets.len(), PROOFS);
    let mut reports = Vec::new();
    for run in 0..5 {
        for peers in [1, 4, 8] {
            let case = directory.path().join(format!("run-{run}-peers-{peers}"));
            persist(&case, &blocks);
            let daemon = Daemon::start_with_activation(&case, "daemon.log", None, Some(2)).await;
            for (hash, _) in &targets {
                assert!(daemon
                    .get(&format!("/api/v1/spv/proof/{}", hex::encode(hash)))
                    .await["proof"]
                    .is_null());
            }
            let cold = wave(
                &daemon,
                &targets,
                peers,
                hex::encode(expected.best_hash().unwrap()),
            )
            .await;
            let warm = wave(
                &daemon,
                &targets,
                peers,
                hex::encode(expected.best_hash().unwrap()),
            )
            .await;
            let log = daemon.logs();
            let reconstruction: Vec<_> = log
                .lines()
                .filter_map(|line| {
                    line.split_once("proof_reconstruction hash=")
                        .map(|(_, s)| s)
                })
                .map(str::to_owned)
                .collect();
            assert_eq!(reconstruction.len(), PROOFS);
            assert!(reconstruction.iter().all(|s| s.contains("found=true")));
            let report = json!({ "run": run, "peers": peers, "utxos": expected.get_utxo_set().len(),
                "cold": cold, "warm": warm, "reconstruction": reconstruction });
            eprintln!(
                "proof_request_cost run={run} peers={peers} cold_us={} warm_us={} cold_rpc_p95_us={} warm_rpc_p95_us={}",
                report["cold"]["elapsed_us"], report["warm"]["elapsed_us"],
                report["cold"]["rpc_info"]["p95_us"], report["warm"]["rpc_info"]["p95_us"]
            );
            reports.push(report);
            daemon.stop(false).await;
        }
    }
    if let Some(path) = std::env::var_os("VTR_PROOF_COST_REPORT") {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        serde_json::to_writer_pretty(file, &reports).unwrap();
    }
}
