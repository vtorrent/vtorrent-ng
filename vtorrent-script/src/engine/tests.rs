use super::*;
use crate::standard::{build_p2ms, build_p2pkh};
use ripemd::Ripemd160;
use secp256k1::{Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};

fn make_keypair() -> (SecretKey, secp256k1::PublicKey) {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[1u8; 32]).unwrap();
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

#[test]
fn test_p2pkh_valid() {
    let (sk, pk) = make_keypair();
    let pk_bytes = pk.serialize().to_vec();

    let tx_hash = [0xabu8; 32];
    let sig = sign_tx(&sk, &tx_hash);

    // Build P2PKH scriptPubKey from pubkey hash
    let pubkey_hash: [u8; 20] = {
        let sha = Sha256::digest(&pk_bytes);
        let ripe = ripemd::Ripemd160::digest(sha);
        ripe.into()
    };
    let script_pubkey = build_p2pkh(&pubkey_hash).unwrap();

    // Build scriptSig: <sig> <pubkey>
    let mut script_sig = crate::script::Script::new();
    script_sig.push_data(&sig).unwrap();
    script_sig.push_data(&pk_bytes).unwrap();

    let env = ScriptEnv {
        tx_hash,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    engine
        .execute(&script_sig, &script_pubkey)
        .expect("P2PKH should be valid");
}

#[test]
fn test_p2pkh_wrong_signature_fails() {
    let (_, pk) = make_keypair();
    let pk_bytes = pk.serialize().to_vec();

    let tx_hash = [0xabu8; 32];
    // Sign with wrong key
    let (wrong_sk, _) = {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[2u8; 32]).unwrap();
        let pk2 = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        (sk, pk2)
    };
    let sig = sign_tx(&wrong_sk, &tx_hash);

    let pubkey_hash: [u8; 20] = {
        let sha = Sha256::digest(&pk_bytes);
        ripemd::Ripemd160::digest(sha).into()
    };
    let script_pubkey = build_p2pkh(&pubkey_hash).unwrap();

    let mut script_sig = crate::script::Script::new();
    script_sig.push_data(&sig).unwrap();
    script_sig.push_data(&pk_bytes).unwrap();

    let env = ScriptEnv {
        tx_hash,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    assert!(engine.execute(&script_sig, &script_pubkey).is_err());
}

#[test]
fn test_op_return_unspendable() {
    let script_sig = Script::new();
    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0x6a); // OP_RETURN
    script_pubkey.push_data(b"hello").unwrap();

    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    let result = engine.execute(&script_sig, &script_pubkey);
    assert!(matches!(result, Err(ScriptError::OpReturnUnspendable)));
}

#[test]
fn test_truncated_push_fails_script() {
    // A redeem script of `OP_TRUE` followed by a PUSHDATA1 claiming 255
    // bytes with no data must FAIL, not silently evaluate to true.
    let script_sig = Script::new();
    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0x51); // OP_TRUE
    script_pubkey.push_opcode(0x4c); // OP_PUSHDATA1
    script_pubkey.push_opcode(0xff); // declared length 255, no data follows

    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    let result = engine.execute(&script_sig, &script_pubkey);
    assert!(result.is_err(), "truncated push must fail the script");
}

#[test]
fn test_op_equal_true() {
    let mut script_sig = Script::new();
    script_sig.push_data(b"hello").unwrap();
    script_sig.push_data(b"hello").unwrap();

    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0x87); // OP_EQUAL

    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine
        .execute(&script_sig, &script_pubkey)
        .expect("equal items should succeed");
}

#[test]
fn test_op_equal_false() {
    let mut script_sig = Script::new();
    script_sig.push_data(b"hello").unwrap();
    script_sig.push_data(b"world").unwrap();

    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0x87); // OP_EQUAL

    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    assert!(engine.execute(&script_sig, &script_pubkey).is_err());
}

