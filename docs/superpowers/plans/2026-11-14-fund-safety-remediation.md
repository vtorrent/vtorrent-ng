# Fund-safety remediation implementation plan

**Goal:** Address the BTC reorg, offline cold-stake fee, and broadcast-status findings without changing consensus or touching the active soak.

**Constraints:** Preserve existing wallet data and legacy recovery. Never infer that a broadcast failed or that a reorg removed a transaction from mempools. Do not stage unrelated files or generated schemas. Independent human review and a new exact-image soak remain release gates.

## Tasks

- [ ] BTC send history: covered reorg, restart, journal prune, corrupt journal, competing confirmation, and multi-input sends are covered by unit tests, plus an isolated Bitcoin Core reorg test and a competing-spend reorg test. Persistence syncs the parent directory on Unix and reports an uncertain save instead of rolling back past a completed rename; `set_utxo_path` adopts a path only after a successful write. Wallet files written before schema version 2 convert their unlabelled removals into fail-closed holds that a scan retires. **Open:** a legacy send whose journal entry was already pruned leaves no record to convert, reorgs deeper than the wallet birthday, non-Unix directory durability, and actual power-loss testing.
- [ ] Offline P2CS signing: the CLI derives the input value from a supplied funding transaction and verifies its txid, outpoint, and script; it shows amount, fee, and change, requires a maximum fee and interactive confirmation, and rejects a fee above 10%. A pseudo-terminal test covers the interactive flow, wrong txid, and excessive fee. **Open:** proving the funding output is confirmed and unspent.
- [x] BTC broadcast status: a timeout without a reject now reports submission, not acceptance, in the code comment, the log line, the RPC documentation, and the desktop wording. Tests cover a silent peer (submission), an explicit reject (error carrying the peer reason), a disconnect after the peer received the bytes (error), a successful rebroadcast that does not retire the record, and a scan-observed spend that retires it.
- [x] Reassess BTC claim replacement validation. Recovery now rejects a mismatched raw copy, an altered witness, a fee that disagrees with the output, a wrong destination, and a replacement chain whose terms disagree with the order. **Open:** trust in the recorded funding terms themselves and independent review.
- [ ] Run focused tests, full workspace tests, formatting, clippy and frontend checks. Report every failure and remaining limitation. Do not claim independent human review or release readiness.
