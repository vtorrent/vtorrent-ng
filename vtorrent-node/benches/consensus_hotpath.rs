use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use vtorrent_node::block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType};
use vtorrent_node::chain::{Chain, Utxo};
use vtorrent_node::consensus::{
    check_stake_kernel, compute_pos_reward, compute_stake_modifier, stake_kernel_hash, COIN,
};
use vtorrent_node::staking::StakingEngine;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_utxo(value: u64, age_seconds: u32) -> Utxo {
    let now = 1_700_000_000u32;
    Utxo {
        txid: [0xAB; 32],
        vout: 0,
        value,
        script_pubkey: vec![
            0x76, 0xa9, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0xac,
        ],
        height: 1,
        timestamp: now.saturating_sub(age_seconds),
    }
}

fn make_txouts(n: usize) -> Vec<TxOutput> {
    (0..n)
        .map(|i| TxOutput {
            value: (i as u64 + 1) * COIN,
            script_pubkey: vec![
                0x76, 0xa9, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0xac,
            ],
        })
        .collect()
}

fn make_pending_txs(count: usize) -> Vec<Transaction> {
    (0..count)
        .map(|i| Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: [i as u8; 32],
                prev_vout: 0,
                script_sig: vec![],
                sequence: 0xffffffff,
            }],
            outputs: make_txouts(2),
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        })
        .collect()
}

fn funded_chain(coins: u64) -> Chain {
    let mut chain = Chain::new().expect("genesis");
    chain
        .mint_to_address("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT", coins)
        .expect("mint");
    chain
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

/// Pure-function benchmarks: no heap, no I/O — baseline for comparison.
fn bench_pure_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("pure_functions");

    let stake_modifier: u64 = 0xDEAD_BEEF_CAFE_BABE;
    let utxo = make_utxo(10_000 * COIN, 7 * 24 * 3600);
    let block_hash = [0x42u8; 32];

    group.bench_function("compute_stake_modifier", |b| {
        b.iter(|| compute_stake_modifier(black_box(stake_modifier), black_box(&block_hash)))
    });

    group.bench_function("stake_kernel_hash", |b| {
        b.iter(|| {
            stake_kernel_hash(
                black_box(stake_modifier),
                black_box(&utxo),
                black_box(1_700_000_000),
            )
        })
    });

    group.bench_function("check_stake_kernel", |b| {
        b.iter(|| {
            check_stake_kernel(
                black_box(stake_modifier),
                black_box(&utxo),
                black_box(1_700_000_000),
            )
        })
    });

    group.bench_function("compute_pos_reward_10k_vtr_7d", |b| {
        b.iter(|| compute_pos_reward(black_box(10_000 * COIN), black_box(7 * 24 * 3600)))
    });

    group.finish();
}

/// build_stake_block: varying eligible UTXO counts and pending tx counts.
fn bench_build_stake_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_stake_block");
    let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
    let prev_hash = [0x11u8; 32];
    let prev_stake_modifier = 12345u64;
    let height = 100u32;
    let now = 1_700_100_000u32;

    for utxo_count in [1, 10, 50] {
        for pending_count in [0, 10, 50] {
            let utxos: Vec<Utxo> = (0..utxo_count)
                .map(|i| {
                    let mut u = make_utxo(10_000 * COIN, 7 * 24 * 3600);
                    u.txid = [i as u8; 32];
                    u
                })
                .collect();
            let pending = make_pending_txs(pending_count);

            group.bench_with_input(
                BenchmarkId::new("utxos_pending", format!("{utxo_count}u_{pending_count}tx")),
                &(utxos, pending),
                |b, (utxos, pending)| {
                    b.iter(|| {
                        engine.build_stake_block(
                            black_box(prev_hash),
                            black_box(prev_stake_modifier),
                            black_box(height),
                            black_box(now),
                            black_box(utxos.clone()),
                            black_box(pending.clone()),
                        )
                    })
                },
            );
        }
    }
    group.finish();
}

/// Chain::add_block: the main hot path.
fn bench_add_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("chain_add_block");

    for tx_count in [1, 10, 50] {
        // We benchmark by minting coins (coinbase) and then adding a second
        // block.  The cost we care about is UTXO journaling + merkle root.
        group.bench_with_input(
            BenchmarkId::new("coinbase", format!("{tx_count}tx")),
            &tx_count,
            |b, &tx_count| {
                b.iter_batched(
                    || {
                        // Fresh chain per iteration — measures genesis + mint + block build.
                        funded_chain(1_000_000 * COIN)
                    },
                    |mut chain| {
                        // Build a block with tx_count outputs via mint_to_address.
                        // This exercises add_block → apply_block_journaled.
                        let amount = (tx_count as u64) * COIN;
                        let _ = black_box(
                            chain.mint_to_address("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT", amount),
                        );
                        chain
                    },
                    criterion::BatchSize::SmallInput,
                )
            },
        );
    }
    group.finish();
}

/// Merkle root computation at different transaction counts.
fn bench_merkle_root(c: &mut Criterion) {
    let mut group = c.benchmark_group("merkle_root");

    for tx_count in [1, 10, 50, 100] {
        let txs: Vec<Transaction> = (0..tx_count)
            .map(|i| Transaction {
                version: 1,
                tx_type: TxType::Standard,
                inputs: vec![TxInput {
                    prev_txid: [i as u8; 32],
                    prev_vout: 0,
                    script_sig: vec![],
                    sequence: 0xffffffff,
                }],
                outputs: make_txouts(2),
                lock_time: 0,
                claim_address: None,
                claim_signature: None,
            })
            .collect();

        let block = Block {
            header: BlockHeader {
                version: 2,
                prev_block_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                timestamp: 1_700_000_000,
                bits: 0x1e0fffff,
                nonce: 0,
                stake_modifier: 0,
            },
            transactions: txs,
        };

        group.bench_with_input(
            BenchmarkId::new("compute", format!("{tx_count}tx")),
            &block,
            |b, block| b.iter(|| black_box(block.compute_merkle_root())),
        );
    }
    group.finish();
}

/// Sighash computation — the per-input hot path in script verification.
fn bench_sighash(c: &mut Criterion) {
    let mut group = c.benchmark_group("sighash");

    let subscript: Vec<u8> = vec![0x76, 0xa9, 0x14, 0xcc]
        .into_iter()
        .chain(std::iter::repeat_n(0xcc, 21))
        .collect();

    for input_count in [1, 5, 20] {
        let tx = Transaction {
            version: 2,
            tx_type: TxType::Standard,
            inputs: (0..input_count)
                .map(|i| TxInput {
                    prev_txid: [i as u8; 32],
                    prev_vout: 0,
                    script_sig: vec![0x51; 72],
                    sequence: 0xFFFFFFFF,
                })
                .collect(),
            outputs: vec![TxOutput {
                value: 100_000,
                script_pubkey: vec![0x76, 0xa9, 0x14, 0x00]
                    .into_iter()
                    .chain(std::iter::repeat_n(0x00, 21))
                    .collect(),
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };

        group.bench_with_input(
            BenchmarkId::new("p2pkh", format!("{input_count}in")),
            &tx,
            |b, tx| b.iter(|| black_box(tx.sighash(0, &subscript))),
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_pure_functions,
    bench_build_stake_block,
    bench_add_block,
    bench_merkle_root,
    bench_sighash,
);
criterion_main!(benches);
