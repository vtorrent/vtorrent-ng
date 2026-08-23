#![no_main]
use libfuzzer_sys::fuzz_target;

use vtorrent_p2p::codec::VtrCodec;

fuzz_target!(|data: &[u8]| {
    let mut buf = bytes::BytesMut::from(data);
    let mut codec = VtrCodec;
    let _ = tokio_util::codec::Decoder::decode(&mut codec, &mut buf);
});
