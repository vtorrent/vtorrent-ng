//! Deterministic two-peer split.
//!
//! Each peer advertises only half of the torrent. The scheduler must never ask
//! a peer for a piece it lacks, both peers must contribute, and the reassembled
//! bytes must hash to the original torrent. This is the fast regression for the
//! live pass-8 split that predates the peer-availability check.

use std::collections::HashMap;

use sha1::{Digest, Sha1};
use vtorrent_torrent::engine::PieceAssembler;
use vtorrent_torrent::scheduler::SchedulerState;

const PIECE_LEN: usize = 64;
const PIECE_COUNT: u32 = 4;

fn piece_bytes(piece: u32) -> &'static [u8] {
    // Four distinct, non-overlapping 64-byte pieces.
    const PIECES: [[u8; PIECE_LEN]; PIECE_COUNT as usize] = [
        [0x11; PIECE_LEN],
        [0x22; PIECE_LEN],
        [0x33; PIECE_LEN],
        [0x44; PIECE_LEN],
    ];
    &PIECES[piece as usize]
}

#[test]
fn two_partial_peers_split_the_torrent_and_assemble_it() {
    let mut scheduler = SchedulerState::new(PIECE_COUNT);
    let first = [0xA1; 20];
    let second = [0xB2; 20];
    // First peer advertises pieces 0 and 1; second peer only 2 and 3.
    scheduler.tracker.set_peer_bitfield(first, &[0xC0]);
    scheduler.tracker.set_peer_bitfield(second, &[0x30]);

    let piece_len = |_: u32| PIECE_LEN as u64;
    let mut assemblers: HashMap<[u8; 20], HashMap<u32, PieceAssembler>> = HashMap::new();
    let mut served: HashMap<[u8; 20], usize> = HashMap::new();
    let mut pieces: Vec<Option<Vec<u8>>> = vec![None; PIECE_COUNT as usize];

    loop {
        let mut progressed = false;
        for peer in [first, second] {
            while let Some((piece, begin, length)) = scheduler.next_block(&piece_len, peer) {
                let block =
                    piece_bytes(piece)[begin as usize..begin as usize + length as usize].to_vec();
                assert!(
                    scheduler.mark_received(piece, begin, peer),
                    "peer {peer:?} must own the block it was leased"
                );
                let assembled = {
                    let assembler = assemblers
                        .entry(peer)
                        .or_default()
                        .entry(piece)
                        .or_insert_with(|| PieceAssembler::new(piece, PIECE_LEN as u64));
                    assert!(assembler.add_block(begin, block));
                    assembler.assemble()
                };
                *served.entry(peer).or_default() += 1;
                progressed = true;

                if let Some(bytes) = assembled {
                    assert_eq!(
                        bytes,
                        piece_bytes(piece),
                        "assembled piece {piece} must match the source"
                    );
                    assemblers.get_mut(&peer).unwrap().remove(&piece);
                    scheduler.mark_have(piece);
                    pieces[piece as usize] = Some(bytes);
                }
            }
        }
        if !progressed {
            break;
        }
    }

    assert!(
        scheduler.tracker.is_complete(),
        "the split peers must be able to complete the torrent together"
    );
    assert!(served[&first] > 0, "the first peer must serve a piece");
    assert!(served[&second] > 0, "the second peer must serve a piece");

    let assembled: Vec<u8> = pieces
        .into_iter()
        .flat_map(|piece| piece.expect("every piece must be assembled"))
        .collect();
    let expected: Vec<u8> = (0..PIECE_COUNT)
        .flat_map(|piece| piece_bytes(piece).to_vec())
        .collect();
    assert_eq!(assembled, expected);
    assert_eq!(
        Sha1::digest(&assembled).as_slice(),
        Sha1::digest(&expected).as_slice()
    );
}
