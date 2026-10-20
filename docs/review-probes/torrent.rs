use vtorrent_torrent::{engine::PieceAssembler, peer_wire::PeerMessage};

#[test]
fn audit_empty_piece_frames_cannot_bypass_the_block_cap() {
    let frame = [0, 0, 0, 9, 7, 0, 0, 0, 0, 0, 0, 0, 0];
    let (message, _) = PeerMessage::decode(&frame).unwrap().unwrap();
    let PeerMessage::Piece { begin, data, .. } = message else {
        panic!("wrong decoded type")
    };
    assert!(data.is_empty());
    let mut assembler = PieceAssembler::new(0, 16_384);
    let admitted = (0..5_000)
        .filter(|_| assembler.add_block(begin, data.clone()))
        .count();
    assert_eq!(
        admitted, 0,
        "empty wire blocks must not allocate range entries indefinitely"
    );
}
