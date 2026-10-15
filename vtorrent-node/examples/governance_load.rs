//! Whole-node governance load test driver.
//!
//! Spawns a regtest daemon with the test-only governance activation override,
//! funds a proposer, fee payers, and voters through the faucet, submits a
//! signed proposal and batches of signed votes through the public broadcast
//! endpoint, and samples the daemon's RSS throughout. See
//! `docs/governance-wholenode-load-test.md`.
//!
//! The daemon binary must be built with the `test-util` feature:
//!
//! ```bash
//! cargo build -p vtorrent-daemon --release --features test-util
//! cargo run -p vtorrent-node --release --example governance_load -- \
//!     --daemon target/release/vtorrent-daemon --data-dir /tmp/vtr-load-test
//! ```

use anyhow::{bail, Context, Result};
use clap::Parser;
use reqwest::Client;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use vtorrent_node::{
    block::{Transaction, TxInput, TxOutput, TxType},
    consensus::COIN,
    governance::{
        proposal_id_of, ParamId, Proposal, SignedVote, Vote, VoteChoice, MIN_PROPOSAL_DEPOSIT,
    },
};

/// Per-transaction relay fee (satoshis). Far above any fee floor.
const FEE_SATS: u64 = 10_000;
/// Value carried by each vote's OP_RETURN output. Consensus rejects zero-value
/// outputs; data carriers never enter the UTXO set, so this satoshi is burned.
const VOTE_CARRIER_SATS: u64 = 1;
/// Wallet (staking) and proposer mint amounts.
const STAKE_MINT_SATS: u64 = 1000 * COIN;
const VOTER_MINT_SATS: u64 = 2 * COIN;

#[derive(Parser)]
struct Args {
    /// Path to a daemon binary built with `--features test-util`.
    #[arg(long)]
    daemon: PathBuf,
    #[arg(long, default_value = "/tmp/vtr-governance-load")]
    data_dir: PathBuf,
    #[arg(long, default_value_t = 22825)]
    rpc_port: u16,
    #[arg(long, default_value_t = 22826)]
    p2p_port: u16,
    /// Distinct voting outpoints to create and vote from.
    #[arg(long, default_value_t = 600)]
    voters: usize,
    /// Vote OP_RETURN outputs carried per relayed transaction.
    #[arg(long, default_value_t = 50)]
    votes_per_tx: usize,
    /// Seconds to wait for faucet-minted UTXOs to reach the 60-second
    /// fast-regtest minimum stake age.
    #[arg(long, default_value_t = 70)]
    maturity_secs: u64,
    /// Seconds to keep sampling RSS after the votes confirm, so retained
    /// journals and the staking loop are observed under load.
    #[arg(long, default_value_t = 200)]
    tail_secs: u64,
    /// Restart the daemon after the votes confirm and verify that replay
    /// rebuilds the same governance state from the store.
    #[arg(long)]
    restart: bool,
}

struct Rss {
    peak_kib: AtomicU64,
    last_kib: AtomicU64,
    /// Daemon pid to sample. Updated on restart so sampling follows the process.
    pid: AtomicU32,
    stop: AtomicBool,
}

fn spawn_sampler(rss: Arc<Rss>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        if rss.stop.load(Ordering::Relaxed) {
            return;
        }
        let pid = rss.pid.load(Ordering::Relaxed);
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
            if let Some(kib) = status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            }) {
                rss.last_kib.store(kib, Ordering::Relaxed);
                rss.peak_kib.fetch_max(kib, Ordering::Relaxed);
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    })
}

