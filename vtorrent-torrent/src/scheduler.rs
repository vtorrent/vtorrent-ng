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
    /// Per-peer bitfields as packed bits: peer_id -> pieces the peer has.
    ///
    /// Packed, not one byte per piece: 200 peers on a 1M-piece torrent would
    /// otherwise cost 200 MB just for availability.
    peer_pieces: HashMap<[u8; 20], Vec<u8>>,
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

    fn bitfield_len(&self) -> usize {
        (self.piece_count as usize).div_ceil(8)
    }

    fn bits_have(bits: &[u8], index: u32, piece_count: u32) -> bool {
        let index = index as usize;
        if index >= piece_count as usize {
            return false;
        }
        bits.get(index / 8)
            .is_some_and(|byte| byte & (0x80 >> (index % 8)) != 0)
    }

    /// Record a peer's bitfield.
    pub fn set_peer_bitfield(&mut self, peer_id: [u8; 20], bits: &[u8]) {
        let len = self.bitfield_len();
        let mut have = vec![0u8; len];
        for (dst, src) in have.iter_mut().zip(bits.iter()) {
            *dst = *src;
        }
        self.peer_pieces.insert(peer_id, have);
    }

    /// Record a peer's `Have` message.
    pub fn set_peer_have(&mut self, peer_id: [u8; 20], index: u32) {
        let len = self.bitfield_len();
        let entry = self
            .peer_pieces
            .entry(peer_id)
            .or_insert_with(|| vec![0u8; len]);
        if (index as usize) < self.piece_count as usize {
            entry[index as usize / 8] |= 0x80 >> (index % 8);
        }
    }

    /// Count how many peers have a given piece.
    pub fn peer_count_for(&self, index: u32) -> usize {
        let piece_count = self.piece_count;
        self.peer_pieces
            .values()
            .filter(|bits| Self::bits_have(bits, index, piece_count))
            .count()
    }

    /// Forget a peer's availability when its task exits.
    pub fn remove_peer(&mut self, peer: &[u8; 20]) {
        self.peer_pieces.remove(peer);
    }

    /// Whether this specific peer advertised the piece.
    pub fn peer_has_piece(&self, peer: &[u8; 20], index: u32) -> bool {
        self.peer_pieces
            .get(peer)
            .is_some_and(|bits| Self::bits_have(bits, index, self.piece_count))
    }

    pub fn piece_count(&self) -> u32 {
        self.piece_count
    }

    pub fn is_requested(&self, index: u32) -> bool {
        (index as usize) < self.requested.len() && self.requested[index as usize]
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
    /// A block that arrived is held in that task's assembler. It must keep its
    /// lease (so it is not re-requested) but no longer counts as in flight.
    received: bool,
}

/// One task's ownership of a partially assembled piece.
#[derive(Debug, Clone, Copy)]
struct PieceOwner {
    task: [u8; 20],
    /// Last time a block for this piece was accepted into the owner's
    /// assembler. An owner that stops making progress is reclaimed.
    last_progress: Instant,
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
    /// The task assembling each partially downloaded piece.
    ///
    /// Piece blocks live in that task's local assembler, so exactly one task
    /// may own a piece. Leasing blocks of one piece to two tasks would leave
    /// each assembler with a subset and the piece would never complete.
    piece_owner: HashMap<u32, PieceOwner>,
    /// Pieces reclaimed from a stalled owner. The owning task drains its own
    /// entries so it drops the local assembler and releases the assembly
    /// budget; otherwise a choked peer could pin both forever.
    reclaimed: Vec<(u32, [u8; 20])>,
    /// (piece, peer) pairs a peer stalled, with the time it may retry them.
    stalled: HashMap<(u32, [u8; 20]), Instant>,
    pub(crate) writing_pieces: std::collections::HashSet<u32>,
    /// Bytes currently held by per-peer piece assemblers across all peer
    /// tasks. Piece data is only verified once the whole piece arrives, so an
    /// attacker could otherwise make every peer task hold a maximum-size
    /// partial piece at the same time.
    assembling_bytes: usize,
}

/// Aggregate in-flight assembly budget shared by every peer task.
pub const MAX_ASSEMBLING_BYTES: usize = 64 * 1024 * 1024;

/// An owned piece that accepts no block for this long is reclaimed from its
/// task, so a choking or silent peer cannot pin it indefinitely.
pub const PIECE_STALL_TIMEOUT: Duration = Duration::from_secs(180);
/// A peer that stalled a piece is not offered it again for this long.
const PIECE_STALL_COOLDOWN: Duration = Duration::from_secs(60);

