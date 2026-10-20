# Adversarial Security Review — 2026-10-20

Status: INTERNAL ADVERSARIAL REVIEW, NOT THE EXTERNAL GATE.
Reviewed revision: `26fec4b7cba01b9ee66311d47afbf1f6017142e8`
(`feat(desktop): load and persist chain state from the block store`).
Fix commits: `7c1e429` (P2CS authorization/burn), `1b0da9a` (BTC scan),
`b7a50e9` (v3 notfound), `5ce91d2` (desktop persistence), `80ccc52`
(raw-script cold-stake funding).

This review was performed by an AI assistant that also contributed to the
implementation. It is useful as a pre-audit and as a source of reproducible
regressions, but it does **not** satisfy the independent external review
required before mainnet.

## Method

- Source trace of the highest-value paths: cold staking (P2CS), BTC SPV,
  compact-block relay, desktop persistence, RPC/WebSocket boundary, wallet
  encryption.
- Six executable probes under
  `docs/review-probes/security-2026-10-20/` (standalone crate, not part of the
  workspace). Run:

```bash
CARGO_TARGET_DIR=target cargo test --offline \
  --manifest-path docs/review-probes/security-2026-10-20/Cargo.toml -- --nocapture
```

Result after the C-1/H-1 fixes: **6 passed, 0 failed**. The two cold-staking
probes now assert rejection; the other four still demonstrate open defects.
The probes are regression tests, not proofs of safety.

Verification after the C-1/H-1 fixes: `cargo test --workspace --no-fail-fast`
passed (exit 0), `cargo clippy --workspace --all-targets --all-features --
-D warnings` passed, and `cargo fmt --all -- --check` passed.

Limitations: no Semgrep/CodeQL, no fuzzing, no live mainnet-scale BTC sync, and
no GUI runtime test. Findings below are source-traced and, where marked,
reproduced by a probe.

## Findings

### C-1 — Critical (FIXED in `7c1e429`): the hot staking key could steal delegated P2CS principal

**Where:** `vtorrent-node/src/chain/chain_reorg.rs:604` and `:660-677`;
`vtorrent-node/src/block.rs:361-376`;
`vtorrent-script/src/standard.rs:268-294`;
`vtorrent-node/src/staking.rs:595-618`.

The P2CS script has a hot-key branch (`OP_IF <staking_hash> OP_CHECKSIG`) and a
cold-key branch (`OP_ELSE <locktime> OP_CLTV OP_DROP <spending_hash>
OP_CHECKSIG`). The R1 re-lock rule is enforced **only inside
`if tx.is_coinstake()`**. A normal `TxType::Standard` transaction spending the
P2CS UTXO can satisfy the hot branch and send the principal anywhere; the
generic script engine accepts it, and no consensus rule requires the cold
branch.

**Reproduction:** before the fix, probe
`hot_key_ordinary_transaction_steals_delegated_principal` built a valid PoS
block at height 2, appended a Standard transaction spending a 1,000 VTR P2CS
UTXO, signed it with only the hot key, and `chain.add_block` accepted it. The
probe now asserts rejection (`hot_key_ordinary_transaction_is_rejected`).

**Impact:** anyone who compromises the online staking key — the key the design
explicitly keeps hot — can drain every delegated cold-stake UTXO. This breaks
the central cold-staking security claim.

**Fix applied:** `p2cs_cold_spend_authorized` in
`vtorrent-node/src/chain/chain_reorg.rs` now requires a non-coinstake P2CS
spend to carry a valid cold-spending-key signature over the sighash and a
locktime at or above the P2CS locktime. Regressions:
`hot_key_cannot_spend_p2cs_as_a_standard_transaction` (rejected) and
`cold_key_can_spend_p2cs_as_a_standard_transaction` (accepted). This covers
raw P2CS UTXOs; the P2SH-wrapped path is still open (see C-2).

### H-1 — High (FIXED in `7c1e429`): a hot-key coinstake could burn delegated principal

**Where:** `vtorrent-node/src/block.rs:361-376`;
`vtorrent-node/src/chain/chain_reorg.rs:660-677`.

`p2cs_redirected_value` only sums value sent to scripts **other than** the
staked P2CS script. A coinstake that pays 1 satoshi back to the same P2CS
script and nothing else has `redirected == 0`, so R1 passes. `minted` is
computed with `saturating_sub`, so the destroyed principal is not detected.

**Reproduction:** before the fix, probe
`hot_key_coinstake_burns_delegated_principal` reduced the coinstake's P2CS
output to 1 satoshi, re-signed with the hot key, and the chain accepted the
block. The probe now asserts rejection (`hot_key_coinstake_burn_is_rejected`).

**Impact:** the hot key can destroy delegated funds and corrupt supply/stake
accounting. It cannot redirect them, but destruction is still a fund-loss and
griefing primitive.

