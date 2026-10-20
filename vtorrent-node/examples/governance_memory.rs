//! Allocation and RSS probe for governance undo journals and the reorg backup.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::{BTreeSet, VecDeque},
    sync::atomic::{AtomicUsize, Ordering},
};

use anyhow::{ensure, Result};
use clap::Parser;
use vtorrent_node::{
    block::{Transaction, TxInput, TxOutput, TxType},
    chain::Utxo,
    consensus::COIN,
    governance::{proposal_id_of, GovernanceState, ParamId, Proposal, ProposalRecord, VoteLock},
};

struct Meter;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static OBSERVED_RSS: AtomicUsize = AtomicUsize::new(0);

fn account(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            account(layout.size());
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            account(layout.size());
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(ptr, layout, size) };
        if !next.is_null() {
            if size >= layout.size() {
                account(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        next
    }
}

#[global_allocator]
static ALLOCATOR: Meter = Meter;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 32)]
    proposals: usize,
    #[arg(long, default_value_t = 1000)]
    voters: usize,
    #[arg(long, default_value_t = 100)]
    depth: usize,
    #[arg(long)]
    stake_transfers: bool,
    #[arg(long, default_value_t = 0)]
    votes_per_block: usize,
    #[arg(long)]
    full_snapshots: bool,
    #[arg(long, default_value_t = 256)]
    budget_mib: usize,
    #[arg(long, default_value_t = 2048)]
    limit_mib: usize,
}

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}
fn rss_kib() -> Result<usize> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    Ok(status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0))
}
fn outpoint(n: usize) -> ([u8; 32], u32) {
    let mut txid = [0; 32];
    txid[..8].copy_from_slice(&(n as u64).to_le_bytes());
    (txid, 0)
}

fn seed(args: &Args) -> GovernanceState {
    let mut gov = GovernanceState::new();
    for n in 0..args.proposals {
        let proposal = Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 500 + n as u64,
            activation_height: 5000,
            voting_end: 2881,
            deposit: 100 * COIN,
        };
        let id = proposal_id_of(&proposal, 1);
        gov.proposals.insert(
            id,
            ProposalRecord {
                proposal,
                created_height: 1,
                yes: args.voters as u64 * COIN,
                no: 0,
                abstain: 0,
                decided: false,
                passed: false,
                applied: false,
                activation_failed: false,
                quorum_supply: (args.voters + args.depth * 2 * args.votes_per_block) as u64 * COIN,
                voted_outpoints: (0..args.voters).map(outpoint).collect::<BTreeSet<_>>(),
            },
        );
    }
    for n in 0..args.voters {
        let (txid, vout) = outpoint(n);
        gov.locked_utxos.insert(
            (txid, vout),
            VoteLock {
                txid,
                vout,
                unlock_height: 2887,
                voter: "VMLVUkkn4hJ6Pex3w9RdmdU4BRUarszhHH".to_owned(),
                proposal_id: *gov.proposals.first_key_value().unwrap().0,
                voting_origin: (txid, vout),
                voting_value: COIN,
                voting_origin_height: 0,
            },
        );
    }
    gov
}

