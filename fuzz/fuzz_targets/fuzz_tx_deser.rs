#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz BTC transaction deserialization via bitcoin-consensus.
    let _result: Result<bitcoin::Transaction, _> = bitcoin::consensus::encode::deserialize(data);
});