#[test]
fn test_op_dup_and_hash160() {
    // OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY — without the final CHECKSIG
    let (_, pk) = make_keypair();
    let pk_bytes = pk.serialize().to_vec();
    let pubkey_hash: Vec<u8> = {
        let sha = Sha256::digest(&pk_bytes);
        ripemd::Ripemd160::digest(sha).to_vec()
    };

    let mut script_sig = Script::new();
    script_sig.push_data(&pk_bytes).unwrap();

    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0x76); // OP_DUP
    script_pubkey.push_opcode(0xa9); // OP_HASH160
    script_pubkey.push_data(&pubkey_hash).unwrap();
    script_pubkey.push_opcode(0x88); // OP_EQUALVERIFY
                                     // Push true to leave stack non-empty
    script_pubkey.push_opcode(0x51); // OP_1

    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine
        .execute(&script_sig, &script_pubkey)
        .expect("hash160 should match");
}

#[test]
fn test_if_else_endif() {
    // Push 1, OP_IF push "yes" OP_ELSE push "no" OP_ENDIF
    let mut script = Script::new();
    script.push_opcode(0x51); // OP_1 (true)
    script.push_opcode(0x63); // OP_IF
    script.push_data(b"yes").unwrap();
    script.push_opcode(0x67); // OP_ELSE
    script.push_data(b"no").unwrap();
    script.push_opcode(0x68); // OP_ENDIF

    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.last().unwrap(), b"yes");
}

#[test]
fn test_is_true_empty_is_false() {
    assert!(!is_true(&[]));
    assert!(!is_true(&[0x00]));
    assert!(is_true(&[0x01]));
    assert!(is_true(&[0x80, 0x01]));
}

// ── New opcode tests ───────────────────────────────────────────────────────

#[test]
fn test_depth_op() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x53);
    script.push_opcode(0x74);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.last().unwrap(), &vec![3]);
}

#[test]
fn test_3dup_op() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x53);
    script.push_opcode(0x6f);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 6);
    assert_eq!(engine.stack.last().unwrap(), &vec![3]);
}

#[test]
fn test_nip_op() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x53);
    script.push_opcode(0x77);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 2);
    assert_eq!(engine.stack[0], vec![1]);
    assert_eq!(engine.stack[1], vec![3]);
}

#[test]
fn test_roll_op() {
    let mut script = Script::new();
    script.push_opcode(0x51); // 1
    script.push_opcode(0x52); // 2
    script.push_opcode(0x53); // 3
    script.push_opcode(0x51); // index 1
    script.push_opcode(0x7a); // OP_ROLL
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    // Pop index 1, stack=[1,2,3]. idx=3-1-1=1, remove stack[1]=2 → [1,3,2]
    assert_eq!(engine.stack.last().unwrap(), &vec![2]);
}

#[test]
fn test_pick_op() {
    let mut script = Script::new();
    script.push_opcode(0x51); // 1
    script.push_opcode(0x52); // 2
    script.push_opcode(0x53); // 3
    script.push_opcode(0x51); // index 1
    script.push_opcode(0x78); // OP_PICK (copy, don't remove)
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    // Pop index 1, stack=[1,2,3]. idx=3-1-1=1, clone stack[1]=2 → [1,2,3,2]
    assert_eq!(engine.stack.len(), 4);
    assert_eq!(engine.stack.last().unwrap(), &vec![2]);
    // Original item still in place
    assert_eq!(engine.stack[1], vec![2]);
}

#[test]
fn test_tuck_op() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x7d);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    // [1,2], top=2, insert at len-2=0 → [2,1,2]
    assert_eq!(engine.stack.len(), 3);
    assert_eq!(engine.stack[0], vec![2]);
    assert_eq!(engine.stack[1], vec![1]);
    assert_eq!(engine.stack[2], vec![2]);
}

#[test]
fn test_2swap_op() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x53);
    script.push_opcode(0x54);
    script.push_opcode(0x72);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack[0], vec![3]);
    assert_eq!(engine.stack[1], vec![4]);
    assert_eq!(engine.stack[2], vec![1]);
    assert_eq!(engine.stack[3], vec![2]);
}

#[test]
fn test_2over_op() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x53);
    script.push_opcode(0x54);
    script.push_opcode(0x70);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 6);
    assert_eq!(engine.stack[4], vec![1]);
    assert_eq!(engine.stack[5], vec![2]);
}

