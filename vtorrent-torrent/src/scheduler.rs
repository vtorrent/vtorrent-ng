//! Piece scheduler: rarest-first selection, block pipelining, resume.

use std::collections::HashMap;

/// Tracks piece availability across peers and our own download progress.
#[derive(Debug, Default)]
pub struct PieceTracker {
    /// Number of pieces in the torrent.
    piece_count: u32,
    /// Pieces we already have (index -> true).
    have: Vec<bool>,
    /// Pieces currently requested/in-flight (index -> true).
    requested: Vec<bool>,
    /// Per-peer bitfields: peer_id -> set of piece indices the peer has.
    peer_pieces: HashMap<[u8; 20], Vec<bool>>,
}

impl PieceTracker {
    pub fn new(piece_count: u32) -> Self {
        Self {
            piece_count,
            have: vec![false; piece_count as usize],
            requested: vec![false; piece_count as usize],
            peer_pieces: HashMap::new(),
        }
    }

    /// Mark a piece as downloaded.
    pub fn mark_have(&mut self, index: u32) {
        if (index as usize) < self.have.len() {
            self.have[index as usize] = true;
            self.requested[index as usize] = false;
        }
    }

    /// Mark a piece as requested (in flight).
    pub fn mark_requested(&mut self, index: u32) {
        if (index as usize) < self.requested.len() {
            self.requested[index as usize] = true;
        }
    }

    /// Clear the requested flag (e.g. on cancel or completion).
    pub fn clear_requested(&mut self, index: u32) {
        if (index as usize) < self.requested.len() {
            self.requested[index as usize] = false;
        }
    }

    /// Record a peer's bitfield.
    pub fn set_peer_bitfield(&mut self, peer_id: [u8; 20], bits: &[u8]) {
        let mut have = vec![false; self.piece_count as usize];
        for (i, byte) in bits.iter().enumerate() {
            for bit in 0..8 {
                let idx = i * 8 + bit;
                if idx < have.len() && (byte & (0x80 >> bit)) != 0 {
                    have[idx] = true;
                }
            }
        }
        self.peer_pieces.insert(peer_id, have);
    }

    /// Record a peer's `Have` message.
    pub fn set_peer_have(&mut self, peer_id: [u8; 20], index: u32) {
        let entry = self
            .peer_pieces
            .entry(peer_id)
            .or_insert_with(|| vec![false; self.piece_count as usize]);
        if (index as usize) < entry.len() {
            entry[index as usize] = true;
        }
    }

    /// Count how many peers have a given piece.
    pub fn peer_count_for(&self, index: u32) -> usize {
        self.peer_pieces
            .values()
            .filter(|bits| bits.get(index as usize).copied().unwrap_or(false))
            .count()
    }

    /// Select the next piece to download: the rarest piece we don't have and
    /// haven't already requested, preferring pieces at least one peer has.
    pub fn next_piece(&self) -> Option<u32> {
        let mut best: Option<(usize, u32)> = None;
        for index in 0..self.piece_count {
            if self.have[index as usize] || self.requested[index as usize] {
                continue;
            }
            let count = self.peer_count_for(index);
            if count == 0 {
                continue;
            }
            match best {
                None => best = Some((count, index)),
                Some((best_count, _)) if count < best_count => best = Some((count, index)),
                _ => {}
            }
        }
        best.map(|(_, index)| index)
    }

    /// Whether all pieces are downloaded.
    pub fn is_complete(&self) -> bool {
        self.have.iter().all(|&h| h)
    }

    /// Number of pieces remaining.
    pub fn remaining(&self) -> usize {
        self.have.iter().filter(|&&h| !h).count()
    }

    /// Number of pieces downloaded.
    pub fn have_count(&self) -> usize {
        self.have.iter().filter(|&&h| h).count()
    }

    /// Serialize the `have` bitfield to bytes (one bit per piece, MSB-first).
    pub fn serialize_have_bitfield(&self) -> Vec<u8> {
        let mut bytes = vec![0u8; self.have.len().div_ceil(8)];
        for (i, &h) in self.have.iter().enumerate() {
            if h {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        bytes
    }

    /// Load a `have` bitfield from bytes.
    pub fn load_have_bitfield(&mut self, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            for bit in 0..8 {
                let idx = i * 8 + bit;
                if idx < self.have.len() && (byte & (0x80 >> bit)) != 0 {
                    self.have[idx] = true;
                }
            }
        }
    }
}

/// Shared scheduler state, coordinated across per-peer tasks.
#[derive(Debug)]
pub struct SchedulerState {
    pub tracker: PieceTracker,
    /// Block size for requests (16 KiB).
    pub block_size: u32,
    /// Maximum blocks in flight per peer (pipelining).
    pub max_pipelined_blocks: usize,
    /// Endgame threshold: when this many pieces remain, request from all peers.
    pub endgame_threshold: usize,
    /// Blocks currently in flight: piece index -> set of begin offsets.
    requested_blocks: HashMap<u32, Vec<u32>>,
}

impl SchedulerState {
    pub fn new(piece_count: u32) -> Self {
        Self {
            tracker: PieceTracker::new(piece_count),
            block_size: 16 * 1024,
            max_pipelined_blocks: 5,
            endgame_threshold: 3,
            requested_blocks: HashMap::new(),
        }
    }

