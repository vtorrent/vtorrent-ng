//! Piece scheduler: rarest-first selection, block pipelining, resume.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a block request stays "in flight" without a response before the
/// lease expires and the block becomes requestable again. Covers peers that
/// choke, disconnect, or stall with requests outstanding.
pub const BLOCK_LEASE_TIMEOUT: Duration = Duration::from_secs(90);

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

    /// Clear the have flag for a piece (e.g. resume bitfield turned out to be
    /// invalid for that piece), making it eligible for download again.
    pub fn mark_not_have(&mut self, index: u32) {
        if (index as usize) < self.have.len() {
            self.have[index as usize] = false;
        }
    }

    /// Whether we hold the given piece (used to serve uploads).
    pub fn has_piece(&self, index: u32) -> bool {
        (index as usize) < self.have.len() && self.have[index as usize]
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

    /// Total bytes of all downloaded pieces, summing each piece's actual length
    /// (the last piece is typically shorter than `piece_length`).
    pub fn have_bytes(&self, piece_len: &dyn Fn(u32) -> u64) -> u64 {
        let mut total = 0u64;
        for (i, &h) in self.have.iter().enumerate() {
            if h {
                total = total.saturating_add(piece_len(i as u32));
            }
        }
        total
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

/// A block currently requested from one peer task.
#[derive(Debug, Clone, Copy)]
struct BlockLease {
    begin: u32,
    length: u32,
    /// The scheduler task id, never the remote's self-reported peer id.
    owner: [u8; 20],
    leased_at: Instant,
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
    /// Blocks currently in flight, keyed by piece index. Each lease expires
    /// after `BLOCK_LEASE_TIMEOUT` so a block lost to a choked, disconnected,
    /// or stalled peer is automatically re-requestable.
    requested_blocks: HashMap<u32, Vec<BlockLease>>,
    pub(crate) writing_pieces: std::collections::HashSet<u32>,
    /// Bytes currently held by per-peer piece assemblers across all peer
    /// tasks. Piece data is only verified once the whole piece arrives, so an
    /// attacker could otherwise make every peer task hold a maximum-size
    /// partial piece at the same time.
    assembling_bytes: usize,
}

/// Aggregate in-flight assembly budget shared by every peer task.
pub const MAX_ASSEMBLING_BYTES: usize = 64 * 1024 * 1024;

impl SchedulerState {
    pub fn new(piece_count: u32) -> Self {
        Self {
            tracker: PieceTracker::new(piece_count),
            block_size: 16 * 1024,
            max_pipelined_blocks: 5,
            endgame_threshold: 3,
            requested_blocks: HashMap::new(),
            writing_pieces: std::collections::HashSet::new(),
            assembling_bytes: 0,
        }
    }

    /// Reserve assembly budget for a new partial piece.
    pub fn reserve_assembler(&mut self, piece_len: u64) -> bool {
        let piece_len = piece_len as usize;
        if self.assembling_bytes.saturating_add(piece_len) > MAX_ASSEMBLING_BYTES {
            return false;
        }
        self.assembling_bytes += piece_len;
        true
    }

    /// Release the assembly budget for a partial piece that was dropped.
    pub fn release_assembler(&mut self, piece_len: u64) {
        self.assembling_bytes = self.assembling_bytes.saturating_sub(piece_len as usize);
    }

    /// Whether endgame mode is active (few pieces remain).
    pub fn in_endgame(&self) -> bool {
        self.tracker.remaining() <= self.endgame_threshold
    }

    /// Drop expired leases and unmark their pieces so they can be re-picked.
    fn purge_expired_leases(&mut self) {
        let now = Instant::now();
        let mut expired_pieces: Vec<u32> = Vec::new();
        for (piece, leases) in self.requested_blocks.iter_mut() {
            let before = leases.len();
            leases.retain(|lease| now.duration_since(lease.leased_at) < BLOCK_LEASE_TIMEOUT);
            if leases.len() != before {
                expired_pieces.push(*piece);
            }
        }
        self.requested_blocks.retain(|_, leases| !leases.is_empty());
        for piece in expired_pieces {
            self.tracker.clear_requested(piece);
        }
    }

    /// Select the next block to request: (piece, begin, length).
    ///
    /// Picks the rarest piece we don't have, then the next unrequested block
    /// within that piece. `piece_len` maps a piece index to its byte length.
    /// Returns `None` when no block is available.
    pub fn next_block(
        &mut self,
        piece_len: &dyn Fn(u32) -> u64,
        peer: [u8; 20],
    ) -> Option<(u32, u32, u32)> {
        self.purge_expired_leases();
        let piece = self.tracker.next_piece()?;
        let len = piece_len(piece);
        let block_size = self.block_size as u64;
        let total_blocks = len.div_ceil(block_size);
        let requested = self.requested_blocks.entry(piece).or_default();
        for block in 0..total_blocks {
            let begin = (block * block_size) as u32;
            if requested.iter().any(|lease| lease.begin == begin) {
                continue;
            }
            let block_len = (len - block * block_size).min(block_size) as u32;
            requested.push(BlockLease {
                begin,
                length: block_len,
                owner: peer,
                leased_at: Instant::now(),
            });
            return Some((piece, begin, block_len));
        }
        // All blocks of this piece are in flight; mark the piece requested so
        // next_piece skips it until a lease expires or the piece completes.
        self.tracker.mark_requested(piece);
        None
    }

    /// Mark a block as no longer in flight (e.g. on cancel).
    pub fn clear_block(&mut self, piece: u32, begin: u32, peer: [u8; 20]) {
        if let Some(leases) = self.requested_blocks.get_mut(&piece) {
            leases.retain(|lease| lease.begin != begin || lease.owner != peer);
            if leases.is_empty() {
                self.requested_blocks.remove(&piece);
                self.tracker.clear_requested(piece);
            }
        }
    }

    /// Length of a block currently leased to `peer`, if any.
    ///
    /// The owner check is the security boundary: a peer may only satisfy a
    /// block it was actually asked for, with the exact length requested.
    pub fn requested_block_length(&self, piece: u32, begin: u32, peer: [u8; 20]) -> Option<u32> {
        self.requested_blocks.get(&piece).and_then(|leases| {
            leases
                .iter()
                .find(|lease| lease.begin == begin && lease.owner == peer)
                .map(|lease| lease.length)
        })
    }

    /// Mark a piece as fully downloaded, clearing any in-flight block tracking
    /// for it so `requested_blocks` does not leak stale offsets.
    pub fn mark_have(&mut self, index: u32) {
        self.tracker.mark_have(index);
        self.requested_blocks.remove(&index);
    }

    /// Whether the piece is already on disk.
    pub fn has_piece(&self, index: u32) -> bool {
        self.tracker.has_piece(index)
    }

    /// Undo a provisional `mark_have` when the disk write fails.
    pub fn mark_not_have(&mut self, index: u32) {
        self.tracker.mark_not_have(index);
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
        let (piece, begin, len) = state.next_block(&piece_len, [1u8; 20]).unwrap();
        assert_eq!(piece, 0);
        assert_eq!(begin, 0);
        assert_eq!(len, 16 * 1024);

        let (_, begin2, _) = state.next_block(&piece_len, [1u8; 20]).unwrap();
        assert_eq!(begin2, 16 * 1024);

        let (_, begin3, _) = state.next_block(&piece_len, [1u8; 20]).unwrap();
        assert_eq!(begin3, 32 * 1024);

        // All blocks requested; no more.
        assert!(state.next_block(&piece_len, [1u8; 20]).is_none());
    }

    #[test]
    fn unsolicited_blocks_are_not_considered_requested() {
        let mut state = SchedulerState::new(1);
        state.tracker.set_peer_bitfield([1u8; 20], &[0x80]);
        let piece_len = |_index: u32| 40 * 1024u64;
        let peer = [1u8; 20];
        let (piece, begin, _) = state.next_block(&piece_len, peer).unwrap();
        assert!(state.requested_block_length(piece, begin, peer).is_some());
        assert!(state
            .requested_block_length(piece, begin + 1, peer)
            .is_none());
        assert!(state
            .requested_block_length(piece + 1, begin, peer)
            .is_none());
        // Another peer's task must not be able to satisfy this lease.
        assert!(state
            .requested_block_length(piece, begin, [2u8; 20])
            .is_none());
    }

    #[test]
    fn cancel_only_clears_the_owners_lease() {
        let mut state = SchedulerState::new(1);
        state.tracker.set_peer_bitfield([1u8; 20], &[0x80]);
        let piece_len = |_index: u32| 40 * 1024u64;
        let peer_a = [1u8; 20];
        let peer_b = [2u8; 20];
        let (piece, begin_a, _) = state.next_block(&piece_len, peer_a).unwrap();
        let (_, begin_b, _) = state.next_block(&piece_len, peer_b).unwrap();

        state.clear_block(piece, begin_b, peer_a);
        assert!(state
            .requested_block_length(piece, begin_b, peer_b)
            .is_some());
        state.clear_block(piece, begin_b, peer_b);
        assert!(state
            .requested_block_length(piece, begin_b, peer_b)
            .is_none());
        assert!(state
            .requested_block_length(piece, begin_a, peer_a)
            .is_some());
    }

    #[test]
    fn test_have_bytes_sums_actual_piece_lengths() {
        // 3 pieces: 2 full (16 KiB) + 1 short last piece (8 KiB) = 40 KiB.
        let mut tracker = PieceTracker::new(3);
        let piece_len = |index: u32| {
            if index == 2 {
                8 * 1024u64
            } else {
                16 * 1024u64
            }
        };
        assert_eq!(tracker.have_bytes(&piece_len), 0);
        tracker.mark_have(0);
        tracker.mark_have(2);
        assert_eq!(tracker.have_bytes(&piece_len), 16 * 1024 + 8 * 1024);
        assert_eq!(tracker.have_count(), 2);
    }

    #[test]
    fn test_mark_have_clears_requested_blocks() {
        let mut state = SchedulerState::new(1);
        state.tracker.set_peer_bitfield([1u8; 20], &[0x80]);
        let piece_len = |_index: u32| 16 * 1024u64;
        let _ = state.next_block(&piece_len, [1u8; 20]).unwrap();
        assert!(!state.requested_blocks.is_empty());
        state.mark_have(0);
        assert!(state.requested_blocks.is_empty());
    }
}
