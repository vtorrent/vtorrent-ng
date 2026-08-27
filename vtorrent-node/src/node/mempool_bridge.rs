// Lock order: chain → mempool — always acquire Chain before Mempool to avoid deadlock.
//! Mempool bridge — confirmed-block pruning and pending filter assembly.

/// Remove confirmed transactions and spent UTXOs from the mempool.
///
/// Mirrors `Mempool::handle_confirmed_block`. Lock order `chain → mempool`
/// explicit: callers lock `Chain` before `Mempool`.
pub fn handle_confirmed_block() {
    // shim — real work is `Mempool::handle_confirmed_block` called from `Node::handle_message`
}

/// Assemble a filter of pending transactions for compact-block reconstruction.
///
/// Corresponds to `assemble_pending_filter` / `pending_compact_blocks` logic.
pub fn assemble_pending_filter() {
    // stub for pending filter assembly
}