    /// Whether endgame mode is active (few pieces remain).
    pub fn in_endgame(&self) -> bool {
        self.tracker.remaining() <= self.endgame_threshold
    }

    /// Select the next block to request: (piece, begin, length).
    ///
    /// Picks the rarest piece we don't have, then the next unrequested block
    /// within that piece. `piece_len` maps a piece index to its byte length.
    /// Returns `None` when no block is available.
    pub fn next_block(&mut self, piece_len: &dyn Fn(u32) -> u64) -> Option<(u32, u32, u32)> {
        let piece = self.tracker.next_piece()?;
        let len = piece_len(piece);
        let block_size = self.block_size as u64;
        let total_blocks = len.div_ceil(block_size);
        let requested = self.requested_blocks.entry(piece).or_default();
        for block in 0..total_blocks {
            let begin = (block * block_size) as u32;
            if requested.contains(&begin) {
                continue;
            }
            let block_len = (len - block * block_size).min(block_size) as u32;
            requested.push(begin);
            return Some((piece, begin, block_len));
        }
        // All blocks of this piece are requested; mark the piece requested so
        // next_piece skips it, and try the next piece.
        self.tracker.mark_requested(piece);
        None
    }

    /// Mark a block as no longer in flight (e.g. on cancel or completion).
    pub fn clear_block(&mut self, piece: u32, begin: u32) {
        if let Some(blocks) = self.requested_blocks.get_mut(&piece) {
            blocks.retain(|&b| b != begin);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitfield_merge() {
        let mut tracker = PieceTracker::new(16);
        // Peer A has pieces 0 and 1 (bits 0x80, 0x40 in first byte).
        tracker.set_peer_bitfield([1u8; 20], &[0xC0, 0x00]);
        assert_eq!(tracker.peer_count_for(0), 1);
        assert_eq!(tracker.peer_count_for(1), 1);
        assert_eq!(tracker.peer_count_for(2), 0);
    }

    #[test]
    fn test_rarest_first() {
        let mut tracker = PieceTracker::new(4);
        // Peer A has pieces 0,1,2,3 (all).
        tracker.set_peer_bitfield([1u8; 20], &[0xF0]);
        // Peer B has pieces 0,1,2 but not 3.
        tracker.set_peer_bitfield([2u8; 20], &[0xE0]);
        // Piece 3 is rarest (1 peer) vs pieces 0,1,2 (2 peers).
        assert_eq!(tracker.next_piece(), Some(3));
    }

    #[test]
    fn test_skips_have_and_requested() {
        let mut tracker = PieceTracker::new(4);
        tracker.set_peer_bitfield([1u8; 20], &[0xF0]);
        tracker.mark_have(0);
        tracker.mark_requested(1);
        // Pieces 0 (have) and 1 (requested) are skipped; 2 and 3 are tied.
        let next = tracker.next_piece().unwrap();
        assert!(next == 2 || next == 3);
    }

    #[test]
    fn test_complete() {
        let mut tracker = PieceTracker::new(2);
        assert!(!tracker.is_complete());
        tracker.mark_have(0);
        tracker.mark_have(1);
        assert!(tracker.is_complete());
        assert_eq!(tracker.remaining(), 0);
    }

    #[test]
    fn test_resume_bitfield_roundtrip() {
        let mut tracker = PieceTracker::new(16);
        tracker.mark_have(0);
        tracker.mark_have(5);
        tracker.mark_have(15);

        let bytes = tracker.serialize_have_bitfield();
        let mut restored = PieceTracker::new(16);
        restored.load_have_bitfield(&bytes);

        assert!(restored.have[0]);
        assert!(restored.have[5]);
        assert!(restored.have[15]);
        assert!(!restored.have[1]);
        assert_eq!(restored.remaining(), 13);
    }

    #[test]
    fn test_scheduler_state_endgame() {
        let mut state = SchedulerState::new(4);
        state.tracker.set_peer_bitfield([1u8; 20], &[0xF0]);
        state.tracker.mark_have(0);
        state.tracker.mark_have(1);
        state.tracker.mark_have(2);
        // Only piece 3 remains; endgame should be active.
        assert!(state.in_endgame());
        assert_eq!(state.tracker.remaining(), 1);
    }

    #[test]
    fn test_next_block_iterates_within_piece() {
        // A piece of 40 KiB with 16 KiB blocks has 3 blocks (0, 16384, 32768).
        let mut state = SchedulerState::new(1);
        state.tracker.set_peer_bitfield([1u8; 20], &[0x80]);
        let piece_len = |_index: u32| 40 * 1024u64;
        let (piece, begin, len) = state.next_block(&piece_len).unwrap();
        assert_eq!(piece, 0);
        assert_eq!(begin, 0);
        assert_eq!(len, 16 * 1024);

        let (_, begin2, _) = state.next_block(&piece_len).unwrap();
        assert_eq!(begin2, 16 * 1024);

        let (_, begin3, _) = state.next_block(&piece_len).unwrap();
        assert_eq!(begin3, 32 * 1024);

        // All blocks requested; no more.
        assert!(state.next_block(&piece_len).is_none());
    }
}
