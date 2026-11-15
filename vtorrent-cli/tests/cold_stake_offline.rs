#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use vtorrent_core::attestation::{FundingAttestation, ATTESTATION_VERSION};
use vtorrent_node::block::{Transaction, TxOutput, TxType};

fn write_attestation(dir: &Path, funding: &Transaction, value_satoshis: u64) -> (PathBuf, String) {
    let key = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let secp = secp256k1::Secp256k1::new();
    let pinned = hex::encode(secp256k1::PublicKey::from_secret_key(&secp, &key).serialize());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut attestation = FundingAttestation {
        version: ATTESTATION_VERSION,
        network: "vtorrent-regtest".into(),
        txid: hex::encode(funding.txid()),
        vout: 0,
        value_satoshis,
        script_pubkey: hex::encode(&funding.outputs[0].script_pubkey),
        block_hash: "33".repeat(32),
        block_height: 100,
        confirmations: 6,
        tip_hash: "44".repeat(32),
        tip_height: 105,
        mempool_spend: false,
        issued_at: now,
        expires_at: now + 3_600,
        attestor_pubkey: String::new(),
        signature: String::new(),
    };
    attestation.sign(&key).unwrap();
    let path = dir.join("attestation.json");
    std::fs::write(&path, serde_json::to_vec(&attestation).unwrap()).unwrap();
    (path, pinned)
}

#[test]
fn interactive_offline_spend_uses_the_actual_funding_output() {
    let dir = std::env::temp_dir().join(format!(
        "vtr-cli-cold-spend-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let keys = vtorrent_wallet::wallet::generate_cold_stake_keys(0, None).unwrap();
    let key_path = dir.join("key");
    vtorrent_wallet::cold_stake_export::export_spending_key(
        &key_path,
        &keys.spending_wif,
        &keys.spending_address,
        "test-passphrase",
    )
    .unwrap();
    let funding = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![],
        outputs: vec![TxOutput {
            value: 100_000,
            script_pubkey: keys.script_pubkey.clone(),
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    let (attestation_path, pinned) = write_attestation(&dir, &funding, funding.outputs[0].value);
    let command = format!(
        "{} cold-stake spend --key-file {} --script {} --funding-txid {} --vout 0 --funding-tx {} --attestation {} --attestor-pubkey {} --network vtorrent-regtest --to {} --amount-sat 99000 --fee-sat 1000 --max-fee-sat 1000",
        env!("CARGO_BIN_EXE_vtorrent-cli"), key_path.display(), hex::encode(&keys.script_pubkey),
        hex::encode(funding.txid()), hex::encode(bincode::serialize(&funding).unwrap()),
        attestation_path.display(), pinned, keys.spending_address,
    );
    let mut child = Command::new("script")
        .args(["-q", "-e", "-c", &command, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"YES\ntest-passphrase\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success(),
        "{output} {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.contains("Funding output:"));
    assert!(output.contains("fee: 1000 sat"));
    let raw = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("raw_tx: "))
        .unwrap();
    let spend: Transaction = bincode::deserialize(&hex::decode(raw).unwrap()).unwrap();
    assert_eq!(spend.inputs[0].prev_txid, funding.txid());
    assert_eq!(spend.outputs.iter().map(|o| o.value).sum::<u64>(), 99_000);
    let env = vtorrent_script::ScriptEnv {
        tx_hash: spend.sighash(0, &keys.script_pubkey),
        tx_lock_time: spend.lock_time,
        input_sequence: spend.inputs[0].sequence,
        ..Default::default()
    };
    vtorrent_script::Engine::new(env)
        .execute(
            &vtorrent_script::Script::from_bytes(spend.inputs[0].script_sig.clone()).unwrap(),
            &vtorrent_script::Script::from_bytes(keys.script_pubkey.clone()).unwrap(),
        )
        .unwrap();

    let rejected = Command::new(env!("CARGO_BIN_EXE_vtorrent-cli"))
        .args([
            "cold-stake",
            "spend",
            "--key-file",
            key_path.to_str().unwrap(),
            "--script",
            &hex::encode(&keys.script_pubkey),
            "--funding-txid",
            &"00".repeat(32),
            "--vout",
            "0",
            "--funding-tx",
            &hex::encode(bincode::serialize(&funding).unwrap()),
            "--attestation",
            attestation_path.to_str().unwrap(),
            "--attestor-pubkey",
            &pinned,
            "--network",
            "vtorrent-regtest",
            "--to",
            &keys.spending_address,
            "--amount-sat",
            "99000",
            "--fee-sat",
            "1000",
            "--max-fee-sat",
            "1000",
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("funding transaction ID does not match")
    );
    assert!(!String::from_utf8_lossy(&rejected.stdout).contains("raw_tx:"));
    let excessive_fee = Command::new(env!("CARGO_BIN_EXE_vtorrent-cli"))
        .args([
            "cold-stake",
            "spend",
            "--key-file",
            key_path.to_str().unwrap(),
            "--script",
            &hex::encode(&keys.script_pubkey),
            "--funding-txid",
            &hex::encode(funding.txid()),
            "--vout",
            "0",
            "--funding-tx",
            &hex::encode(bincode::serialize(&funding).unwrap()),
            "--attestation",
            attestation_path.to_str().unwrap(),
            "--attestor-pubkey",
            &pinned,
            "--network",
            "vtorrent-regtest",
            "--to",
            &keys.spending_address,
            "--amount-sat",
            "80000",
            "--fee-sat",
            "20000",
            "--max-fee-sat",
            "20000",
        ])
        .output()
        .unwrap();
    assert!(!excessive_fee.status.success());
    assert!(String::from_utf8_lossy(&excessive_fee.stderr).contains("fee exceeds 10%"));
    assert!(!String::from_utf8_lossy(&excessive_fee.stdout).contains("raw_tx:"));

    let (wrong_value, wrong_pinned) =
        write_attestation(&dir, &funding, funding.outputs[0].value - 1);
    let mismatched = Command::new(env!("CARGO_BIN_EXE_vtorrent-cli"))
        .args([
            "cold-stake",
            "spend",
            "--key-file",
            key_path.to_str().unwrap(),
            "--script",
            &hex::encode(&keys.script_pubkey),
            "--funding-txid",
            &hex::encode(funding.txid()),
            "--vout",
            "0",
            "--funding-tx",
            &hex::encode(bincode::serialize(&funding).unwrap()),
            "--attestation",
            wrong_value.to_str().unwrap(),
            "--attestor-pubkey",
            &wrong_pinned,
            "--network",
            "vtorrent-regtest",
            "--to",
            &keys.spending_address,
            "--amount-sat",
            "99000",
            "--fee-sat",
            "1000",
            "--max-fee-sat",
            "1000",
        ])
        .output()
        .unwrap();
    assert!(!mismatched.status.success());
    assert!(String::from_utf8_lossy(&mismatched.stderr).contains("value_satoshis"));
    assert!(!String::from_utf8_lossy(&mismatched.stdout).contains("raw_tx:"));

    std::fs::remove_dir_all(dir).unwrap();
}
