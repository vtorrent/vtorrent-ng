# Frontier-model review — 2026-10-23

- Reviewer: Codex CLI configured with `gpt-6-astra`, read-only source review.
- Checkout: `83e55e5` (code candidate `77744bf`).
- Scope: recent desktop/security changes plus the critical wallet, cold-stake,
  RPC, BTC SPV, and consensus paths.
- Result: 2 High, 6 Medium, 2 Low. No files were modified.
- This is a model review, not the independent human security review.

## Status

| # | Severity | Finding | Status |
|---|---|---|---|
| 1 | High | BTC reorg recovery deletes wallet recovery metadata | **FIXED** in `61f4be4`; regression test preserves reservations, contracts, pending transactions, and address index |
| 2 | High | Desktop wallet switching leaves the previous BTC wallet active | **FIXED** in `cb765ce`; wallet switches are rejected while the node runs, and the BTC UTXO file is scoped by wallet seed |
| 3 | Medium | Mempool admission omits the P2CS cold-spend authorization rule | **FIXED** in `f272e64`; hot-key P2CS spends are rejected, cold-key spends are admitted |
| 4 | Medium | Wallet-path confinement does not validate the final file | **FIXED** in `326ce27`; final targets are canonicalized, validation runs before directory creation, and symlink-escape tests cover open/create |
| 5 | Medium | Cold-stake registry writes are not concurrency-safe or crash-durable | **FIXED** in `6f82b63`; updates are serialized, corrupt files are not overwritten, temp files are exclusive, and the file and directory are fsynced |
| 6 | Medium | BTC discovery updates are not persisted by the normal scan path | **FIXED** in `3975693`; successful scans save the UTXO set and the scan cursor is persisted with it |
| 7 | Medium | Desktop BTC DNS failures bypass UTXO rollback; fallback broadcast ignores network | **FIXED** in `326ce27` and `07a6881`; broadcast errors flow through UTXO rollback and non-mainnet broadcasts require a configured peer |
| 8 | Medium | Cold-stake funding authorization survives desktop wallet changes | **FIXED** in `326ce27`; session scripts are cleared on wallet transitions, creation requires an active wallet, and tests cover both |
| 9 | Low | WebSocket connection capacity is checked without reserving a slot | **FIXED** in `07a6881`; the slot is reserved atomically before upgrade and released by a guard |
| 10 | Low | Some desktop signing paths leave ordinary WIF strings unzeroized | **FIXED** in `326ce27`; send, cold-stake funding, and legacy-claim WIFs are wrapped in `Zeroizing` |

## Follow-up fixes

- **SPV P2CS principal preservation** — `39da204`. The verifier now requires at
  least the staked value to re-lock to the P2CS script, and the test re-signs
  redirect and burn coinstakes so the R1 checks are actually exercised.
- **BTC reservation release** — `f803cf9`, with the reserved-UTXO serialization fix in `afe5974`. Expiry alone no longer releases a
  reservation: the reconciler requires a confirmed spend of the reserved input,
  which makes the signed funding transaction impossible to confirm. The
  claim-after-expiry test documents why the claim branch's missing upper time
  bound matters.
- **Passphrase retention** — `2287026`. The documentation now states that the
  passphrase is held in a zeroizing buffer while the wallet is unlocked and
  zeroized on lock/drop; it is not erased immediately after key derivation.

## Verification

- Full workspace suite (`cargo test --workspace --all-features`) passes at
  `3c580c1`.
- Targeted suites: 13 Tauri tests, 22 frontend tests, and the wallet, BTC,
  node, and RPC crate suites; strict Clippy and formatting pass.

## Raw review

Reviewed checkout `83e55e5bea26b1b37306f36cfdd07aa2b8f0f3b6`, including both review documents and the recent desktop changes. No files were modified. This was a source review; I did not run builds or tests because those write files.

I found two High-severity issues and several Medium-severity gaps. “Confirmed” below means the failure follows from the inspected code, not that I executed an exploit.

1. **High — BTC reorg recovery deletes wallet recovery metadata. Confirmed.**  
   [vtorrent-btc/src/wallet.rs:456](/home/pnoch/Development/vtorrent-ng/vtorrent-btc/src/wallet.rs:456), [utxo.rs:43](/home/pnoch/Development/vtorrent-ng/vtorrent-btc/src/utxo.rs:43)

   Reorg handling replaces the entire `UtxoSet` with its default and immediately persists it. That object also contains reserved inputs, signed pending swap transactions, swap contracts, and the persisted address index.

   **Scenario:** A reorg occurs while BTC funding remains pending. Rescanning can make its input selectable again because its reservation disappeared. After restart, the address index also returns to zero, so previously issued higher-index addresses are no longer watched. This can hide funds and disrupt swap recovery.

   **Minimal fix:** Clear only chain-derived UTXOs and observations. Preserve address indexes, contracts, signed transactions, and reservations; reconcile reservations against canonical chain evidence. The claimed reorg-safe persistence fix is incomplete.

