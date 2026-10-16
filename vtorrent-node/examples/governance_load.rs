//! Whole-node governance load test driver.
//!
//! Spawns a regtest daemon with the test-only governance activation override,
//! funds proposers, fee payers, and voters through the faucet, submits
//! signed proposals and waves of votes through the public broadcast
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

use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use reqwest::Client;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ops::Range,
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
        proposal_id_of, ParamId, Proposal, SignedVote, Vote, VoteChoice, DEFAULT_LOCK_GRACE_BLOCKS,
        MIN_PROPOSAL_DEPOSIT,
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
    /// Data directory for the daemon. Must not exist or must be empty; the
    /// driver never deletes existing files. Defaults to a fresh temporary
    /// directory removed on success and retained on failure.
    #[arg(long)]
    data_dir: Option<PathBuf>,
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
    /// Proposals with distinct voting deadlines, all voted on by the same cohort.
    #[arg(long, default_value_t = 1)]
    proposals: usize,
    /// Split voters across waves; alternate ascending and descending deadline order.
    #[arg(long, default_value_t = 1)]
    voting_waves: usize,
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
    missed_samples: AtomicU64,
}

impl Rss {
    fn sample(&self) -> Result<u64> {
        let pid = self.pid.load(Ordering::Relaxed);
        ensure!(pid != 0, "no running daemon to sample");
        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .with_context(|| format!("sampling daemon PID {pid}"))?;
        let kib: u64 = status
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
            .context("daemon status has no VmRSS")?;
        ensure!(kib > 0, "daemon VmRSS is zero");
        self.last_kib.store(kib, Ordering::Relaxed);
        self.peak_kib.fetch_max(kib, Ordering::Relaxed);
        Ok(kib)
    }
}