impl SchedulerState {
    pub fn new(piece_count: u32) -> Self {
        Self {
            tracker: PieceTracker::new(piece_count),
            block_size: 16 * 1024,
            max_pipelined_blocks: 5,
            endgame_threshold: 3,
            requested_blocks: HashMap::new(),
            piece_owner: HashMap::new(),
            reclaimed: Vec::new(),
            stalled: HashMap::new(),
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
        let mut released_pieces: Vec<u32> = Vec::new();
        for (piece, leases) in self.requested_blocks.iter_mut() {
            leases.retain(|lease| {
                // A received block is in the owner's assembler and must keep
                // its lease so it is never re-requested. Only a block that
                // never arrived becomes requestable again.
                lease.received || now.duration_since(lease.leased_at) < BLOCK_LEASE_TIMEOUT
            });
            if leases.is_empty() {
                released_pieces.push(*piece);
            }
        }
        self.requested_blocks.retain(|_, leases| !leases.is_empty());
        for piece in released_pieces {
            self.piece_owner.remove(&piece);
            self.tracker.clear_requested(piece);
        }

        // A received block keeps its lease forever, which is correct for
        // avoiding re-requests but lets a peer that delivers one block and
        // then chokes pin the piece to its task. Reclaim pieces with no
        // accepted block for the stall window and cool the peer down so it is
        // not immediately handed the same piece again.
        let stalled: Vec<(u32, [u8; 20])> = self
            .piece_owner
            .iter()
            .filter(|(_, owner)| now.duration_since(owner.last_progress) >= PIECE_STALL_TIMEOUT)
            .map(|(piece, owner)| (*piece, owner.task))
            .collect();
        for (piece, task) in stalled {
            self.piece_owner.remove(&piece);
            self.requested_blocks.remove(&piece);
            self.tracker.clear_requested(piece);
            self.reclaimed.push((piece, task));
            self.stalled
                .insert((piece, task), now + PIECE_STALL_COOLDOWN);
            tracing::debug!(piece, "Reclaiming a stalled piece from its peer task");
        }
        self.stalled.retain(|_, until| *until > now);
    }

    /// Pick a piece this peer can make progress on.
    ///
    /// Prefer a piece the peer already owns, so its assembler can finish.
    /// Otherwise choose the rarest unowned, unrequested piece this peer
    /// advertised. Never lease a block of a piece the calling peer did not
    /// advertise: a well-behaved peer would ignore it and the block would be
    /// stuck until its lease expired.
    fn select_piece(&self, peer: &[u8; 20]) -> Option<u32> {
        let mut owned: Option<(usize, u32)> = None;
        for (index, owner) in &self.piece_owner {
            if owner.task != *peer || self.tracker.has_piece(*index) {
                continue;
            }
            let count = self.tracker.peer_count_for(*index);
            if owned.is_none_or(|(best, _)| count < best) {
                owned = Some((count, *index));
            }
        }
        if let Some((_, index)) = owned {
            return Some(index);
        }

        let mut best: Option<(usize, u32)> = None;
        for index in 0..self.tracker.piece_count() {
            if self.tracker.has_piece(index)
                || self.tracker.is_requested(index)
                || self.piece_owner.contains_key(&index)
                || !self.tracker.peer_has_piece(peer, index)
                || self
                    .stalled
                    .get(&(index, *peer))
                    .is_some_and(|until| *until > Instant::now())
            {
                continue;
            }
            let count = self.tracker.peer_count_for(index);
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

    /// Select the next block to request: (piece, begin, length).
    ///
    /// Picks a piece this peer can serve, then the next unrequested block
    /// within that piece. `piece_len` maps a piece index to its byte length.
    /// Returns `None` when no block is available.
    pub fn next_block(
        &mut self,
        piece_len: &dyn Fn(u32) -> u64,
        peer: [u8; 20],
    ) -> Option<(u32, u32, u32)> {
        self.purge_expired_leases();
        let piece = self.select_piece(&peer)?;
        self.piece_owner.entry(piece).or_insert(PieceOwner {
            task: peer,
            last_progress: Instant::now(),
        });
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
                received: false,
            });
            self.tracker.mark_requested(piece);
            return Some((piece, begin, block_len));
        }
        // All blocks of this piece are already leased or received.
        self.tracker.mark_requested(piece);
        None
    }

