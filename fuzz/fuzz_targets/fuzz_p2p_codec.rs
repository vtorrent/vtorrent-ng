#![no_main]
use libfuzzer_sys::fuzz_target;

use bytes::BytesMut;
use tokio_util::codec::Decoder;
use vtorrent_p2p::codec::VtrCodec;

fuzz_target!(|data: &[u8]| {
    // Fuzz the P2P message codec with arbitrary bytes.
    let mut buf = BytesMut::from(data);
    let mut codec = VtrCodec;
    // Must not panic on arbitrary input.
    let _ = codec.decode(&mut buf);
});
