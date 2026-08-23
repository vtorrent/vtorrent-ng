#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz PSBT deserialization.
    let _result: Result<bitcoin::psbt::Psbt, _> = bitcoin::psbt::Psbt::deserialize(data);
});