    /// Mark that a leased block actually arrived and is held by the owner.
    pub fn mark_received(&mut self, piece: u32, begin: u32, peer: [u8; 20]) -> bool {
        if let Some(leases) = self.requested_blocks.get_mut(&piece) {
            if let Some(lease) = leases
                .iter_mut()
                .find(|lease| lease.begin == begin && lease.owner == peer)
            {
                lease.received = true;
                if let Some(owner) = self.piece_owner.get_mut(&piece) {
                    owner.last_progress = Instant::now();
                }
                return true;
            }
        }
        false
    }

    /// Unreceived blocks leased to one task, i.e. its real pipeline depth.
    pub fn outstanding_for(&self, peer: [u8; 20]) -> usize {
        self.requested_blocks
            .values()
            .flatten()
            .filter(|lease| lease.owner == peer && !lease.received)
            .count()
    }

    /// Expire stale leases, then return this task's real pipeline depth and
    /// any of its pieces the scheduler reclaimed from a stall.
    pub fn refresh_outstanding(&mut self, peer: [u8; 20]) -> (usize, Vec<u32>) {
        self.purge_expired_leases();
        let reclaimed: Vec<u32> = self
            .reclaimed
            .iter()
            .filter(|(_, task)| *task == peer)
            .map(|(piece, _)| *piece)
            .collect();
        self.reclaimed.retain(|(_, task)| *task != peer);
        (self.outstanding_for(peer), reclaimed)
    }

    /// Release every piece a task was assembling, making its blocks requestable
    /// by another task. Called when the task exits.
    pub fn release_owned(&mut self, peer: [u8; 20]) {
        self.tracker.remove_peer(&peer);
        self.reclaimed.retain(|(_, task)| *task != peer);
        self.stalled.retain(|(_, task), _| *task != peer);
        let owned: Vec<u32> = self
            .piece_owner
            .iter()
            .filter(|(_, owner)| owner.task == peer)
            .map(|(piece, _)| *piece)
            .collect();
        for piece in owned {
            self.requested_blocks.remove(&piece);
            self.piece_owner.remove(&piece);
            self.tracker.clear_requested(piece);
        }
    }

    /// Drop all tracking for one piece so it can be fetched again after a
    /// verification or disk-write failure.
    pub fn clear_piece(&mut self, piece: u32, task: [u8; 20]) {
        if self
            .piece_owner
            .get(&piece)
            .is_none_or(|owner| owner.task != task)
        {
            return;
        }
        self.requested_blocks.remove(&piece);
        self.piece_owner.remove(&piece);
        self.tracker.clear_requested(piece);
        self.stalled.retain(|(index, _), _| *index != piece);
    }