struct Daemon {
    child: Child,
    pid: u32,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Deterministic key derivation: distinct 32-byte secrets per (prefix, index).
fn key_at(prefix: u8, index: usize) -> secp256k1::SecretKey {
    let mut bytes = [0u8; 32];
    bytes[0] = prefix;
    bytes[1] = (index >> 16) as u8;
    bytes[2] = (index >> 8) as u8;
    bytes[3] = index as u8;
    bytes[31] = 0x5A;
    secp256k1::SecretKey::from_slice(&bytes).expect("nonzero secret")
}

fn address_of(sk: &secp256k1::SecretKey) -> String {
    let secp = secp256k1::Secp256k1::new();
    let pk = secp256k1::PublicKey::from_secret_key(&secp, sk);
    vtorrent_core::address::Address::from_pubkey(&pk, true, 70).to_string()
}

fn p2pkh_script(address: &str) -> Vec<u8> {
    vtorrent_core::address::Address::parse(address)
        .expect("driver-generated address must parse")
        .p2pkh_script_pubkey()
}

fn sign_p2pkh(tx: &mut Transaction, sk: &secp256k1::SecretKey, script_pubkey: &[u8]) {
    let secp = secp256k1::Secp256k1::new();
    let mut sig = secp
        .sign_ecdsa(
            &secp256k1::Message::from_digest(tx.sighash(0, script_pubkey)),
            sk,
        )
        .serialize_der()
        .to_vec();
    sig.push(0x01);
    let mut script = vtorrent_script::Script::new();
    script.push_data(&sig).expect("sig fits pushdata");
    script
        .push_data(&secp256k1::PublicKey::from_secret_key(&secp, sk).serialize())
        .expect("pubkey fits pushdata");
    tx.inputs[0].script_sig = script.as_bytes().to_vec();
}

fn carrier_script(payload: &[u8]) -> Vec<u8> {
    let mut script = vtorrent_script::Script::new();
    script.push_opcode(0x6a);
    script.push_data(payload).expect("payload fits pushdata");
    script.as_bytes().to_vec()
}

struct Mint {
    txid: [u8; 32],
    amount: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.voters == 0 || args.votes_per_tx == 0 {
        bail!("--voters and --votes-per-tx must be positive");
    }
    let base = format!("http://127.0.0.1:{}", args.rpc_port);
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;

    std::fs::create_dir_all(&args.data_dir)?;
    for entry in [
        "blocks",
        "chain.redb",
        "mempool.json",
        "wallet.json",
        "staking.json",
    ] {
        let path = args.data_dir.join(entry);
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }

    let daemon = spawn_daemon(&args)?;
    let rss = Arc::new(Rss {
        peak_kib: AtomicU64::new(0),
        last_kib: AtomicU64::new(0),
        pid: AtomicU32::new(daemon.pid),
        stop: AtomicBool::new(false),
    });
    let sampler = spawn_sampler(Arc::clone(&rss));
    let started = Instant::now();

    let result = workload(&args, &base, &client, &rss, daemon).await;

    rss.stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();

