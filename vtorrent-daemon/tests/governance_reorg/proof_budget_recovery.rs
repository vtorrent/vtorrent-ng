use super::*;
use std::{collections::BTreeSet, net::SocketAddr};
use vtorrent_p2p::{
    message::{
        decode_for_peer, encode_for_peer, GetProofMsg, NetMessage, ProofMsg, PROTOCOL_VERSION,
    },
    peer::PeerEvent,
    peer_manager::PeerManager,
};
use vtorrent_spv::stake::StakeProof;

const ADMISSIONS: usize = 100;

struct Probe {
    manager: PeerManager,
    provider: SocketAddr,
    ignored_requests: Vec<[u8; 32]>,
}

impl Probe {
    async fn connect(provider: &str) -> Self {
        let mut manager = PeerManager::new_testnet(0, "127.0.0.1:0");
        manager.connect(provider).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if manager.process_events().await.into_iter().any(|event| {
                    if let PeerEvent::HandshakeComplete { version, .. } = event {
                        assert_eq!(version.version, PROTOCOL_VERSION);
                        true
                    } else {
                        false
                    }
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        Self {
            manager,
            provider: provider.parse().unwrap(),
            ignored_requests: Vec::new(),
        }
    }

    async fn request(&mut self, hash: [u8; 32]) -> StakeProof {
        self.manager
            .send_to(
                self.provider,
                NetMessage::new(
                    "getproof",
                    encode_for_peer(&GetProofMsg { block_hash: hash }, PROTOCOL_VERSION),
                ),
            )
            .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                for event in self.manager.process_events().await {
                    match event {
                        PeerEvent::Message { msg, .. } if msg.command_str() == "proof" => {
                            let response: ProofMsg =
                                decode_for_peer(&msg.payload, PROTOCOL_VERSION).unwrap();
                            assert_eq!(response.block_hash, hash);
                            return vtorrent_p2p::message::decode_v2(
                                &response
                                    .proof
                                    .expect("the first 100 cold requests must be admitted"),
                            )
                            .unwrap();
                        }
                        PeerEvent::Disconnected { .. } => panic!("drain peer disconnected"),
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("cold proof request timed out")
    }

    async fn observe(&mut self) {
        for event in self.manager.process_events().await {
            match event {
                PeerEvent::Message { msg, .. } if msg.command_str() == "getproof" => {
                    let req: GetProofMsg = decode_for_peer(&msg.payload, PROTOCOL_VERSION).unwrap();
                    self.ignored_requests.push(req.block_hash);
                }
                PeerEvent::Disconnected { .. } => panic!("injecting peer disconnected"),
                _ => {}
            }
        }
    }
}

fn count(log: &str, hash: [u8; 32], marker: &str, outcome: Option<bool>) -> usize {
    let prefix = format!("{marker} hash={}", hex::encode(hash));
    log.lines()
        .filter(|line| {
            line.contains(&prefix)
                && outcome.is_none_or(|found| line.contains(&format!("found={found}")))
        })
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual release-mode test of real quota expiry and 65-second retry deadlines"]
async fn daemon_proof_quota_expires_and_automatic_retry_completes() {
    assert!(!cfg!(debug_assertions), "run with --release");
    let _scenario = RECOVERY_SCENARIO.lock().await;
    let directory = tempfile::Builder::new()
        .prefix("vtr-proof-budget-recovery-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("Proof quota recovery artifacts: {}", directory.display());
    let fixture = fixture(true);
    let mut blocks = fixture.winning;
    let mut expected = replay(&blocks);
    let mut proofs = fixture.proofs;
    while expected.best_height() < ADMISSIONS as u32 + 2 {
        let (block, proof) = pos_block(&expected, 2, vec![]);
        proofs.insert(block.hash(), proof);
        expected.add_block(block.clone()).unwrap();
        blocks.push(block);
    }
    let targets: BTreeSet<_> = blocks.iter().skip(2).map(Block::hash).collect();
    assert_eq!(targets.len(), ADMISSIONS);
    let (target, target_proof) = pos_block(&expected, 2, vec![]);
    expected.add_block(target.clone()).unwrap();
    let provider_dir = directory.join("provider");
    let receiver_dir = directory.join("receiver");
    persist(&provider_dir, &blocks);
    persist(&receiver_dir, &blocks);
    let mut provider =
        Daemon::start_with_activation(&provider_dir, "daemon.log", None, Some(2)).await;
    let mut receiver =
        Daemon::start_with_activation(&receiver_dir, "daemon.log", Some(&provider.p2p), Some(2))
            .await;
    let mut probe = Probe::connect(&provider.p2p).await;
    let drain_started = Instant::now();
    tokio::time::timeout(Duration::from_secs(45), async {
        for block in blocks.iter().skip(2) {
            assert_eq!(probe.request(block.hash()).await, proofs[&block.hash()]);
        }
    })
    .await
    .expect("quota drain must finish before the oldest admissions expire");
    let drain_ms = drain_started.elapsed().as_millis();
    let logs = provider.logs();
    for hash in &targets {
        assert_eq!(count(&logs, *hash, "proof_reconstruction", Some(true)), 1);
    }
    assert_eq!(
        logs.lines()
            .filter(|line| line.contains("proof_reconstruction hash="))
            .count(),
        ADMISSIONS
    );
    let path = format!("/api/v1/spv/proof/{}", hex::encode(target.hash()));
    let injected = Instant::now();
    probe
        .manager
        .send_to(
            probe.provider,
            NetMessage::new("block", encode_for_peer(&target, PROTOCOL_VERSION)),
        )
        .await;
    provider.wait_tip(&hex::encode(target.hash())).await;
    receiver.wait_tip(&hex::encode(target.hash())).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            probe.observe().await;
            if count(
                &provider.logs(),
                target.hash(),
                "proof_reconstruction",
                Some(false),
            ) == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the receiver's initial proof request must hit the exhausted provider quota");
    let denied_ms = injected.elapsed().as_millis();
    assert_eq!(
        count(
            &provider.logs(),
            target.hash(),
            "proof_reconstruction_denied",
            None
        ),
        1
    );
    assert!(provider.get(&path).await["proof"].is_null());
    assert!(receiver.get(&path).await["proof"].is_null());
    assert_eq!(
        count(
            &provider.logs(),
            target.hash(),
            "proof_request_received",
            None
        ),
        1
    );
    eprintln!("Verified 100 cold admissions in {drain_ms} ms; target denied at {denied_ms} ms");
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            assert!(
                provider.child.try_wait().unwrap().is_none(),
                "provider exited: {}",
                provider.logs()
            );
            assert!(
                receiver.child.try_wait().unwrap().is_none(),
                "receiver exited: {}",
                receiver.logs()
            );
            probe.observe().await;
            let response = receiver.get(&path).await;
            if !response["proof"].is_null() {
                assert_eq!(
                    serde_json::from_value::<StakeProof>(response["proof"].clone()).unwrap(),
                    target_proof
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "automatic retry failed\nprovider: {}\nreceiver: {}",
            provider.logs(),
            receiver.logs()
        )
    });
    let recovered_ms = injected.elapsed().as_millis();
    assert!(
        recovered_ms >= 65_000,
        "proof appeared before the actual retry deadline"
    );
    assert_eq!(
        count(
            &provider.logs(),
            target.hash(),
            "proof_request_received",
            None
        ),
        2
    );
    assert_eq!(
        count(
            &provider.logs(),
            target.hash(),
            "proof_reconstruction",
            Some(true)
        ),
        1
    );
    assert_eq!(
        serde_json::from_value::<StakeProof>(provider.get(&path).await["proof"].clone()).unwrap(),
        target_proof
    );
    eprintln!("Automatic retry cached the exact target proof at {recovered_ms} ms; watching the next deadline");
    let observation_deadline = injected.elapsed() + Duration::from_secs(67);
    while injected.elapsed() < observation_deadline {
        probe.observe().await;
        assert!(provider.child.try_wait().unwrap().is_none());
        assert!(receiver.child.try_wait().unwrap().is_none());
        assert_eq!(
            count(
                &provider.logs(),
                target.hash(),
                "proof_request_received",
                None
            ),
            2,
            "a completed proof request must not be retried"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let observed_ms = injected.elapsed().as_millis();
    assert_eq!(
        serde_json::from_value::<StakeProof>(receiver.get(&path).await["proof"].clone()).unwrap(),
        target_proof
    );
    assert!(probe.ignored_requests.contains(&target.hash()));
    assert_governance(&receiver, &expected).await;
    receiver.persisted(103).await;
    provider.persisted(103).await;
    probe.manager.disconnect(probe.provider).await;
    receiver.stop(false).await;
    provider.stop(false).await;
    assert_persisted_governance(&receiver_dir, &expected);
    assert_persisted_governance(&provider_dir, &expected);
    let report = json!({ "cold_admissions": ADMISSIONS, "quota_drain_ms": drain_ms,
        "initial_denial_ms": denied_ms, "automatic_recovery_ms": recovered_ms,
        "observed_after_injection_ms": observed_ms, "target_height": 103,
        "target_hash": hex::encode(target.hash()), "target_requests": 2,
        "target_denials": 1, "target_reconstructions": 1 });
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("Real-time quota recovery verified: {report}");
}
