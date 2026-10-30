# Codebase-wide review, 2026-10-29

## Verdict

**All code findings remediated; release remains gated on independent review and
operational deployment.** This pass found ten high-severity issues across fund
movement, consensus relay, network isolation, and release operations. The fixes
are in the working tree and pass the full workspace and frontend suites. This is
still an internal model review, not independent human signoff.

Reviewed code: `dd98176fcb6eb724e0753df8ee47dba40df9dc22`. The remediation changes
are uncommitted at review time.

## Remediation status

- H1-H9: fixed in the working tree.
- H10: `deploy/seeds-monitoring/deadman.sh` added; it must still be installed on
  seed2/seed3.
- M1-M3 and M5-M12: fixed in the working tree.
- M4: TOTP replay is now checked for wallet unlock via a sidecar step file; the
  2FA enable/disable path still uses the older verifier.
- M6: existing torrent directories need the `.vtorrent-torrent` marker or a
  migration before resuming.

Verification: full workspace 1,122 passed, frontend 42 passed, Clippy,
formatting, machete, Prometheus rules, and default Docker startup passed.

## High severity

### H1. Fee-bump endpoints can destroy nearly the whole HTLC value

`vtorrent-rpc/src/btc_claim_bump.rs:39-56` checks only `approve` and durable
recovery; it does not require an unlocked wallet or `verify_wallet_auth`.
`btc_claim_bump.rs:126-132` checks only that the new fee is higher, and
`btc_claim_bump.rs:152-184` signs and broadcasts with the node's BTC seed.
`vtorrent-rpc/src/refund_bump.rs:181-200` requires an unlocked wallet but not a
passphrase/TOTP, and has no upper fee bound. Both routes are protected only by
the transport API key.

An API-key holder, or a local caller when the default no-key loopback mode is
used, can set the fee close to the HTLC amount, leaving only dust to the
recipient. The BTC variant works while the VTR wallet is locked. The replacement
is persisted before broadcast, so a rejected replacement can also corrupt the
recoverable claim state. Source-traced; no live broadcast performed.

### H2. Wallet import can irreversibly overwrite a wallet without current authentication

`vtorrent-rpc/src/handlers/wallet.rs:202-212` accepts `overwrite: true` without
verifying the existing wallet's passphrase or TOTP. Lines 255-265 replace the
encrypted wallet, and 279-330 atomically overwrite `wallet.json` with no
pre-overwrite backup. The default no-key loopback mode removes even the API-key
barrier.

A caller can replace the custody key and make the old key unrecoverable. This is
an incomplete guard around the prior wallet-overwrite fix. Source-traced.

### H3. Desktop cold-stake persists the cold spending WIF in the online wallet

`vtorrent-tauri/src/commands/wallet.rs:778-803` calls
`wallet.generate_cold_stake_keys(...)` and then `persist_wallet`.
`vtorrent-wallet/src/wallet.rs:334-342` pushes the whole `ColdStakeKeys`,
including `spending_wif`, into `WalletData.cold_stake`; `Wallet::save` encrypts
and writes it. The UI at `vtorrent-ui/src/pages/StakingPage.tsx:547-559` says the
spending key is not stored by the node.

The RPC path uses the free `generate_cold_stake_keys` function and does not
persist it. The desktop path defeats the cold-key separation and exposes the
spending key through wallet backups. Source-traced.

### H4. BTC swap spending bypasses the per-operation reauthorization policy

`vtorrent-rpc/src/handlers/swap.rs:597-618` claims BTC without an unlock or
passphrase check. `handlers/swap.rs:258-263` funds BTC with only
`is_wallet_unlocked`, not `verify_wallet_auth`. `btc_claim_bump.rs` has no
unlock/auth check. The Tauri `send_btc` command
(`vtorrent-tauri/src/commands/wallet.rs:1007-1041`) also ignores the UI
passphrase, and `useBtc.tsx` drops it in Tauri mode.

The `/btc/send` fix is not applied to the other BTC-spending paths. An API-key
caller, or an unlocked desktop session, can move BTC without the passphrase the
policy requires. Source-traced.

### H5. Consensus block-size accounting does not match the wire encoding

`vtorrent-node/src/block.rs:192-217` estimates `serialized_size()` with a
hand-rolled layout. `vtorrent-node/src/consensus.rs:288-295` uses that estimate
for `MAX_BLOCK_SIZE`, and the staking/reorg template paths use the same value.
`vtorrent-node/src/node/chain.rs:42-50` limits bincode block decoding to
`MAX_BLOCK_SIZE` over the actual payload.

The estimator omits bincode enum/length prefixes and the block header, so a
block accepted locally can serialize above the receiver's decode limit. A full
mempool or malicious staker can produce unrelayable blocks and fork itself off
the network. Source-traced; an exact-size regression was not run.

### H6. Tracker and DHT peer addresses bypass the private-network policy

`vtorrent-torrent/src/engine.rs:336-340,410-413,509` dials tracker/DHT peers
directly. Tracker and DHT parsers
(`tracker.rs:434-454`, `udp.rs:164-184`, `dht.rs:214-234`) do not apply the
`is_non_public`/tracker-target checks used for tracker URLs. A malicious tracker
or DHT node can make the node connect to loopback, RFC1918, link-local, or
metadata-service addresses.