fn spawn_sampler(rss: Arc<Rss>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        if rss.stop.load(Ordering::Relaxed) {
            return;
        }
        if rss.pid.load(Ordering::Relaxed) != 0 && rss.sample().is_err() {
            rss.missed_samples.fetch_add(1, Ordering::Relaxed);
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

impl Daemon {
    fn ensure_running(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            bail!("load-test daemon exited: {status}");
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        self.ensure_running()?;
        let status = Command::new("kill")
            .args(["-TERM", &self.pid.to_string()])
            .status()?;
        ensure!(status.success(), "could not signal load-test daemon");
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait()? {
                ensure!(status.success(), "daemon shutdown failed: {status}");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("daemon did not shut down gracefully within 30 seconds")
    }
}

fn voting_ranges(voters: usize, waves: usize) -> Result<Vec<Range<usize>>> {
    ensure!(
        waves > 0 && waves <= voters,
        "voting waves must be between 1 and the voter count"
    );
    let base = voters / waves;
    let remainder = voters % waves;
    Ok((0..waves)
        .map(|wave| {
            let start = wave * base + wave.min(remainder);
            start..start + base + usize::from(wave < remainder)
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct GovernanceSnapshot {
    height: u64,
    hash: String,
    proposals: Vec<Value>,
    locks: Vec<Value>,
    params: Value,
}

fn number(value: &Value, field: &str) -> Result<u64> {
    value[field]
        .as_u64()
        .with_context(|| format!("missing or invalid {field}"))
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .with_context(|| format!("missing or invalid {field}"))
}

async fn snapshot(client: &Client, base: &str) -> Result<GovernanceSnapshot> {
    for _ in 0..5 {
        let before = get(client, base, "/info").await?;
        let proposals = get(client, base, "/governance/proposals").await?;
        let locks = get(client, base, "/governance/locks").await?;
        let params = get(client, base, "/governance/params").await?;
        let info = get(client, base, "/info").await?;
        let height = number(&info, "block_height")?;
        if number(&proposals, "tip_height")? != height
            || number(&locks, "tip_height")? != height
            || number(&before, "block_height")? != height
            || text(&before, "best_block_hash")? != text(&info, "best_block_hash")?
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        let mut proposals = proposals["proposals"]
            .as_array()
            .context("proposals array missing")?
            .clone();
        let mut locks = locks["locks"]
            .as_array()
            .context("locks array missing")?
            .clone();
        for lock in &mut locks {
            ensure!(
                number(lock, "remaining_blocks")?
                    == number(lock, "unlock_height")?.saturating_sub(height),
                "remaining lock duration disagrees with the snapshot height"
            );
            lock.as_object_mut()
                .context("lock is not an object")?
                .remove("remaining_blocks");
        }
        proposals.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        locks.sort_by(|a, b| {
            (a["txid"].as_str(), a["vout"].as_u64()).cmp(&(b["txid"].as_str(), b["vout"].as_u64()))
        });
        return Ok(GovernanceSnapshot {
            height,
            hash: text(&info, "best_block_hash")?.to_owned(),
            proposals,
            locks,
            params,
        });
    }
    bail!("could not obtain governance state at a consistent tip")
}

struct TrackedProposal {
    id: [u8; 8],
    proposal: Proposal,
    height: u32,
}

struct ExpectedVotes {
    counts: Vec<usize>,
    longest: Vec<Option<usize>>,
    voters: BTreeMap<String, (usize, String)>,
}

impl ExpectedVotes {
    fn cast(&mut self, range: Range<usize>, proposal: usize, tracked: &[TrackedProposal]) {
        self.counts[proposal] += range.len();
        for voter in range {
            if self.longest[voter].is_none_or(|old| {
                tracked[old].proposal.voting_end < tracked[proposal].proposal.voting_end
            }) {
                self.longest[voter] = Some(proposal);
            }
        }
    }
}

fn verify_state(
    state: &GovernanceSnapshot,
    tracked: &[TrackedProposal],
    expected: &ExpectedVotes,
) -> Result<()> {
    ensure!(
        state.proposals.len() == tracked.len(),
        "proposal count mismatch"
    );
    for (i, tracked) in tracked.iter().enumerate() {
        let id = hex::encode(tracked.id);
        let row = state
            .proposals
            .iter()
            .find(|row| row["id"] == id)
            .context("expected proposal missing")?;
        ensure!(
            number(row, "yes")? == expected.counts[i] as u64 * VOTER_MINT_SATS
                && number(row, "no")? == 0
                && number(row, "abstain")? == 0,
            "proposal {id} tally mismatch"
        );
        ensure!(
            number(row, "voting_end")? == u64::from(tracked.proposal.voting_end)
                && number(row, "activation_height")?
                    == u64::from(tracked.proposal.activation_height)
                && number(row, "new_value")? == tracked.proposal.new_value
                && number(row, "created_height")? == u64::from(tracked.height),
            "proposal {id} metadata mismatch"
        );
        ensure!(
            text(row, "param")? == format!("{:?}", tracked.proposal.param),
            "proposal parameter mismatch"
        );
        ensure!(
            state.height < u64::from(tracked.proposal.voting_end)
                && ["decided", "passed", "applied", "activation_failed"]
                    .iter()
                    .all(|field| row[*field].as_bool() == Some(false)),
            "proposal unexpectedly left its open voting window"
        );
    }
    ensure!(
        state.locks.len() == expected.longest.iter().filter(|id| id.is_some()).count(),
        "lock count mismatch"
    );
    let mut seen = std::collections::BTreeSet::new();
    for lock in &state.locks {
        let txid = text(lock, "txid")?;
        ensure!(seen.insert(txid), "duplicate voting lock");
        let (index, owner) = expected
            .voters
            .get(txid)
            .context("lock for unknown voting outpoint")?;
        let p = expected.longest[*index].context("lock for a voter that has not voted")?;
        ensure!(
            number(lock, "vout")? == 0
                && text(lock, "voter")? == owner
                && number(lock, "unlock_height")?
                    == u64::from(tracked[p].proposal.voting_end + DEFAULT_LOCK_GRACE_BLOCKS)
                && text(lock, "proposal_id")? == hex::encode(tracked[p].id),
            "voting lock does not match the longest proposal"
        );
    }
    Ok(())
}

fn record_phase(
    rss: &Rss,
    name: &str,
    state: &GovernanceSnapshot,
    started: Instant,
) -> Result<Value> {
    Ok(
        json!({ "phase": name, "elapsed_secs": started.elapsed().as_secs(),
        "pid": rss.pid.load(Ordering::Relaxed), "tip_height": state.height,
        "tip_hash": state.hash, "rss_kib": rss.sample()?, "locks": state.locks.len(),
        "tallies": state.proposals.iter().map(|p| json!({"id": p["id"], "yes_sats": p["yes"]})).collect::<Vec<_>>() }),
    )
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
    ensure!(
        (1..=100_000).contains(&args.voters),
        "--voters must be between 1 and 100000"
    );
    ensure!(
        (1..=1000).contains(&args.votes_per_tx),
        "--votes-per-tx must be between 1 and 1000"
    );
    ensure!(
        (1..=8).contains(&args.proposals),
        "--proposals must be between 1 and 8"
    );
    let ranges = voting_ranges(args.voters, args.voting_waves)?;
    let base = format!("http://127.0.0.1:{}", args.rpc_port);
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;

    ensure_ports_free(args.rpc_port, args.p2p_port)?;
    // Resolve the data directory without ever deleting existing files. A
    // supplied directory must be empty; the default is a fresh temporary
    // directory held for the run and removed on drop.
    let temp_guard;
    let data_dir = match &args.data_dir {
        Some(dir) => {
            temp_guard = None;
            if dir.exists() {
                if std::fs::read_dir(dir)?.next().is_some() {
                    bail!(
                        "data directory {} is not empty; pass a fresh path (the driver never deletes existing files)",
                        dir.display()
                    );
                }
            } else {
                std::fs::create_dir_all(dir)?;
            }
            dir.clone()
        }
        None => {
            let tmp = tempfile::Builder::new()
                .prefix("vtr-governance-load-")
                .tempdir()?;
            let path = tmp.path().to_path_buf();
            temp_guard = Some(tmp);
            path
        }
    };

    let daemon = spawn_daemon(&args, &data_dir)?;
    let rss = Arc::new(Rss {
        peak_kib: AtomicU64::new(0),
        last_kib: AtomicU64::new(0),
        pid: AtomicU32::new(daemon.pid),
        stop: AtomicBool::new(false),
        missed_samples: AtomicU64::new(0),
    });
    let sampler = spawn_sampler(Arc::clone(&rss));
    let started = Instant::now();

    let result = workload(&args, &data_dir, &base, &client, &rss, daemon, ranges).await;

    rss.stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();

    match result {
        Ok(mut report) => {
            report["duration_secs"] = json!(started.elapsed().as_secs());
            report["data_dir"] = json!(data_dir);
            report["rss_kib"] = json!({ "observed_peak": rss.peak_kib.load(Ordering::Relaxed),
                "last_live_sample": rss.last_kib.load(Ordering::Relaxed),
                "missed_periodic_samples": rss.missed_samples.load(Ordering::Relaxed) });
            let encoded = serde_json::to_string_pretty(&report)?;
            std::fs::write(data_dir.join("report.json"), &encoded)?;
            println!("{encoded}");
            Ok(())
        }
        Err(error) => {
            let retained = temp_guard.map(|tmp| tmp.keep()).unwrap_or(data_dir);
            Err(error.context(format!(
                "load test failed; artifacts retained at {}",
                retained.display()
            )))
        }
    }
}

async fn workload(
    args: &Args,
    data_dir: &std::path::Path,
    base: &str,
    client: &Client,
    rss: &Arc<Rss>,
    mut daemon: Daemon,
    ranges: Vec<Range<usize>>,
) -> Result<Value> {
    let started = Instant::now();
    let mut phases = Vec::new();
    wait_for_rpc(client, base, &mut daemon).await?;
    let initial = snapshot(client, base).await?;
    ensure!(
        initial.height == 0 && initial.proposals.is_empty() && initial.locks.is_empty(),
        "daemon did not start with a fresh chain"
    );
    phases.push(record_phase(rss, "startup", &initial, started)?);
    let staking_sk = key_at(1, 0);
    let staking_address = import_and_start_staking(client, base, &staking_sk).await?;
    faucet(client, base, &staking_address, STAKE_MINT_SATS).await?;
    let mut proposer_mints = Vec::new();
    for i in 0..args.proposals {
        proposer_mints
            .push(faucet(client, base, &address_of(&key_at(2, i)), STAKE_MINT_SATS).await?);
    }
    let batches = args.proposals
        * ranges
            .iter()
            .map(|range| range.len().div_ceil(args.votes_per_tx))
            .sum::<usize>();
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
    let funded = snapshot(client, base).await?;
    phases.push(record_phase(rss, "funded", &funded, started)?);
    // Faucet blocks advance timestamps by at least one second per block.
    let tip = get(client, base, "/blockchain/height").await?;
    let ready_at = number(&tip, "timestamp")?
        .checked_add(number(&funded.params, "min_stake_age")?)
        .and_then(|time| time.checked_add(2))
        .context("maturity timestamp overflow")?;
    let now = u64::from(vtorrent_core::time::now_timestamp_u32());
    let maturity_wait = args.maturity_secs.max(ready_at.saturating_sub(now));
    tokio::time::sleep(Duration::from_secs(maturity_wait)).await;
    let mature = snapshot(client, base).await?;
    phases.push(record_phase(rss, "mature", &mature, started)?);

    let tip = u32::try_from(mature.height).context("tip height exceeds u32")?;
    let mut pending = Vec::new();
    for (i, mint) in proposer_mints.iter().enumerate() {
        let voting_end = tip
            .checked_add(4001 + i as u32 * 100)
            .context("voting height overflow")?;
        let proposal = Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 400 + i as u64,
            voting_end,
            activation_height: voting_end
                .checked_add(3000)
                .context("activation height overflow")?,
            deposit: MIN_PROPOSAL_DEPOSIT,
        };
        let txid = broadcast(
            client,
            base,
            &proposal_transaction(&key_at(2, i), mint, &proposal),
        )
        .await?;
        pending.push((proposal, txid));
    }
    let mut tracked = Vec::new();
    for (proposal, txid) in pending {
        let height = wait_for_confirmation(client, base, &txid, "proposal").await?;
        tracked.push(TrackedProposal {
            id: proposal_id_of(&proposal, height),
            proposal,
            height,
        });
    }
    let mut expected = ExpectedVotes {
        counts: vec![0; args.proposals],
        longest: vec![None; args.voters],
        voters: voters
            .iter()
            .enumerate()
            .map(|(i, (key, mint))| (hex::encode(mint.txid), (i, address_of(key))))
            .collect(),
    };
    let proposals = snapshot(client, base).await?;
    verify_state(&proposals, &tracked, &expected)?;
    phases.push(record_phase(
        rss,
        "proposals_confirmed",
        &proposals,
        started,
    )?);
    let genesis = vtorrent_node::chain::Chain::new_regtest()?
        .best_hash()
        .context("genesis missing")?;
    let mut payer = 0;
    for (wave, range) in ranges.iter().enumerate() {
        let order: Vec<_> = if wave % 2 == 0 {
            (0..tracked.len()).collect()
        } else {
            (0..tracked.len()).rev().collect()
        };
        for p in order {
            let mut txids = Vec::new();
            for cohort in voters[range.clone()].chunks(args.votes_per_tx) {
                let tx = vote_transaction(
                    &key_at(10, payer),
                    &payer_mints[payer],
                    cohort,
                    tracked[p].id,
                    genesis,
                );
                txids.push(broadcast(client, base, &tx).await?);
                payer += 1;
            }
            for txid in txids {
                wait_for_confirmation(client, base, &txid, "vote").await?;
            }
            expected.cast(range.clone(), p, &tracked);
            let state = snapshot(client, base).await?;
            verify_state(&state, &tracked, &expected)?;
            phases.push(record_phase(
                rss,
                &format!("wave_{}_proposal_{}", wave + 1, p + 1),
                &state,
                started,
            )?);
        }
    }
    ensure!(
        payer == batches,
        "payer allocation does not match the voting schedule"
    );
    let tail_deadline = Instant::now()
        .checked_add(Duration::from_secs(args.tail_secs))
        .context("tail duration overflow")?;
    while Instant::now() < tail_deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        daemon.ensure_running()?;
    }

    // ── Restart: verify replay rebuilds the same governance state ────────────
    post(client, base, "/staking/stop", json!({})).await?;
    let before = snapshot(client, base).await?;
    verify_state(&before, &tracked, &expected)?;
    phases.push(record_phase(rss, "before_shutdown", &before, started)?);
    let mut restart_ok = false;
    if args.restart {
        daemon.shutdown().await?;
        rss.pid.store(0, Ordering::Relaxed);
        daemon = spawn_daemon(args, data_dir)?;
        rss.pid.store(daemon.pid, Ordering::Relaxed);
        wait_for_rpc(client, base, &mut daemon).await?;
        let after = snapshot(client, base).await?;
        verify_state(&after, &tracked, &expected)?;
        ensure!(
            before == after,
            "governance state or chain tip changed across graceful restart"
        );
        phases.push(record_phase(rss, "after_restart", &after, started)?);
        restart_ok = true;
    }
    daemon.shutdown().await?;
    rss.pid.store(0, Ordering::Relaxed);
    Ok(
        json!({ "ok": true, "voters": args.voters, "votes_per_tx": args.votes_per_tx,
        "proposals": tracked.iter().map(|p| json!({ "id": hex::encode(p.id), "created_height": p.height,
            "voting_end": p.proposal.voting_end, "unlock_height": p.proposal.voting_end + DEFAULT_LOCK_GRACE_BLOCKS })).collect::<Vec<_>>(),
        "voting_waves": args.voting_waves, "vote_transactions": batches,
        "votes_counted_per_proposal": expected.counts, "locks": before.locks.len(),
        "maturity_wait_secs": maturity_wait, "restart_requested": args.restart,
        "restart_verified": restart_ok, "shutdown_graceful": true, "phases": phases }),
    )
}

/// Fail early if either port is already bound, so the run does not silently
/// talk to another process (for example a soak-fleet docker proxy).
fn ensure_ports_free(rpc_port: u16, p2p_port: u16) -> Result<()> {
    ensure!(
        rpc_port != 0 && p2p_port != 0 && rpc_port != p2p_port,
        "RPC and P2P need different nonzero ports"
    );
    for (name, port) in [("rpc", rpc_port), ("p2p", p2p_port)] {
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => drop(listener),
            Err(error) => bail!("{name} port {port} is not free: {error}"),
        }
    }
    Ok(())
}

fn spawn_daemon(args: &Args, data_dir: &std::path::Path) -> Result<Daemon> {
    ensure_ports_free(args.rpc_port, args.p2p_port)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("daemon.log"))
        .context("opening daemon.log")?;
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
            data_dir.to_str().context("data-dir must be valid UTF-8")?,
        ])
        .env("VTR_GOVERNANCE_TEST_ACTIVATION", "2")
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
        .with_context(|| format!("spawning daemon {}", args.daemon.display()))?;
    let pid = child.id();
    Ok(Daemon { child, pid })
}

async fn wait_for_rpc(client: &Client, base: &str, daemon: &mut Daemon) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        daemon.ensure_running()?;
        if let Ok(value) = get(client, base, "/info").await {
            if value["block_height"].is_u64() {
                daemon.ensure_running()?;
                ensure!(
                    text(&value, "network")? == "vtorrent-regtest",
                    "RPC is not a regtest node"
                );
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
    genesis: [u8; 32],
) -> Transaction {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (GovernanceSnapshot, Vec<TrackedProposal>, ExpectedVotes) {
        let tracked: Vec<_> = (0..2)
            .map(|i| TrackedProposal {
                id: [i as u8 + 1; 8],
                height: 1,
                proposal: Proposal {
                    param: ParamId::PosAnnualRateBps,
                    new_value: 400 + i,
                    voting_end: 4000 + i as u32 * 100,
                    activation_height: 7000 + i as u32 * 100,
                    deposit: MIN_PROPOSAL_DEPOSIT,
                },
            })
            .collect();
        let voters: BTreeMap<_, _> = (0..2)
            .map(|i| (hex::encode([i as u8 + 11; 32]), (i, format!("owner_{i}"))))
            .collect();
        let proposals = tracked.iter().map(|p| json!({
            "id": hex::encode(p.id), "param": "PosAnnualRateBps", "new_value": p.proposal.new_value,
            "created_height": p.height, "voting_end": p.proposal.voting_end,
            "activation_height": p.proposal.activation_height, "yes": 2 * VOTER_MINT_SATS,
            "no": 0, "abstain": 0, "decided": false, "passed": false,
            "applied": false, "activation_failed": false,
        })).collect();
        let locks = voters
            .iter()
            .map(|(id, (_, owner))| {
                json!({ "txid": id, "vout": 0,
            "voter": owner, "proposal_id": hex::encode(tracked[1].id),
            "unlock_height": tracked[1].proposal.voting_end + DEFAULT_LOCK_GRACE_BLOCKS })
            })
            .collect();
        (
            GovernanceSnapshot {
                height: 10,
                hash: "tip10".into(),
                proposals,
                locks,
                params: json!({}),
            },
            tracked,
            ExpectedVotes {
                counts: vec![2, 2],
                longest: vec![Some(1), Some(1)],
                voters,
            },
        )
    }

    #[test]
    fn waves_partition_every_voter_once_without_empty_cohorts() {
        assert_eq!(voting_ranges(5, 2).unwrap(), vec![0..3, 3..5]);
        assert_eq!(voting_ranges(3, 3).unwrap(), vec![0..1, 1..2, 2..3]);
        assert!(voting_ranges(3, 4).is_err());
        assert!(voting_ranges(3, 0).is_err());
    }

    #[test]
    fn oracle_matches_proposals_by_id_not_response_position() {
        let (mut state, tracked, expected) = fixture();
        state.proposals.reverse();
        verify_state(&state, &tracked, &expected).unwrap();
        state.proposals[0]["yes"] = json!(VOTER_MINT_SATS);
        assert!(verify_state(&state, &tracked, &expected)
            .unwrap_err()
            .to_string()
            .contains("tally mismatch"));
    }

    #[test]
    fn oracle_rejects_a_shortened_lock_and_a_duplicate_outpoint() {
        let (mut state, tracked, expected) = fixture();
        state.locks[0]["unlock_height"] =
            json!(tracked[0].proposal.voting_end + DEFAULT_LOCK_GRACE_BLOCKS);
        assert!(verify_state(&state, &tracked, &expected)
            .unwrap_err()
            .to_string()
            .contains("longest proposal"));
        let (mut state, tracked, expected) = fixture();
        state.locks[1] = state.locks[0].clone();
        assert!(verify_state(&state, &tracked, &expected)
            .unwrap_err()
            .to_string()
            .contains("duplicate voting lock"));
    }

    #[test]
    fn expected_state_extends_locks_but_never_shortens_them() {
        let (state, tracked, mut expected) = fixture();
        expected.counts.fill(0);
        expected.longest.fill(None);
        expected.cast(0..1, 0, &tracked);
        assert_eq!(expected.longest[0], Some(0));
        expected.cast(0..1, 1, &tracked);
        expected.cast(1..2, 1, &tracked);
        expected.cast(1..2, 0, &tracked);
        verify_state(&state, &tracked, &expected).unwrap();
    }
}
