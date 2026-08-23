#![no_main]
use libfuzzer_sys::fuzz_target;

use vtorrent_script::{Engine, Script, ScriptEnv};

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let mid = data.len() / 2;
    let script_sig = Script(data[..mid].to_vec());
    let script_pubkey = Script(data[mid..].to_vec());

    let env = ScriptEnv {
        tx_hash: [0u8; 32],
        block_height: 100,
        block_time: 1_700_000_000,
        tx_lock_time: 0,
        input_sequence: 0xFFFFFFFF,
    };
    let mut engine = Engine::new(env);
    let _ = engine.execute(&script_sig, &script_pubkey);
});