    /// Mark a block as no longer in flight (e.g. on cancel).
    pub fn clear_block(&mut self, piece: u32, begin: u32, peer: [u8; 20]) {
        if let Some(leases) = self.requested_blocks.get_mut(&piece) {
            // A received block is already in the owner's assembler. A remote
            // Cancel must not orphan it or let another task re-lease the
            // piece while that data is still held.
            leases.retain(|lease| lease.begin != begin || lease.owner != peer || lease.received);
            if leases.is_empty() {
                self.requested_blocks.remove(&piece);
                self.piece_owner.remove(&piece);
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
        self.piece_owner.remove(&index);
        self.stalled.retain(|(piece, _), _| *piece != index);
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
        let mut state = SchedulerState::new(4);
        // Peer A has pieces 0,1,2,3 (all).
        state.tracker.set_peer_bitfield([1u8; 20], &[0xF0]);
        // Peer B has pieces 0,1,2 but not 3.
        state.tracker.set_peer_bitfield([2u8; 20], &[0xE0]);
        // Piece 3 is rarest (1 peer) vs pieces 0,1,2 (2 peers).
        assert_eq!(state.select_piece(&[1u8; 20]), Some(3));
    }

    #[test]
    fn test_skips_have_and_requested() {
        let mut state = SchedulerState::new(4);
        state.tracker.set_peer_bitfield([1u8; 20], &[0xF0]);
        state.tracker.mark_have(0);
        state.tracker.mark_requested(1);
        // Pieces 0 (have) and 1 (requested) are skipped; 2 and 3 are tied.
        let next = state.select_piece(&[1u8; 20]).unwrap();
        assert!(next == 2 || next == 3);
    }

    #[test]
    fn a_piece_is_owned_by_one_peer_until_released() {
        let mut state = SchedulerState::new(2);
        state.tracker.set_peer_bitfield([1u8; 20], &[0xC0]);
        state.tracker.set_peer_bitfield([2u8; 20], &[0xC0]);
        let len = |_: u32| 32 * 1024u64;

        // Peer 1 takes piece 0 and leases its first block.
        let (piece, _, _) = state.next_block(&len, [1u8; 20]).unwrap();
        assert_eq!(piece, 0);
        // Peer 2 must not lease a block of peer 1's piece; it takes piece 1.
        let (other, _, _) = state.next_block(&len, [2u8; 20]).unwrap();
        assert_eq!(other, 1);

        // Releasing peer 1's ownership lets peer 2 take the piece.
        state.release_owned([1u8; 20]);
        assert!(!state.piece_owner.contains_key(&0));
    }

    #[test]
    fn a_peer_is_only_leased_pieces_it_advertised() {
        let mut state = SchedulerState::new(2);
        // Peer 1 has only piece 0; peer 2 only piece 1.
        state.tracker.set_peer_bitfield([1u8; 20], &[0x80]);
        state.tracker.set_peer_bitfield([2u8; 20], &[0x40]);
        let len = |_: u32| 32 * 1024u64;
        // Ask the peer that has only piece 1. The old scheduler would pick
        // the rarest (or first) piece 0 and lease it to this peer.
        let (piece, _, _) = state.next_block(&len, [2u8; 20]).unwrap();
        assert_eq!(piece, 1, "a peer must not be asked for a piece it lacks");
    }

    #[test]
    fn a_stalled_owner_is_reclaimed_and_the_peer_is_cooled_down() {
        let mut state = SchedulerState::new(2);
        state.tracker.set_peer_bitfield([1u8; 20], &[0xC0]);
        let len = |_: u32| 32 * 1024u64;
        let peer = [1u8; 20];
        let (piece, begin, _) = state.next_block(&len, peer).unwrap();
        assert!(state.mark_received(piece, begin, peer));

        // Pretend the accepted block was long ago and the peer then choked.
        state.piece_owner.get_mut(&piece).unwrap().last_progress =
            Instant::now() - PIECE_STALL_TIMEOUT - Duration::from_secs(1);
        let (_, reclaimed) = state.refresh_outstanding(peer);
        assert!(reclaimed.contains(&piece));
        assert!(!state.piece_owner.contains_key(&piece));

        // The stalled peer is not immediately handed the same piece again.
        let next = state.select_piece(&peer);
        assert_ne!(next, Some(piece));
    }

    #[test]
    fn received_blocks_keep_their_lease_but_leave_the_pipeline() {
        let mut state = SchedulerState::new(1);
        state.tracker.set_peer_bitfield([1u8; 20], &[0x80]);
        let len = |_: u32| 32 * 1024u64;
        let (piece, begin, _) = state.next_block(&len, [1u8; 20]).unwrap();
        assert_eq!(state.outstanding_for([1u8; 20]), 1);
        assert!(state.mark_received(piece, begin, [1u8; 20]));
        assert_eq!(
            state.outstanding_for([1u8; 20]),
            0,
            "a received block must free pipeline capacity"
        );
        assert_eq!(
            state.requested_block_length(piece, begin, [1u8; 20]),
            Some(16 * 1024),
            "the lease stays so the block cannot be re-requested"
        );
        // A remote Cancel for a block already delivered must not orphan the
        // stored block or hand the piece to another task while it is held.
        state.clear_block(piece, begin, [1u8; 20]);
        assert_eq!(
            state.requested_block_length(piece, begin, [1u8; 20]),
            Some(16 * 1024),
            "a received lease cannot be cancelled by the peer"
        );
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
        let mut state = SchedulerState::new(2);
        state.tracker.set_peer_bitfield([1u8; 20], &[0xC0]);
        state.tracker.set_peer_bitfield([2u8; 20], &[0xC0]);
        let piece_len = |_index: u32| 40 * 1024u64;
        let peer_a = [1u8; 20];
        let peer_b = [2u8; 20];
        // Each task owns a different piece, so their assemblers cannot split
        // one piece between them.
        let (piece_a, begin_a, _) = state.next_block(&piece_len, peer_a).unwrap();
        let (piece_b, begin_b, _) = state.next_block(&piece_len, peer_b).unwrap();
        assert_ne!(piece_a, piece_b);

        // Another task cannot cancel this task's lease.
        state.clear_block(piece_a, begin_a, peer_b);
        assert!(state
            .requested_block_length(piece_a, begin_a, peer_a)
            .is_some());
        state.clear_block(piece_a, begin_a, peer_a);
        assert!(state
            .requested_block_length(piece_a, begin_a, peer_a)
            .is_none());
        assert!(state
            .requested_block_length(piece_b, begin_b, peer_b)
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