    let report = json!({
        "voters": args.voters,
        "votes_per_tx": args.votes_per_tx,
        "duration_secs": started.elapsed().as_secs(),
        "rss_kib": {
            "peak": rss.peak_kib.load(Ordering::Relaxed),
            "final": rss.last_kib.load(Ordering::Relaxed),
        },
        "ok": result.is_ok(),
    });
    result?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn workload(
    args: &Args,
    base: &str,
    client: &Client,
    rss: &Arc<Rss>,
    daemon: Daemon,
) -> Result<()> {
    let pid_sample = |rss: &Arc<Rss>| rss.last_kib.load(Ordering::Relaxed);
    wait_for_rpc(client, base).await?;
    let startup_rss = pid_sample(rss);

    // ── Wallet: import a staking key, unlock, mint, start staking ────────────
    let staking_sk = key_at(1, 0);
    let staking_address = import_and_start_staking(client, base, &staking_sk).await?;
    faucet(client, base, &staking_address, STAKE_MINT_SATS).await?;

    // ── Funding: proposer, fee payers, voters ────────────────────────────────
    let proposer_sk = key_at(2, 0);
    let proposer_mint = faucet(client, base, &address_of(&proposer_sk), STAKE_MINT_SATS).await?;

    let batches = args.voters.div_ceil(args.votes_per_tx);
    let mut payer_mints = Vec::with_capacity(batches);
    for i in 0..batches {
        payer_mints.push(faucet(client, base, &address_of(&key_at(10, i)), VOTER_MINT_SATS).await?);
    }
    let mut voters: Vec<(secp256k1::SecretKey, Mint)> = Vec::with_capacity(args.voters);
    for i in 0..args.voters {
        let sk = key_at(100, i);
        let mint = faucet(client, base, &address_of(&sk), VOTER_MINT_SATS).await?;
        voters.push((sk, mint));
    }
    let funded_rss = pid_sample(rss);
    let funded_at = Instant::now();

    // ── Maturity: fast-regtest minimum stake age is 60 seconds ───────────────
    tokio::time::sleep(Duration::from_secs(args.maturity_secs)).await;

    // ── Proposal ─────────────────────────────────────────────────────────────
    let tip: u32 = get(client, base, "/info").await?["block_height"]
        .as_u64()
        .context("info.block_height")? as u32;
    // Window tolerance: the proposal validates `voting_end - inclusion_height`
    // within [2880, 10080]; aiming at tip + 4001 tolerates ~1100 blocks of
    // staking delay in either direction.
    let proposal = Proposal {
        param: ParamId::PosAnnualRateBps,
        new_value: 400,
        voting_end: tip + 1 + 4000,
        activation_height: tip + 1 + 4000 + 3000,
        deposit: MIN_PROPOSAL_DEPOSIT,
    };
    let proposal_txid = broadcast(
        client,
        base,
        &proposal_transaction(&proposer_sk, &proposer_mint, &proposal),
    )
    .await?;
    println!("proposal broadcast: {proposal_txid}");
    let proposal_height = wait_for_confirmation(client, base, &proposal_txid, "proposal").await?;
    let proposal_id = proposal_id_of(&proposal, proposal_height);
    println!(
        "proposal confirmed at height {proposal_height}, id {}",
        hex::encode(proposal_id)
    );

    // ── Votes: one relayed transaction per batch of voters ───────────────────
    let mut vote_txids = Vec::with_capacity(batches);
    for (batch, payer_mint) in payer_mints.iter().enumerate() {
        let range = batch * args.votes_per_tx..((batch + 1) * args.votes_per_tx).min(args.voters);
        let tx = vote_transaction(
            &key_at(10, batch),
            payer_mint,
            &voters[range.clone()],
            proposal_id,
        );
        vote_txids.push(broadcast(client, base, &tx).await?);
    }
    println!("{} vote transactions broadcast", vote_txids.len());
    let voted_at = Instant::now();
    for txid in &vote_txids {
        wait_for_confirmation(client, base, txid, "vote").await?;
    }

    // ── Verify tallies and locks through the authoritative RPC state ─────────
    let proposals = get(client, base, "/governance/proposals").await?;
    let locks = get(client, base, "/governance/locks").await?;
    let expected_yes = (args.voters as u64) * VOTER_MINT_SATS;
    let yes = proposals["proposals"][0]["yes"].as_u64().unwrap_or(0);
    let lock_count = locks["locks"].as_array().map(|a| a.len()).unwrap_or(0);
    if yes != expected_yes {
        bail!("vote tally mismatch: expected {expected_yes}, got {yes}");
    }
    if lock_count != args.voters {
        bail!(
            "lock count mismatch: expected {}, got {lock_count}",
            args.voters
        );
    }

    // ── Tail: keep sampling while the staking loop accumulates journals ──────
    let tail_deadline = Instant::now() + Duration::from_secs(args.tail_secs);
    while Instant::now() < tail_deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        pid_sample(rss);
    }

