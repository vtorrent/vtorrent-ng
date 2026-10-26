#!/usr/bin/env bash
set -euo pipefail

cargo test -p vtorrent-btc --lib interrupted_header_reorg_does_not_commit_partial_state
cargo test -p vtorrent-btc --lib restored_chain_cache_is_rebuilt_after_first_header_sync
cargo test -p vtorrent-btc --lib settlement_scan_follows_higher_work_fork_and_invalidates_old_spend
cargo test -p vtorrent-btc --lib silent_and_chatty_header_peers_reach_healthy_fallback
cargo test -p vtorrent-btc --lib peer_chatter_cannot_extend_filter_or_block_deadlines
cargo test -p vtorrent-btc --lib filters::tests
cargo test -p vtorrent-rpc --lib btc_peers::tests