2. **High — Desktop wallet switching leaves the previous BTC wallet active. Confirmed.**  
   [vtorrent-tauri/src/state.rs:81](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/state.rs:81), [commands/node.rs:171](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/node.rs:171), [commands/wallet.rs:774](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/wallet.rs:774)

   BTC keys are initialized when the node starts. Opening another wallet updates the VTR hot key but does not rebuild the BTC wallet. Creating or importing another desktop wallet does not even synchronize the existing RPC hot-wallet state.

   **Scenario:** Start with wallet A, lock it, then open wallet B. B makes `is_wallet_unlocked()` true, but `send_btc` still spends A’s BTC. Starting the node before opening any wallet leaves BTC uninitialized after opening one.

   BTC persistence compounds this: all wallets in one directory use the same `btc_utxos-{network}.json`, without a wallet-identity check on load.

   **Minimal fix:** Make wallet activation an atomic operation covering desktop state, RPC keys, BTC keys/cache, staking, and recovery paths. Scope BTC files by wallet identity and network. Until implemented, reject wallet changes while the node is running.

3. **Medium — Mempool admission omits the P2CS cold-spend authorization rule. Confirmed.**  
   [vtorrent-node/src/chain.rs:837](/home/pnoch/Development/vtorrent-ng/vtorrent-node/src/chain.rs:837), [chain/chain_reorg.rs:594](/home/pnoch/Development/vtorrent-ng/vtorrent-node/src/chain/chain_reorg.rs:594), [mempool.rs:542](/home/pnoch/Development/vtorrent-ng/vtorrent-node/src/mempool.rs:542)

   Block validation requires the cold spending key for ordinary P2CS spends. Mempool verification only executes the script, whose hot branch can succeed for an ordinary transaction.

   **Scenario:** A malicious staking delegate submits a hot-key-signed ordinary spend with a large apparent fee. It cannot confirm, but admission reserves the outpoint and applies conflict/replacement rules to the owner’s legitimate spend. It can also survive mempool reload. The default stale-entry lifetime is 48 hours.

   **Minimal fix:** Share P2CS authorization checks between block validation and mempool admission. Reject these transactions before conflict indexing or relay.

4. **Medium — Wallet-path confinement does not validate the final file. Confirmed.**  
   [vtorrent-tauri/src/commands/wallet.rs:30](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/wallet.rs:30), [wallet.rs:205](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/wallet.rs:205)

   The validator canonicalizes only the parent. `open_wallet` subsequently follows a wallet-file symlink. Creation/import also call `create_dir_all` before validating confinement.

   **Scenario:** An existing `app-data/alias.vtr` symlink points outside the permitted directory. Opening it passes validation and reads the external wallet, given its credentials. Separately, a rejected creation request can still create directories outside app data.

   **Minimal fix:** Validate before filesystem mutation. Resolve and confine existing targets, reject unsupported symlinks, and use directory-relative, no-follow operations to avoid check/use races. The path-hardening claim is incomplete.

5. **Medium — Cold-stake registry writes are neither concurrency-safe nor crash-durable. Confirmed.**  
   [vtorrent-wallet/src/cold_stake_registry.rs:24](/home/pnoch/Development/vtorrent-ng/vtorrent-wallet/src/cold_stake_registry.rs:24)

   Registration performs an unlocked read/modify/write using one predictable temporary filename. It neither flushes the file nor syncs the directory. Loading treats malformed or unreadable registries as empty.

   **Scenario:** Concurrent creation requests can lose registrations or interfere with the same temporary file. A power failure after success can lose an acknowledged registration. A later addition can silently replace a corrupt registry with a partial one. Restart funding then rejects previously generated scripts.

   **Minimal fix:** Serialize updates per registry, fail explicitly on corruption, use exclusively created temporary files, and sync the file and directory. Protect against temporary-file symlinks.

6. **Medium — BTC discovery updates are not persisted by the normal scan path. Confirmed.**  
   [vtorrent-btc/src/wallet.rs:478](/home/pnoch/Development/vtorrent-ng/vtorrent-btc/src/wallet.rs:478), [sync.rs:433](/home/pnoch/Development/vtorrent-ng/vtorrent-btc/src/sync.rs:433)

   Scanning adds outputs and removes spent inputs in memory, but the wallet wrapper does not save afterward. Advancing the scan cursor also does not persist anything.

   **Scenario:** Receive BTC, complete discovery, then restart before another operation happens to save the set. The discovered balance disappears until rescanning. Conversely, an externally spent output can remain in the persisted cache.

   **Minimal fix:** Persist successful scan results with their canonical checkpoint and address index. Revalidate cached state before spending. Existing persistence tests do not establish that this live discovery path persists.