    // ── Restart: verify replay rebuilds the same governance state ────────────
    let mut restart_ok = None;
    if args.restart {
        let before_proposals = get(client, base, "/governance/proposals").await?;
        let before_locks = get(client, base, "/governance/locks").await?;
        drop(daemon);
        let restarted = spawn_daemon(args)?;
        rss.pid.store(restarted.pid, Ordering::Relaxed);
        wait_for_rpc(client, base).await?;
        let after_proposals = get(client, base, "/governance/proposals").await?;
        let after_locks = get(client, base, "/governance/locks").await?;
        let proposals_match = before_proposals["proposals"] == after_proposals["proposals"];
        let locks_match = before_locks["locks"] == after_locks["locks"];
        if !proposals_match || !locks_match {
            bail!(
                "governance state changed across restart: proposals_match={proposals_match} locks_match={locks_match}"
            );
        }
        restart_ok = Some(true);
        // `restarted` drops here, killing the restarted daemon.
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "voters": args.voters,
            "votes_per_tx": args.votes_per_tx,
            "batches": batches,
            "expected_yes_sats": expected_yes,
            "counted_yes_sats": yes,
            "locks": lock_count,
            "proposal_height": proposal_height,
            "duration_secs": Instant::now().duration_since(funded_at).as_secs(),
            "vote_confirm_secs": (voted_at - funded_at).as_secs(),
            "restart_verified": restart_ok,
            "rss_kib": {
                "startup": startup_rss,
                "after_funding": funded_rss,
                "peak": rss.peak_kib.load(Ordering::Relaxed),
                "final": rss.last_kib.load(Ordering::Relaxed),
            },
        }))?
    );
    Ok(())
}

fn spawn_daemon(args: &Args) -> Result<Daemon> {
    let log =
        std::fs::File::create(args.data_dir.join("daemon.log")).context("creating daemon.log")?;
    let child = Command::new(&args.daemon)
        .args([
            "--regtest",
            "--regtest-fast-stake",
            "--testnet",
            "--isolated",
            "--no-dht",
            "--rpc-addr",
            &format!("127.0.0.1:{}", args.rpc_port),
            "--listen",
            &format!("127.0.0.1:{}", args.p2p_port),
            "--public-addr",
            &format!("127.0.0.1:{}", args.p2p_port),
            "--log-level",
            "info",
            "--data-dir",
            args.data_dir
                .to_str()
                .context("data-dir must be valid UTF-8")?,
        ])
        .env("VTR_GOVERNANCE_TEST_ACTIVATION", "2")
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
        .with_context(|| format!("spawning daemon {}", args.daemon.display()))?;
    let pid = child.id();
    Ok(Daemon { child, pid })
}