**Fix applied:** the coinstake path now requires
`sum(P2CS outputs) >= staked.value` in addition to the no-redirect rule.
Regression: `coinstake_cannot_burn_p2cs_principal`.

### C-2 — Critical (theft path FIXED in `7c1e429`; staking support FIXED in
`80ccc52` via raw-script funding): the wallet-generated cold-stake address is
P2SH, but the consensus and staking paths only recognize the raw P2CS script

**Where:** `vtorrent-wallet/src/wallet.rs:94-120`;
`vtorrent-node/src/consensus.rs:203-216`;
`vtorrent-node/src/staking.rs:706-716`;
`vtorrent-node/src/block.rs:361-376`.

`generate_cold_stake_keys` derives a **P2SH** address from the P2CS redeem
script, but `is_stakeable_with` accepts only `P2PKH` or a raw `P2CS`
scriptPubKey. The staking engine's `is_p2cs_script` and the R1 rule likewise
classify the UTXO script, which is P2SH. Consequences:

- Funds sent to the generated cold-stake address are **not stakeable**.
- The R1 re-lock rule did not apply to a P2SH-wrapped P2CS spend.
- A hot key could present the redeem script and take the hot branch.

**Impact:** the documented cold-staking flow does not work, and funds sent to
the advertised address were not protected by the R1 rule. This was a fund-loss
path, not just a usability gap.

**Fix applied (defensive):** `p2cs_redeem_script` now extracts the redeem
script from a P2SH scriptSig, and the cold-spend check and R1 rule apply to
P2SH-wrapped P2CS spends. Regression:
`hot_key_cannot_spend_p2sh_wrapped_p2cs`.

**Resolved for the intended flow:** `fund_cold_stake` (RPC
`/api/v1/wallet/fund-cold-stake`) pays the **raw P2CS script**, which
`is_stakeable_with`, the staking engine, and R1 all recognize. The P2SH label
address remains non-stakeable and must not be used as a funding address; the
docs and the `ColdStakeResp` comment now say so. The UI must call
`fund_cold_stake` with the returned `script_pubkey`, not a generic send to the
P2SH label.

### H-2 — High (FIXED in `1b0da9a`): BTC SPV scanning failed on mainnet with the default
retention

**Where:** `vtorrent-btc/src/sync.rs:514-532`;
`vtorrent-btc/src/headers.rs:348-372`;
`vtorrent-daemon/src/config.rs:147` (default retention 100,000).

`authenticate_filter_headers` derives a checkpoint from `start_height` and
requests from `checkpoint + 1`. `hashes_from` returns only the retained range.
When the retained floor is above `checkpoint + 1` (the normal case once
retention prunes), the returned length is shorter than the requested range and
the scan fails with "Bitcoin main-chain header range is not contiguous". A
fresh mainnet wallet starts at the retained floor, so it cannot scan.

**Reproduction:** probe
`retained_scan_floor_can_lose_filter_checkpoint_prefix` sets retention 2 on a
7-header chain, starts at floor 4, and shows the requested range is not
available.

**Impact:** BTC SPV cannot discover funds on mainnet with the shipped default
retention. `--btc-birthday` does not fix history older than the floor: the scan
still clamps to `max(last_scanned, retained_floor)`, and the CLI help claiming
older funds are found is false.

**Fix applied:** `align_scan_start` moves the scan to the next checkpoint
boundary when the checkpoint is below the retained floor, so the filter-header
anchor is inside the retained range. Regression:
`scan_start_aligns_above_retained_floor`. The `--btc-birthday` help now states
that funds older than the retained window are not discoverable; a real
historical checkpoint/birthday path is still not implemented.

### H-3 — High (FIXED in `b7a50e9`): protocol-v3 compact-block `notfound` fallback was
unreadable

**Where:** `vtorrent-node/src/node/handler.rs:690-701` (sender uses
`encode_for_peer`, bincode for v3); `:2033-2060` (receiver uses
`serde_json::from_slice`); `vtorrent-p2p/src/message.rs:142-167`.

When a `blocktxn` response cannot fit the wire limit, the sender emits a
`notfound` in the peer's negotiated format. The receiver parses `notfound` as
JSON only. For v3 peers the bincode payload fails to parse, so the pending
compact block is not removed and no full-block fallback is requested.

**Reproduction:** probe `protocol_three_notfound_is_not_json` encodes an
`InvMsg` with `encode_for_peer(..., PROTOCOL_VERSION)` and shows
`serde_json::from_slice` fails.

**Impact:** compact-block relay stalls when a transaction set exceeds the
wire budget; the receiver waits for a response it cannot parse.

**Fix applied:** the `notfound` handler now uses
`decode_for_peer::<InvMsg>(&msg.payload, peer_version)`. Probe
`protocol_three_notfound_round_trips` covers the v3 round trip. Other direct
JSON decoders on network payloads still need an audit.