7. **Medium — Desktop BTC DNS failures bypass UTXO rollback; fallback broadcasting ignores the configured network. Confirmed.**  
   [vtorrent-tauri/src/commands/wallet.rs:788](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/wallet.rs:788)

   `send_to` removes and persists selected inputs before broadcasting. The DNS-resolution `?` exits before the restoration block. With no configured peer, the command calls `broadcast_tx`, which explicitly uses Bitcoin mainnet.

   **Scenario:** A temporary DNS failure makes spendable inputs disappear locally despite no broadcast. On a non-mainnet wallet, the fallback sends the transaction to mainnet peers.

   **Minimal fix:** Use the shared network-aware RPC broadcast helper and route every post-selection error through rollback. Require an appropriate peer when network-specific discovery is unavailable.

8. **Medium — Cold-stake funding authorization survives desktop wallet changes. Confirmed.**  
   [vtorrent-tauri/src/commands/wallet.rs:642](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/wallet.rs:642), [state.rs:97](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/state.rs:97)

   The in-memory script allowlist belongs to the application session, not a wallet. Locking or switching wallets never clears it, and funding accepts either this set or the current wallet’s registry.

   **Scenario:** Generate a script while wallet A is active, then open wallet B. B can fund A’s script despite the stated “generated by this wallet” restriction. Creation without an active wallet is also accepted.

   **Minimal fix:** Bind registrations to wallet identity and clear session authorization on wallet transitions. Require an active wallet for wallet-scoped registration.

9. **Low — WebSocket connection capacity is checked without reserving a slot. Confirmed.**  
   [vtorrent-rpc/src/ws.rs:178](/home/pnoch/Development/vtorrent-ng/vtorrent-rpc/src/ws.rs:178), [ws.rs:243](/home/pnoch/Development/vtorrent-ng/vtorrent-rpc/src/ws.rs:243)

   The handler checks the count before upgrade but increments it afterward.

   **Scenario:** Concurrent upgrades all observe available capacity and exceed the advertised 128-connection limit. With authentication enabled, valid tokens are required; with authentication disabled, they are not.

   **Minimal fix:** Acquire an owned semaphore permit before accepting the upgrade and retain it for the connection lifetime.

10. **Low — Some desktop signing paths leave ordinary WIF strings unzeroized. Confirmed.**  
    [vtorrent-tauri/src/commands/wallet.rs:495](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/wallet.rs:495), [wallet.rs:648](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/wallet.rs:648), [wallet.rs:839](/home/pnoch/Development/vtorrent-ng/vtorrent-tauri/src/commands/wallet.rs:839)

    Sending VTR and funding cold stakes copy WIFs into ordinary `String`s. Legacy-claim submission also receives an ordinary WIF string.

    **Scenario:** After signing and locking, freed heap allocations can retain key material recoverable through a memory disclosure or dump.

    **Minimal fix:** Wrap owned WIF values in `Zeroizing` immediately, including error paths. This reduces exposure but cannot erase copies already serialized through IPC.

The inspected full-node **R1 rule** rejects value redirection and principal destruction, and consensus restricts coinstakes to one input. I found no confirmed hot-key theft bypass there. The mempool discrepancy above remains material.

The **wallet encryption** implementation uses the stated Argon2id parameters, fresh salts and nonces, authenticated encryption, and zeroizing plaintext/derived-key containers. I found no confirmed nonce-reuse or authentication defect. However, `Wallet` retains the passphrase for subsequent saves; the claim that passphrases are erased immediately after derivation is not true for this implementation.

The **WebSocket token repair** does atomically consume tokens and enforce expiry. Sensitive RPC routes inspected are behind API-key middleware, and cold-key responses set `no-store`. These observations do not eliminate the connection-cap race.

Two unresolved concerns need separate validation:

- The dormant SPV P2CS verifier checks redirection but lacks the full node’s principal-preservation check. PoS header acceptance currently fails closed, so this is not a demonstrated live acceptance vulnerability.
- BTC reservation release assumes an expired HTLC cannot be claimed. The Bitcoin script’s claim branch has no upper time bound; expiry enables refunds without disabling claims. Late funding and competing claim/refund transactions need an explicit adversarial test before relying on that assumption.

**Top 5 actions**

1. Preserve BTC recovery metadata across reorgs.
2. Make wallet switching atomic and scope BTC state by wallet identity.
3. Apply P2CS authorization rules during mempool admission.
4. Complete wallet-path confinement and durable registry writes.
5. Persist BTC scan results and unify network-aware broadcasting with rollback.