#[test]
fn test_2rot_op() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x53);
    script.push_opcode(0x54);
    script.push_opcode(0x55);
    script.push_opcode(0x56);
    script.push_opcode(0x71);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 6);
    assert_eq!(engine.stack[0], vec![3]);
    assert_eq!(engine.stack[1], vec![4]);
    assert_eq!(engine.stack[2], vec![5]);
    assert_eq!(engine.stack[3], vec![6]);
    assert_eq!(engine.stack[4], vec![1]);
    assert_eq!(engine.stack[5], vec![2]);
}

#[test]
fn test_csv_nop_when_disable_flag_set() {
    let mut script = Script::new();
    // Push 0x80000001 as a 5-byte value so it's positive in sign-magnitude
    // (bit 31 set for disable flag, but sign bit in byte[4] is clear)
    script.push_data(&[0x01, 0x00, 0x00, 0x80, 0x00]).unwrap();
    script.push_opcode(0xb2);
    script.push_opcode(0x75);
    script.push_opcode(0x51);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.last().unwrap(), &vec![1]);
}

#[test]
fn test_0notequal() {
    let mut script = Script::new();
    script.push_opcode(0x00);
    script.push_opcode(0x92);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.last().unwrap(), &vec![]);
}

#[test]
fn test_booland_boolor() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x9a);
    script.push_opcode(0x51);
    script.push_opcode(0x00);
    script.push_opcode(0x9b);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 2);
    assert_eq!(engine.stack[0], vec![1]);
    assert_eq!(engine.stack[1], vec![1]);
}

#[test]
fn test_numequal_numnotequal() {
    let mut script = Script::new();
    script.push_opcode(0x55);
    script.push_opcode(0x55);
    script.push_opcode(0x9c);
    script.push_opcode(0x55);
    script.push_opcode(0x54);
    script.push_opcode(0x9e);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 2);
    assert_eq!(engine.stack[0], vec![1]);
    assert_eq!(engine.stack[1], vec![1]);
}

#[test]
fn test_numequalverify_fails_on_mismatch() {
    let mut script = Script::new();
    script.push_opcode(0x55);
    script.push_opcode(0x54);
    script.push_opcode(0x9d);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    assert!(engine.run(&script).is_err());
}

#[test]
fn test_compare_ops() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0x52);
    script.push_opcode(0x9f);
    script.push_opcode(0x52);
    script.push_opcode(0x51);
    script.push_opcode(0xa0);
    script.push_opcode(0x53);
    script.push_opcode(0x53);
    script.push_opcode(0xa1);
    script.push_opcode(0x54);
    script.push_opcode(0x54);
    script.push_opcode(0xa2);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 4);
    for item in &engine.stack {
        assert_eq!(item, &vec![1u8]);
    }
}

#[test]
fn test_min_max_within() {
    let mut script = Script::new();
    script.push_opcode(0x53);
    script.push_opcode(0x55);
    script.push_opcode(0xa3);
    script.push_opcode(0x53);
    script.push_opcode(0x55);
    script.push_opcode(0xa4);
    script.push_opcode(0x54);
    script.push_opcode(0x53);
    script.push_opcode(0x57);
    script.push_opcode(0xa5);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 3);
    assert_eq!(engine.stack[0], vec![3]);
    assert_eq!(engine.stack[1], vec![5]);
    assert_eq!(engine.stack[2], vec![1]);
}

#[test]
fn test_csv_succeeds_when_sequence_sufficient() {
    let mut script = Script::new();
    script.push_opcode(0x52);
    script.push_opcode(0xb2);
    script.push_opcode(0x75);
    script.push_opcode(0x51);
    let env = ScriptEnv {
        input_sequence: 0x0002_0002,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.last().unwrap(), &vec![1]);
}

