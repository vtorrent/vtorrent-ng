use criterion::{black_box, criterion_group, criterion_main, Criterion};
use secp256k1::{Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use vtorrent_script::{build_p2ms, build_p2pkh, Engine, Script, ScriptEnv};

fn make_keypair(seed: u8) -> (SecretKey, secp256k1::PublicKey) {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
    let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
    (sk, pk)
}

fn sign_tx(sk: &SecretKey, tx_hash: &[u8; 32]) -> Vec<u8> {
    let secp = Secp256k1::new();
    let msg = Message::from_digest(*tx_hash);
    let sig = secp.sign_ecdsa(&msg, sk);
    let mut sig_bytes = sig.serialize_der().to_vec();
    sig_bytes.push(0x01); // SIGHASH_ALL
    sig_bytes
}

fn pubkey_hash(pk: &secp256k1::PublicKey) -> [u8; 20] {
    let sha = Sha256::digest(pk.serialize());
    ripemd::Ripemd160::digest(sha).into()
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

/// P2PKH with 1 input — the common single-signature case.
fn bench_p2pkh_1_input(c: &mut Criterion) {
    let tx_hash = [0xabu8; 32];
    let (sk, pk) = make_keypair(1);
    let pk_bytes = pk.serialize().to_vec();
    let sig = sign_tx(&sk, &tx_hash);
    let hash = pubkey_hash(&pk);
    let script_pubkey = build_p2pkh(&hash).unwrap();

    let mut script_sig = Script::new();
    script_sig.push_data(&sig).unwrap();
    script_sig.push_data(&pk_bytes).unwrap();

    c.bench_function("p2pkh_1_input", |b| {
        b.iter(|| {
            let env = ScriptEnv {
                tx_hash,
                ..Default::default()
            };
            let mut engine = Engine::new(env);
            black_box(engine.execute(&script_sig, &script_pubkey)).unwrap();
        })
    });
}

/// P2PKH with 5 inputs — multi-input transaction.
fn bench_p2pkh_5_inputs(c: &mut Criterion) {
    let tx_hash = [0xcd_u8; 32];

    // Build 5 independent keypairs and their signatures
    let mut keys: Vec<(SecretKey, secp256k1::PublicKey)> = Vec::new();
    let mut sigs: Vec<Vec<u8>> = Vec::new();
    for i in 1..=5u8 {
        let (sk, pk) = make_keypair(i);
        let sig = sign_tx(&sk, &tx_hash);
        keys.push((sk, pk));
        sigs.push(sig);
    }

    // Build 5 separate scriptPubKeys (one per input)
    let script_pubkeys: Vec<Script> = keys
        .iter()
        .map(|(_, pk)| build_p2pkh(&pubkey_hash(pk)).unwrap())
        .collect();

    // Build 5 separate scriptSigs
    let script_sigs: Vec<Script> = keys
        .iter()
        .zip(sigs.iter())
        .map(|((_, pk), sig)| {
            let mut s = Script::new();
            s.push_data(sig).unwrap();
            s.push_data(&pk.serialize()).unwrap();
            s
        })
        .collect();

    c.bench_function("p2pkh_5_inputs", |b| {
        b.iter(|| {
            let env = ScriptEnv {
                tx_hash,
                ..Default::default()
            };
            for i in 0..5 {
                let mut engine = Engine::new(env.clone());
                black_box(engine.execute(&script_sigs[i], &script_pubkeys[i])).unwrap();
            }
        })
    });
}

/// OP_EQUAL — push two equal values and compare.
fn bench_op_equal(c: &mut Criterion) {
    let data = vec![0x42u8; 32];

    let mut script_sig = Script::new();
    script_sig.push_data(&data).unwrap();
    script_sig.push_data(&data).unwrap();

    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0x87); // OP_EQUAL

    c.bench_function("op_equal", |b| {
        b.iter(|| {
            let env = ScriptEnv::default();
            let mut engine = Engine::new(env);
            black_box(engine.execute(&script_sig, &script_pubkey)).unwrap();
        })
    });
}

/// OP_SHA256 — push 32 bytes, hash them.
fn bench_op_sha256(c: &mut Criterion) {
    let data = vec![0xabu8; 32];

    let mut script_sig = Script::new();
    script_sig.push_data(&data).unwrap();

    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0xa8); // OP_SHA256

    c.bench_function("op_sha256", |b| {
        b.iter(|| {
            let env = ScriptEnv::default();
            let mut engine = Engine::new(env);
            black_box(engine.execute(&script_sig, &script_pubkey)).unwrap();
        })
    });
}

/// OP_HASH160 — push 32 bytes, hash with RIPEMD160(SHA256(data)).
fn bench_op_hash160(c: &mut Criterion) {
    let data = vec![0xabu8; 32];

    let mut script_sig = Script::new();
    script_sig.push_data(&data).unwrap();

    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0xa9); // OP_HASH160

    c.bench_function("op_hash160", |b| {
        b.iter(|| {
            let env = ScriptEnv::default();
            let mut engine = Engine::new(env);
            black_box(engine.execute(&script_sig, &script_pubkey)).unwrap();
        })
    });
}

/// 2-of-3 multisig verification.
fn bench_2of3_multisig(c: &mut Criterion) {
    let tx_hash = [0xde_u8; 32];
    let (_, pk1) = make_keypair(1);
    let (_, pk2) = make_keypair(2);
    let (_, pk3) = make_keypair(3);

    let script_pubkey = build_p2ms(
        2,
        &[
            pk1.serialize().to_vec(),
            pk2.serialize().to_vec(),
            pk3.serialize().to_vec(),
        ],
    )
    .unwrap();

    let sig1 = sign_tx(&make_keypair(1).0, &tx_hash);
    let sig2 = sign_tx(&make_keypair(2).0, &tx_hash);

    let mut script_sig = Script::new();
    script_sig.push_opcode(0x00); // OP_0 dummy (Bitcoin CHECKMULTISIG bug)
    script_sig.push_data(&sig1).unwrap();
    script_sig.push_data(&sig2).unwrap();

    c.bench_function("2of3_multisig", |b| {
        b.iter(|| {
            let env = ScriptEnv {
                tx_hash,
                ..Default::default()
            };
            let mut engine = Engine::new(env);
            black_box(engine.execute(&script_sig, &script_pubkey)).unwrap();
        })
    });
}

criterion_group!(
    benches,
    bench_p2pkh_1_input,
    bench_p2pkh_5_inputs,
    bench_op_equal,
    bench_op_sha256,
    bench_op_hash160,
    bench_2of3_multisig,
);
criterion_main!(benches);