async fn wait_for_rpc(client: &Client, base: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(value) = get(client, base, "/info").await {
            if value["block_height"].is_u64() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    bail!("daemon RPC did not become ready within 60s")
}

async fn get(client: &Client, base: &str, path: &str) -> Result<Value> {
    let response = client
        .get(format!("{base}/api/v1{path}"))
        .send()
        .await
        .with_context(|| format!("GET {path}"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .with_context(|| format!("GET {path} body"))?;
    if !status.is_success() {
        bail!("GET {path} failed: {status}: {value}");
    }
    Ok(value)
}

async fn post(client: &Client, base: &str, path: &str, body: Value) -> Result<Value> {
    let response = client
        .post(format!("{base}/api/v1{path}"))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {path}"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .with_context(|| format!("POST {path} body"))?;
    if !status.is_success() {
        bail!("POST {path} failed: {status}: {value}");
    }
    Ok(value)
}

async fn faucet(client: &Client, base: &str, address: &str, amount: u64) -> Result<Mint> {
    for attempt in 0..3 {
        match post(
            client,
            base,
            "/faucet",
            json!({ "address": address, "amount_satoshis": amount }),
        )
        .await
        {
            Ok(value) => {
                let txid_hex = value["txid"].as_str().context("faucet.txid")?;
                let mut txid = [0u8; 32];
                hex::decode_to_slice(txid_hex, &mut txid)?;
                return Ok(Mint { txid, amount });
            }
            Err(error) if attempt == 2 => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    unreachable!("faucet retry loop");
}

async fn import_and_start_staking(
    client: &Client,
    base: &str,
    sk: &secp256k1::SecretKey,
) -> Result<String> {
    let wif = vtorrent_core::keys::PrivateKey::from_bytes(sk.secret_bytes(), true)
        .context("staking key bytes")?
        .to_wif(198)
        .to_string();
    let imported = post(
        client,
        base,
        "/wallet/import",
        json!({ "wif": wif, "passphrase": "governance load test" }),
    )
    .await?;
    let address = imported["address"]
        .as_str()
        .context("import.address")?
        .to_string();
    post(
        client,
        base,
        "/wallet/unlock",
        json!({ "passphrase": "governance load test", "timeout_secs": 0 }),
    )
    .await?;
    post(
        client,
        base,
        "/staking/start",
        json!({ "address": address, "passphrase": "governance load test" }),
    )
    .await?;
    Ok(address)
}

fn proposal_transaction(
    sk: &secp256k1::SecretKey,
    mint: &Mint,
    proposal: &Proposal,
) -> Transaction {
    let script_pubkey = p2pkh_script(&address_of(sk));
    let change = mint.amount - proposal.deposit - FEE_SATS;
    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: mint.txid,
            prev_vout: 0,
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        outputs: vec![
            TxOutput {
                value: proposal.deposit,
                script_pubkey: carrier_script(&proposal.encode()),
            },
            TxOutput {
                value: change,
                script_pubkey: script_pubkey.clone(),
            },
        ],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    sign_p2pkh(&mut tx, sk, &script_pubkey);
    tx
}

fn vote_transaction(
    payer_sk: &secp256k1::SecretKey,
    payer_mint: &Mint,
    voters: &[(secp256k1::SecretKey, Mint)],
    proposal_id: [u8; 8],
) -> Transaction {
    let genesis = vtorrent_node::chain::Chain::new_regtest()
        .expect("deterministic regtest genesis")
        .best_hash()
        .expect("genesis hash");
    let script_pubkey = p2pkh_script(&address_of(payer_sk));
    let carriers = voters.len() as u64 * VOTE_CARRIER_SATS;
    let change = payer_mint.amount - carriers - FEE_SATS;
    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: payer_mint.txid,
            prev_vout: 0,
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        outputs: voters
            .iter()
            .map(|(sk, mint)| {
                let vote = SignedVote::sign(
                    Vote {
                        proposal_id,
                        choice: VoteChoice::Yes,
                    },
                    (mint.txid, 0),
                    genesis,
                    sk,
                );
                TxOutput {
                    value: VOTE_CARRIER_SATS,
                    script_pubkey: vote.script().expect("vote payload fits pushdata"),
                }
            })
            .chain(std::iter::once(TxOutput {
                value: change,
                script_pubkey: script_pubkey.clone(),
            }))
            .collect(),
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    sign_p2pkh(&mut tx, payer_sk, &script_pubkey);
    tx
}

async fn broadcast(client: &Client, base: &str, tx: &Transaction) -> Result<String> {
    let raw = hex::encode(bincode::serialize(tx).expect("transaction serializes"));
    let value = post(
        client,
        base,
        "/blockchain/broadcast",
        json!({ "raw_tx": raw }),
    )
    .await?;
    Ok(value["txid"]
        .as_str()
        .context("broadcast.txid")?
        .to_string())
}

async fn wait_for_confirmation(client: &Client, base: &str, txid: &str, kind: &str) -> Result<u32> {
    let deadline = Instant::now() + Duration::from_secs(600);
    while Instant::now() < deadline {
        if let Ok(value) = get(client, base, &format!("/blockchain/tx/{txid}")).await {
            if let Some(height) = value["block_height"].as_u64() {
                return Ok(height as u32);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("{kind} transaction {txid} was not confirmed within 600s")
}