#[test]
fn test_csv_fails_when_sequence_insufficient() {
    let mut script = Script::new();
    script.push_opcode(0x52);
    script.push_opcode(0xb2);
    let env = ScriptEnv {
        input_sequence: 0x0001_0001,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    assert!(matches!(
        engine.run(&script),
        Err(ScriptError::UnsatisfiedSequence)
    ));
}

#[test]
fn test_csv_fails_when_final_input() {
    let mut script = Script::new();
    script.push_opcode(0x51);
    script.push_opcode(0xb2);
    let env = ScriptEnv {
        input_sequence: 0xffff_ffff,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    assert!(matches!(
        engine.run(&script),
        Err(ScriptError::UnsatisfiedSequence)
    ));
}

#[test]
fn test_csv_fails_on_negative() {
    let mut script = Script::new();
    script.push_data(&[0x81]).unwrap();
    script.push_opcode(0xb2);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    assert!(matches!(
        engine.run(&script),
        Err(ScriptError::NegativeSequence)
    ));
}

#[test]
fn test_csv_fails_on_type_mismatch() {
    let mut script = Script::new();
    script
        .push_data(&0x0040_0001u32.to_le_bytes()[..4])
        .unwrap();
    script.push_opcode(0xb2);
    let env = ScriptEnv {
        input_sequence: 0x0000_0002,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    assert!(matches!(
        engine.run(&script),
        Err(ScriptError::UnsatisfiedSequence)
    ));
}

#[test]
fn test_op_codeseparator_is_nop() {
    let mut script = Script::new();
    script.push_data(&[42]).unwrap();
    script.push_opcode(0xab); // OP_CODESEPARATOR
    script.push_data(&[99]).unwrap();
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.run(&script).unwrap();
    assert_eq!(engine.stack.len(), 2);
    assert_eq!(engine.stack[0], vec![42]);
    assert_eq!(engine.stack[1], vec![99]);
}

// ── Differential / coverage tests for previously-untested opcodes ──────

fn run_op(opcode: u8) -> Result<()> {
    let script_sig = Script::new();
    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(opcode);
    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine.execute(&script_sig, &script_pubkey)
}

#[test]
fn test_op_1negate() {
    // OP_1NEGATE pushes -1 (truthy)
    run_op(0x4f).expect("OP_1NEGATE should succeed");
}

#[test]
fn test_op_verify_succeeds() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    let mut sp = Script::new();
    sp.push_opcode(0x51); // OP_1
    sp.push_opcode(0x69); // OP_VERIFY (consumes 1, passes)
    sp.push_opcode(0x51); // OP_1 (leaves 1 for execute's final check)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_VERIFY on true should pass");
}

#[test]
fn test_op_verify_fails_on_false() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_opcode(0x00); // OP_0
    sp.push_opcode(0x69); // OP_VERIFY (consumes 0, fails)
    sp.push_opcode(0x51); // OP_1 (never reached)
    let mut e = Engine::new(ScriptEnv::default());
    assert!(
        e.execute(&sig, &sp).is_err(),
        "OP_VERIFY on false must fail"
    );
}

#[test]
fn test_op_toalt_fromalt() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    let mut sp = Script::new();
    sp.push_opcode(0x6b); // TOALTSTACK
    sp.push_opcode(0x6c); // FROMALTSTACK
                          // After: stack has [1], execute checks top → true
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("TOALTSTACK/FROMALTSTACK roundtrip should succeed");
}

#[test]
fn test_op_2drop() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    sig.push_opcode(0x52); // OP_2
    let mut sp = Script::new();
    sp.push_opcode(0x6d); // OP_2DROP (drops both)
    sp.push_opcode(0x51); // OP_1 (leaves 1 for execute)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_2DROP should succeed");
}

#[test]
fn test_op_2dup() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    sig.push_opcode(0x52); // OP_2
    let mut sp = Script::new();
    sp.push_opcode(0x6e); // OP_2DUP → [1,2,1,2]
                          // execute checks top → 2 → true
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_2DUP should succeed");
}

#[test]
fn test_op_ifdup_true() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    let mut sp = Script::new();
    sp.push_opcode(0x73); // OP_IFDUP → [1,1]
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("OP_IFDUP on true should succeed");
}

#[test]
fn test_op_ifdup_false() {
    let mut sig = Script::new();
    sig.push_opcode(0x00); // OP_0
    let mut sp = Script::new();
    sp.push_opcode(0x73); // OP_IFDUP → [0] (no dup)
    sp.push_opcode(0x51); // OP_1 (for execute final check)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("OP_IFDUP on false should succeed");
}

#[test]
fn test_op_over() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    sig.push_opcode(0x52); // OP_2
    let mut sp = Script::new();
    sp.push_opcode(0x79); // OP_OVER → [1,2,1]
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_OVER should succeed");
}

#[test]
fn test_op_rot() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // 1
    sig.push_opcode(0x52); // 2
    sig.push_opcode(0x53); // 3
    let mut sp = Script::new();
    sp.push_opcode(0x7b); // OP_ROT → [2,3,1]
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_ROT should succeed");
}