This is a TCP BitTorrent SSRF/port-scanning primitive, not an HTTP SSRF. No live
internal target was contacted in this review. Source-traced.

### H7. Overlay peers are never disconnected, exhausting the peer table

`vtorrent-overlay/src/overlay.rs:78` defines `PeerDisconnected`, but the overlay
receive path only emits `PeerConnected` (`overlay.rs:347-351`). The node's
removal path is `OverlayIngress::PeerDisconnected`
(`vtorrent-node/src/node/peering.rs:166-201`), which is unreachable. Session TTL
eviction in `holepunch.rs` does not notify the node.

After roughly 125 overlay handshakes, `PeerManager` reaches `MAX_PEERS` and
rejects new outbound and virtual peers until restart. Source-traced.

### H8. The published image cannot start with its default data directory

`docker/Dockerfile:44-47` switches to `vtorrent` and declares
`VOLUME /home/vtorrent/.vtorrent` without creating or chowning the directory.
Running the candidate image with no overrides produced:

```text
Error: Data directory "/home/vtorrent/.vtorrent" is not writable: Permission denied
```

The soak compose works only because it runs as root. Runtime-confirmed.

### H9. Desktop release builds bypass the reviewed lockfile

`.github/workflows/build.yml:188-263` runs `tauri-action` without copying
`release/Cargo.lock` or passing `--locked`. The release upload at lines 265-272
attaches the static `release/Cargo.lock`, not the lock Cargo resolved during the
build. A fresh runner has no root `Cargo.lock` because it is gitignored.

Release binaries can be built from an unreviewed dependency graph while a
different lockfile is published as exact. Source-traced.

### H10. Monitoring has no external dead-man switch

Prometheus, Alertmanager, Grafana, and the ntfy relay all run on `vtr-seed1`
(`deploy/seeds-monitoring/README.md:3-14`). `DaemonDown` is evaluated by the same
Prometheus that would be lost with seed1. No external heartbeat or independent
monitoring exists.

If seed1 or the relay dies, alerting stops silently. Source/docs-confirmed; no
failure drill was run.

## Medium severity

1. **P2CS cold stake is never staked by the production loop.** The staking loop
   uses `get_utxos_for_address` (`vtorrent-node/src/node/staking_loop.rs:57`),
   while the P2CS-aware function is used only for UI/status
   (`vtorrent-node/src/staking.rs:98`). Cold outputs dilute `total_staked` but
   never enter the kernel.
2. **External BTC claim preimage is discarded.** A spend that is not the local
   `btc_claim_txid` is classified `SpentElsewhere`
   (`vtorrent-rpc/src/btc_reconciliation.rs:203-225`), so the extracted preimage
   is not stored and monitoring stops. The automatic cross-node handoff fails;
   the manual external claim endpoint remains.
3. **`spent_reserved` survives a BTC reorg.** `clear_chain_utxos` clears chain
   UTXOs but not `spent_reserved`
   (`vtorrent-btc/src/utxo.rs:109-117`), allowing a stale spend to authorize
   reservation release after a reorg.
4. **TOTP replay is possible outside RPC.** `Wallet::load` and Tauri
   `open_wallet` verify a code without persisting a used step; the RPC guard is
   in-memory and resets on restart.
5. **The WebSocket has no Origin validation and is unauthenticated in no-key
   mode.** A browser page can subscribe to peer and staking events, or occupy
   the 128 slots.
6. **Torrent downloads can write into pre-existing sensitive directories under
   the chosen root.** The symlink fix does not reject an existing `.ssh`
   directory or overwrite existing regular files.
7. **Legacy wallet parsing has unbounded KDF work.** Many high-iteration `mkey`
   records can block the synchronous import/UI for minutes.
8. **P2P version strings are unbounded.** A 4 MiB `user_agent` is retained per
   peer; 64 peers can consume hundreds of MiB.
9. **Torrent tracker/peer/DHT waits lack timeouts.** A silent tracker or peer can
   pin a session indefinitely.
10. **Peer-wire `Bitfield` length is not checked before a bit loop.** A 16 MiB
    payload forces roughly 134 million iterations.
11. **`getheaders`/`getblocks` scan the chain for an unknown locator.** A peer
    can force O(chain height) work per request.
12. **`rollback_tip` leaks abandoned block bodies.** Reorgs grow `chain.db`
    permanently.

## Coverage and limitations

Reviewed by five parallel source reviewers, then spot-validated locally:
consensus/script/store, wallet/BTC/swap, P2P/overlay/onion/SPV/torrent,
RPC/Tauri/UI/migration/snapshot/CLI, and build/CI/deployment/operations.

The Docker data-directory failure was reproduced. The remaining findings are
source-traced; they were not all exploited end to end. No full workspace test
run or independent human review was performed for this pass. The earlier
whole-project findings F01-F06 are fixed and out of scope unless a bypass is
listed above.

This report is uncommitted. No code was changed.