fn stake(gov: &mut GovernanceState, step: usize) -> Result<()> {
    let (&(txid, vout), _) = gov.locked_utxos.first_key_value().unwrap();
    let script = vtorrent_script::standard::build_p2pkh(&[7; 20])?
        .as_bytes()
        .to_vec();
    let u = Utxo {
        txid,
        vout,
        value: COIN,
        height: 1,
        timestamp: 1,
        script_pubkey: script.clone(),
    };
    let tx = Transaction {
        version: 1,
        tx_type: TxType::Coinstake,
        inputs: vec![TxInput {
            prev_txid: txid,
            prev_vout: vout,
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        outputs: vec![
            TxOutput {
                value: 0,
                script_pubkey: vec![],
            },
            TxOutput {
                value: COIN,
                script_pubkey: script,
            },
        ],
        lock_time: step as u32,
        claim_address: None,
        claim_signature: None,
    };
    gov.transfer_stake_lock(&tx, &u, 2)?;
    Ok(())
}

fn advance(gov: &mut GovernanceState, args: &Args, step: usize) -> Result<()> {
    if args.stake_transfers {
        stake(gov, step)?;
    }
    for offset in 0..args.votes_per_block {
        let n = step * args.votes_per_block + offset;
        let point = outpoint(args.voters + n);
        let id = *gov.proposals.keys().nth(n % args.proposals).unwrap();
        let record = gov.proposals.get_mut(&id).unwrap();
        ensure!(
            record.voted_outpoints.insert(point),
            "duplicate synthetic vote"
        );
        record.yes += COIN;
        gov.locked_utxos.insert(
            point,
            VoteLock {
                txid: point.0,
                vout: point.1,
                unlock_height: 2887,
                voter: "VMLVUkkn4hJ6Pex3w9RdmdU4BRUarszhHH".to_owned(),
                proposal_id: id,
                voting_origin: point,
                voting_value: COIN,
                voting_origin_height: 0,
            },
        );
    }
    Ok(())
}

fn measure<J: Clone + PartialEq>(
    args: &Args,
    mut capture: impl FnMut(&mut GovernanceState, usize) -> Result<J>,
) -> Result<()> {
    ensure!(
        (1..=128).contains(&args.proposals)
            && (1..=100_000).contains(&args.voters)
            && (1..=100).contains(&args.depth)
            && args.votes_per_block <= 1000,
        "workload exceeds probe limits"
    );
    let limit = args
        .limit_mib
        .checked_mul(1024 * 1024)
        .ok_or_else(|| anyhow::anyhow!("limit overflow"))?;
    ensure!(
        args.budget_mib <= args.limit_mib && args.limit_mib > 0,
        "invalid memory budget"
    );
    let baseline = live();
    let baseline_rss = rss_kib()?;
    let mut gov = seed(args);
    ensure!(
        gov.proposals.len() == args.proposals && gov.locked_utxos.len() == args.voters,
        "incomplete seed"
    );
    ensure!(
        gov.proposals
            .values()
            .all(|r| r.voted_outpoints.len() == args.voters),
        "incomplete vote records"
    );
    let seed_bytes = live() - baseline;
    let mut journals = VecDeque::with_capacity(args.depth.next_power_of_two());
    for step in 0..args.depth * 2 {
        journals.push_back(capture(&mut gov, step)?);
        if journals.len() > args.depth {
            journals.pop_front();
        }
        ensure!(
            live() - baseline < limit,
            "allocation limit reached before full journal rollover"
        );
    }
    ensure!(
        journals.len() == args.depth
            && gov.locked_utxos.len() == args.voters + args.depth * 2 * args.votes_per_block,
        "workload did not complete"
    );
    let steady_bytes = live() - baseline;
    let steady_rss = rss_kib()?;
    // Match the governance copies made by reorganize_to: current state plus journals.
    let backup_gov = gov.clone();
    let backup_journals = journals.clone();
    let reorg_bytes = live() - baseline;
    let reorg_rss = rss_kib()?;
    ensure!(
        reorg_bytes < limit,
        "allocation limit reached while backing up journals"
    );
    ensure!(
        backup_gov == gov && backup_journals == journals,
        "backup differs from live state"
    );
    // Attempt a replacement block while the original branch backups remain live.
    journals.push_back(capture(&mut gov, args.depth * 2)?);
    if journals.len() > args.depth {
        journals.pop_front();
    }
    ensure!(
        live() - baseline < limit,
        "allocation limit reached during replacement-block attempt"
    );
    let replacement_bytes = live() - baseline;
    gov = backup_gov;
    journals = backup_journals;
    let restored_bytes = live() - baseline;
    std::hint::black_box(&gov);
    std::hint::black_box(&journals);
    drop(gov);
    drop(journals);
    let released_bytes = live().saturating_sub(baseline);
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    println!(
        "{}",
        serde_json::json!({
            "proposals": args.proposals, "voters_per_proposal": args.voters, "journal_depth": args.depth,
        "stake_transfers": args.stake_transfers, "completed": true,
        "votes_per_block": args.votes_per_block,
        "journal_kind": if args.full_snapshots { "full_snapshots" } else { "compact_undo" },
            "seed_requested_bytes": seed_bytes, "steady_requested_bytes": steady_bytes,
        "reorg_requested_bytes": reorg_bytes, "restored_requested_bytes": restored_bytes,
        "replacement_requested_bytes": replacement_bytes,
            "released_requested_bytes": released_bytes, "peak_requested_bytes": peak,
        "baseline_rss_kib": baseline_rss, "steady_rss_kib": steady_rss, "reorg_rss_kib": reorg_rss,
        "observed_peak_rss_kib": OBSERVED_RSS.load(Ordering::Relaxed),
        "governance_fits_budget": peak <= args.budget_mib * 1024 * 1024,
            "budget_mib": args.budget_mib,
        })
    );
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.full_snapshots {
        measure(&args, |gov, step| {
            let before = gov.clone();
            advance(gov, &args, step)?;
            OBSERVED_RSS.fetch_max(rss_kib()?, Ordering::Relaxed);
            Ok(before)
        })
    } else {
        measure(&args, |gov, step| {
            let before = gov.clone();
            advance(gov, &args, step)?;
            let undo = gov.undo_since(&before);
            OBSERVED_RSS.fetch_max(rss_kib()?, Ordering::Relaxed);
            Ok(undo)
        })
    }
}