#[test]
fn test_op_swap() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // 1
    sig.push_opcode(0x52); // 2
    let mut sp = Script::new();
    sp.push_opcode(0x7c); // OP_SWAP → [2,1]
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_SWAP should succeed");
}

#[test]
fn test_op_size() {
    let mut sig = Script::new();
    sig.push_data(b"hello").unwrap(); // 5 bytes
    let mut sp = Script::new();
    sp.push_opcode(0x82); // OP_SIZE → pushes 5
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_SIZE should succeed");
}

#[test]
fn test_op_1add() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    let mut sp = Script::new();
    sp.push_opcode(0x8b); // OP_1ADD → 2
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_1ADD should succeed");
}

#[test]
fn test_op_1sub() {
    let mut sig = Script::new();
    sig.push_opcode(0x52); // OP_2
    let mut sp = Script::new();
    sp.push_opcode(0x8c); // OP_1SUB → 1
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_1SUB should succeed");
}

#[test]
fn test_op_negate() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    let mut sp = Script::new();
    sp.push_opcode(0x8f); // OP_NEGATE → -1 (truthy)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_NEGATE should succeed");
}

#[test]
fn test_op_abs() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    let mut sp = Script::new();
    sp.push_opcode(0x8f); // OP_NEGATE → -1
    sp.push_opcode(0x90); // OP_ABS → 1
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_ABS should succeed");
}

#[test]
fn test_op_not_zero() {
    let mut sig = Script::new();
    sig.push_opcode(0x00); // OP_0
    let mut sp = Script::new();
    sp.push_opcode(0x91); // OP_NOT → 1 (true)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_NOT(0) should be true");
}

#[test]
fn test_op_not_nonzero_fails() {
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    let mut sp = Script::new();
    sp.push_opcode(0x91); // OP_NOT → 0 (false)
    let mut e = Engine::new(ScriptEnv::default());
    assert!(
        e.execute(&sig, &sp).is_err(),
        "OP_NOT(1) = 0, execute should fail"
    );
}

#[test]
fn test_op_add() {
    let mut sig = Script::new();
    sig.push_opcode(0x52); // 2
    sig.push_opcode(0x53); // 3
    let mut sp = Script::new();
    sp.push_opcode(0x93); // OP_ADD → 5
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_ADD should succeed");
}

#[test]
fn test_op_notif_true() {
    // OP_1 OP_NOTIF → false branch (push 0), OP_ENDIF → stack [0] → execute fails
    let mut sig = Script::new();
    sig.push_opcode(0x51); // OP_1
    let mut sp = Script::new();
    sp.push_opcode(0x64); // OP_NOTIF (1 is true → NOTIF → false branch)
    sp.push_opcode(0x00); // OP_0 (executed in false branch)
    sp.push_opcode(0x68); // OP_ENDIF
    sp.push_opcode(0x51); // OP_1 (for execute final check)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("OP_NOTIF true → false branch should execute");
}

#[test]
fn test_op_notif_false() {
    // OP_0 OP_NOTIF → true branch (push 1), OP_ENDIF → stack [1] → execute passes
    let mut sig = Script::new();
    sig.push_opcode(0x00); // OP_0
    let mut sp = Script::new();
    sp.push_opcode(0x64); // OP_NOTIF (0 is false → NOTIF → true branch)
    sp.push_opcode(0x51); // OP_1 (executed in true branch)
    sp.push_opcode(0x68); // OP_ENDIF
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("OP_NOTIF false → true branch should execute");
}

#[test]
fn test_op_sub() {
    let mut sig = Script::new();
    sig.push_opcode(0x53); // 3
    sig.push_opcode(0x52); // 2
    let mut sp = Script::new();
    sp.push_opcode(0x94); // OP_SUB → 1
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_SUB should succeed");
}

#[test]
fn test_op_1negate_abs() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_opcode(0x4f); // OP_1NEGATE → -1
    sp.push_opcode(0x90); // OP_ABS → 1
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("OP_1NEGATE + OP_ABS should succeed");
}