### M-1 — Medium (FIXED in `5ce91d2`): desktop persistence silently dropped blocks
on event lag

**Where:** `vtorrent-tauri/src/commands/node.rs:296` (ignores `Lagged`);
daemon equivalent reconciles at `vtorrent-daemon/src/main.rs:693-...`.

The Tauri event task persists `NewBlock`/`Reorg` from a bounded broadcast
channel. On `Lagged` it does nothing. The daemon, by contrast, reconciles the
store from the chain. A burst of transaction events can drop a `NewBlock`
before it is persisted.

**Reproduction:** probe
`desktop_event_channel_drops_block_under_transaction_event_pressure` fills the
channel with `TxUnconfirmed` events and shows the `NewBlock` is lost.

**Impact:** the desktop store diverges from the in-memory chain; the next
restart can fail replay or lose blocks.

**Fix applied:** on `Lagged`, the desktop rebuilds the block store from the
chain (with the store installed as the chain's body source). Compile-checked;
no desktop restart test yet.

### M-2 — Medium (FIXED in `5ce91d2`): desktop first run failed before creating the data
directory

**Where:** `vtorrent-tauri/src/commands/node.rs:93-97`; daemon creates the
directory at `vtorrent-daemon/src/main.rs:186`.

`BlockStore::open` is called before `create_dir_all`. On a fresh profile
`~/.vtorrent` does not exist, so redb cannot create `chain.db`.

**Reproduction:** probe `desktop_store_open_requires_parent_directory` shows
`BlockStore::open` fails when the parent is absent.

**Impact:** first-run desktop node startup fails.

**Fix applied:** `create_dir_all(&config.data_dir)` runs before
`BlockStore::open`, with a clear error on failure.

### M-3 — Medium (FIXED in `5ce91d2`): desktop BTC wallet always used Bitcoin mainnet

**Where:** `vtorrent-tauri/src/commands/node.rs:157`.

The wallet is constructed with `bitcoin::Network::Bitcoin` regardless of
`config.regtest`/testnet.

**Impact:** a testnet/regtest desktop session derives mainnet BTC addresses and
cannot discover testnet funds; a user could send real BTC to an address the
test session treats as its own.

**Fix applied:** the wallet network is selected from `config.regtest` /
`config.testnet` / mainnet.

### L-1 — Low: WebSocket API key is accepted in the query string

**Where:** `vtorrent-rpc/src/ws.rs:169-173`; public route at
`vtorrent-rpc/src/server.rs:222-226`.

Browsers cannot set headers on a WebSocket upgrade, so the key is passed as
`?api_key=...`. Query strings are commonly logged by proxies and tracing
layers.

**Impact:** credential leakage to logs or intermediaries.

**Fix:** use a short-lived one-time token, a subprotocol, or a cookie; redact
query strings in logs. At minimum document the exposure.

## Positive observations

- Wallet encryption uses a fresh random 32-byte salt and 96-bit nonce per
  encryption, Argon2id (64 MiB, t=3, p=4), ChaCha20-Poly1305, and zeroizing
  key material (`vtorrent-wallet/src/encryption.rs`). No nonce-reuse or
  hard-coded-key defect found in this review.
- RPC auth hashes both inputs before comparison and applies auth outside the
  rate limiter (`vtorrent-rpc/src/server.rs:25-57`, `:193-198`).
- The VTR and BTC HTLC scripts pin the preimage to 32 bytes and use CLTV on the
  refund branch; the swap policy orders BTC refund before VTR refund.

## Residual gaps not closed by this review

- No external review has been commissioned.
- The BTC SPV scan is not exercised at mainnet scale with retention enabled.
- The desktop restart, peer-status, BTC discovery, and two-node propagation
  paths have no runtime test.
- The `notfound`/`blocktxn` wire-format audit is incomplete; other direct JSON
  decoders may exist.
- The `--btc-birthday` behavior is not implemented for history below the
  retained floor.
- The P2SH label address is not stakeable; cold-stake funding must use the raw
  P2CS script via `fund_cold_stake`.
- The governance regression suite was red at the reviewed revision:
  `cold_stake_owner_votes_not_operator` failed with "voting stake did not exist
  at proposal creation" because its fixture created the UTXO and the proposal
  at the same height. This was pre-existing (confirmed by stashing the
  cold-stake changes). The fixture now uses height 0, and the suite passes
  (12/12); this is a fixture correction, not a consensus change.
- The cold-staking fixes need a re-review of the P2CS script and SPV verifier.

## Recommended order

1. Ensure the UI uses `fund_cold_stake` with the raw P2CS script and never
   advertises the P2SH label as a funding address.
2. Fix L-1 (WebSocket query token) and audit the remaining direct JSON
   decoders.
3. Add a desktop restart/BTC-discovery runtime test and a mainnet-scale BTC SPV
   retention test.
4. Re-run the probes and commission the external review on the fixed revision.