#[test]
fn test_op_within_true() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_data(&5i64.to_le_bytes()[..1]).unwrap(); // x = 5
    sp.push_data(&1i64.to_le_bytes()[..1]).unwrap(); // min = 1
    sp.push_data(&10i64.to_le_bytes()[..1]).unwrap(); // max = 10
    sp.push_opcode(0xa5); // OP_WITHIN → 1 (1 <= 5 < 10)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("5 within [1,10) should succeed");
}

#[test]
fn test_op_within_false_below_min() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_data(&0i64.to_le_bytes()[..1]).unwrap(); // x = 0
    sp.push_data(&1i64.to_le_bytes()[..1]).unwrap(); // min = 1
    sp.push_data(&10i64.to_le_bytes()[..1]).unwrap(); // max = 10
    sp.push_opcode(0xa5); // OP_WITHIN → 0 (0 < 1)
    sp.push_opcode(0x51); // OP_1 (for execute final check)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("0 within [1,10) should evaluate without error");
}

#[test]
fn test_op_ripemd160() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_data(b"hello").unwrap();
    sp.push_opcode(0xa6); // OP_RIPEMD160
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_RIPEMD160 should succeed");
    let result = e.stack.last().unwrap();
    assert_eq!(result.len(), 20);
    let expected: [u8; 20] = [
        0x10, 0x8f, 0x07, 0xb8, 0x38, 0x24, 0x12, 0x61, 0x2c, 0x04, 0x8d, 0x07, 0xd1, 0x3f, 0x81,
        0x41, 0x18, 0x44, 0x5a, 0xcd,
    ];
    assert_eq!(result.as_slice(), &expected);
}

#[test]
fn test_op_sha1() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_data(b"hello").unwrap();
    sp.push_opcode(0xa7); // OP_SHA1
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_SHA1 should succeed");
    let result = e.stack.last().unwrap();
    assert_eq!(result.len(), 20);
    let expected: [u8; 20] = [
        0xaa, 0xf4, 0xc6, 0x1d, 0xdc, 0xc5, 0xe8, 0xa2, 0xda, 0xbe, 0xde, 0x0f, 0x3b, 0x48, 0x2c,
        0xd9, 0xae, 0xa9, 0x43, 0x4d,
    ];
    assert_eq!(result.as_slice(), &expected);
}

#[test]
fn test_op_hash160() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_data(b"hello").unwrap();
    sp.push_opcode(0xa9); // OP_HASH160
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_HASH160 should succeed");
    let result = e.stack.last().unwrap();
    assert_eq!(result.len(), 20);
    let expected: [u8; 20] = [
        0xb6, 0xa9, 0xc8, 0xc2, 0x30, 0x72, 0x2b, 0x7c, 0x74, 0x83, 0x31, 0xa8, 0xb4, 0x50, 0xf0,
        0x55, 0x66, 0xdc, 0x7d, 0x0f,
    ];
    assert_eq!(result.as_slice(), &expected);
}

#[test]
fn test_op_hash256() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_data(b"hello").unwrap();
    sp.push_opcode(0xaa); // OP_HASH256
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_HASH256 should succeed");
    let result = e.stack.last().unwrap();
    assert_eq!(result.len(), 32);
    let expected: [u8; 32] = [
        0x95, 0x95, 0xc9, 0xdf, 0x90, 0x07, 0x51, 0x48, 0xeb, 0x06, 0x86, 0x03, 0x65, 0xdf, 0x33,
        0x58, 0x4b, 0x75, 0xbf, 0xf7, 0x82, 0xa5, 0x10, 0xc6, 0xcd, 0x48, 0x83, 0xa4, 0x19, 0x83,
        0x3d, 0x50,
    ];
    assert_eq!(result.as_slice(), &expected);
}

#[test]
fn test_op_equalverify_pass() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_data(b"abc").unwrap();
    sp.push_data(b"abc").unwrap();
    sp.push_opcode(0x88); // OP_EQUALVERIFY
    sp.push_opcode(0x51); // OP_1 (leave truthy for execute)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp)
        .expect("OP_EQUALVERIFY on equal items should pass");
}

#[test]
fn test_op_equalverify_fail() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_data(b"abc").unwrap();
    sp.push_data(b"xyz").unwrap();
    sp.push_opcode(0x88); // OP_EQUALVERIFY
    sp.push_opcode(0x51); // OP_1 (never reached)
    let mut e = Engine::new(ScriptEnv::default());
    assert!(
        e.execute(&sig, &sp).is_err(),
        "OP_EQUALVERIFY on different items must fail"
    );
}

#[test]
fn test_op_mul() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_opcode(0x53); // 3
    sp.push_opcode(0x54); // 4
    sp.push_opcode(0x95); // OP_MUL → 12
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_MUL should succeed");
    assert_eq!(e.stack.last().unwrap(), &vec![12]);
}

#[test]
fn test_op_div() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_opcode(0x5a); // 10
    sp.push_opcode(0x53); // 3
    sp.push_opcode(0x96); // OP_DIV → 3 (integer division)
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_DIV should succeed");
    assert_eq!(e.stack.last().unwrap(), &vec![3]);
}

#[test]
fn test_op_mod() {
    let sig = Script::new();
    let mut sp = Script::new();
    sp.push_opcode(0x5a); // 10
    sp.push_opcode(0x53); // 3
    sp.push_opcode(0x97); // OP_MOD → 1
    let mut e = Engine::new(ScriptEnv::default());
    e.execute(&sig, &sp).expect("OP_MOD should succeed");
    assert_eq!(e.stack.last().unwrap(), &vec![1]);
}

// ── Multisig / P2SH tests ────────────────────────────────────────────────

fn make_keypair_seed(seed: u8) -> (SecretKey, secp256k1::PublicKey) {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
    let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
    (sk, pk)
}

#[test]
fn test_2of3_multisig() {
    let (_, pk1) = make_keypair_seed(1);
    let (_, pk2) = make_keypair_seed(2);
    let (_, pk3) = make_keypair_seed(3);

    let tx_hash = [0xcd_u8; 32];

    let script_pubkey = build_p2ms(
        2,
        &[
            pk1.serialize().to_vec(),
            pk2.serialize().to_vec(),
            pk3.serialize().to_vec(),
        ],
    )
    .unwrap();

    let sig1 = sign_tx(&make_keypair_seed(1).0, &tx_hash);
    let sig2 = sign_tx(&make_keypair_seed(2).0, &tx_hash);

    let mut script_sig = Script::new();
    script_sig.push_opcode(0x00); // OP_0 dummy (Bitcoin CHECKMULTISIG bug)
    script_sig.push_data(&sig1).unwrap();
    script_sig.push_data(&sig2).unwrap();

    let env = ScriptEnv {
        tx_hash,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    engine
        .execute(&script_sig, &script_pubkey)
        .expect("2-of-3 multisig should succeed");
}

#[test]
fn test_1of2_multisig() {
    let (_, pk1) = make_keypair_seed(10);
    let (_, pk2) = make_keypair_seed(20);

    let tx_hash = [0xef_u8; 32];

    let script_pubkey =
        build_p2ms(1, &[pk1.serialize().to_vec(), pk2.serialize().to_vec()]).unwrap();

    let sig = sign_tx(&make_keypair_seed(10).0, &tx_hash);

    let mut script_sig = Script::new();
    script_sig.push_opcode(0x00); // OP_0 dummy
    script_sig.push_data(&sig).unwrap();

    let env = ScriptEnv {
        tx_hash,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    engine
        .execute(&script_sig, &script_pubkey)
        .expect("1-of-2 multisig should succeed");
}

#[test]
fn test_multisig_wrong_keys_fails() {
    let (_, pk1) = make_keypair_seed(1);
    let (_, pk2) = make_keypair_seed(2);
    let (_, pk3) = make_keypair_seed(3);

    let tx_hash = [0xab_u8; 32];

    let script_pubkey = build_p2ms(
        2,
        &[
            pk1.serialize().to_vec(),
            pk2.serialize().to_vec(),
            pk3.serialize().to_vec(),
        ],
    )
    .unwrap();

    // Sign with wrong keys (seeds 4 and 5, not part of the multisig)
    let sig1 = sign_tx(&make_keypair_seed(4).0, &tx_hash);
    let sig2 = sign_tx(&make_keypair_seed(5).0, &tx_hash);

    let mut script_sig = Script::new();
    script_sig.push_opcode(0x00); // OP_0 dummy
    script_sig.push_data(&sig1).unwrap();
    script_sig.push_data(&sig2).unwrap();

    let env = ScriptEnv {
        tx_hash,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    assert!(
        engine.execute(&script_sig, &script_pubkey).is_err(),
        "multisig with wrong keys must fail"
    );
}

#[test]
fn test_multisig_insufficient_sigs_fails() {
    let (_, pk1) = make_keypair_seed(1);
    let (_, pk2) = make_keypair_seed(2);
    let (_, pk3) = make_keypair_seed(3);

    let tx_hash = [0xde_u8; 32];

    let script_pubkey = build_p2ms(
        2,
        &[
            pk1.serialize().to_vec(),
            pk2.serialize().to_vec(),
            pk3.serialize().to_vec(),
        ],
    )
    .unwrap();

    // Only 1 signature for a 2-of-3
    let sig = sign_tx(&make_keypair_seed(1).0, &tx_hash);

    let mut script_sig = Script::new();
    script_sig.push_opcode(0x00); // OP_0 dummy
    script_sig.push_data(&sig).unwrap();

    let env = ScriptEnv {
        tx_hash,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    assert!(
        engine.execute(&script_sig, &script_pubkey).is_err(),
        "multisig with insufficient sigs must fail"
    );
}

#[test]
fn test_p2sh_roundtrip() {
    // Build a simple redeem script: OP_1 (always true)
    let mut redeem_script = Script::new();
    redeem_script.push_opcode(0x51); // OP_1

    let redeem_bytes = redeem_script.as_bytes().to_vec();

    // Hash the redeem script: HASH160(redeem_script)
    let script_hash: [u8; 20] = {
        let sha = Sha256::digest(&redeem_bytes);
        Ripemd160::digest(sha).into()
    };

    // Build P2SH scriptPubKey: OP_HASH160 <hash> OP_EQUAL
    let mut script_pubkey = Script::new();
    script_pubkey.push_opcode(0xa9); // OP_HASH160
    script_pubkey.push_data(&script_hash).unwrap();
    script_pubkey.push_opcode(0x87); // OP_EQUAL

    // scriptSig: push the redeem script
    let mut script_sig = Script::new();
    script_sig.push_data(&redeem_bytes).unwrap();

    let env = ScriptEnv::default();
    let mut engine = Engine::new(env);
    engine
        .execute(&script_sig, &script_pubkey)
        .expect("P2SH roundtrip should succeed");
}
#[test]
fn test_cltv_passes_with_sufficient_sequence() {
    let mut script = Script::new();
    script.push_data(&int_to_bytes(50)).unwrap();
    script.push_opcode(0xb1); // OP_CHECKLOCKTIMEVERIFY
    script.push_opcode(0x75); // OP_DROP
    script.push_opcode(0x51); // OP_1
    let env = ScriptEnv {
        tx_lock_time: 100,
        input_sequence: 0,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    engine
        .run(&script)
        .expect("CLTV should pass with sufficient tx_lock_time");
}

#[test]
fn test_cltv_fails_with_insufficient_sequence() {
    let mut script = Script::new();
    script.push_data(&int_to_bytes(500)).unwrap();
    script.push_opcode(0xb1); // OP_CHECKLOCKTIMEVERIFY
    script.push_opcode(0x75); // OP_DROP
    script.push_opcode(0x51); // OP_1

    let env = ScriptEnv {
        tx_lock_time: 100,
        input_sequence: 0,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    assert!(matches!(
        engine.run(&script),
        Err(ScriptError::HtlcLocktimeNotExpired)
    ));
}

#[test]
fn test_cltv_with_absolute_locktime() {
    let locktime: i64 = 1_700_000_000;
    let tx_lock_time: u32 = 1_700_000_100;
    let mut script = Script::new();
    script.push_data(&int_to_bytes(locktime)).unwrap();
    script.push_opcode(0xb1); // OP_CHECKLOCKTIMEVERIFY
    script.push_opcode(0x75); // OP_DROP
    script.push_opcode(0x51); // OP_1

    let env = ScriptEnv {
        tx_lock_time,
        input_sequence: 0,
        ..Default::default()
    };
    let mut engine = Engine::new(env);
    engine
        .run(&script)
        .expect("CLTV with absolute locktime should pass");
}
