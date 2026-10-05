# HD Account Discovery (Gap-Limit Scan) — Design

Status: DRAFT (no consensus change; wallet + RPC)
Scope: `vtorrent-wallet` (`hd.rs`, wallet), `vtorrent-rpc`.
Motivation: `HdAccount` (`hd.rs:16`) stores only the **mnemonic + word count** —
there is **no derivation index and no gap-limit scan**. So a restored HD wallet
cannot find the addresses it previously used (it doesn't know how far the chain
was derived), and balances appear empty. This is a correctness bug for the
restore path (`docs/wallet-backup-recovery-design.md`) and for watch-only xpub
import (`docs/watch-only-wallets-design.md`).

## 1. What exists

- `HdAccount { mnemonic, word_count, created_at }` (`hd.rs:16`) — no index, no
  address list, no scan.
- `Mnemonic::generate/from_phrase` (`:37,47`), `to_seed` (`:65`).
- Address derivation exists (`pubkey_to_vtorrent_address`); BIP84 derivation on
  the BTC side (`vtorrent-btc/src/keys.rs`).
- No gap-limit logic anywhere.

## 2. Design

### 2.1 Derivation scheme

- Define the VTR HD path (e.g. `m/44'/…'/0'/0/i` for receive, `/1/i` for change) —
  pick a coin type and document it (BIP44-style).
- Derive **receive** and **change** chains; store the **next unused index** per
  chain in `HdAccount` so new addresses are sequential.

### 2.2 Gap-limit scan (restore)

- On restore/import, **scan** the chain: derive addresses from index 0 upward on
  each chain, query the chain for any that have history/balance, and stop after
  **`GAP_LIMIT` (e.g. 20) consecutive unused** addresses.
- This finds all used addresses without knowing the count in advance — the
  standard BIP44 discovery.
- Record the highest used index so subsequent derivations continue from there.

### 2.3 Watch-only xpub

- The same scan applies to an **xpub** (public-only derivation): derive addresses
  and scan, so a watch-only wallet finds the balance
  (`docs/watch-only-wallets-design.md` R4).

### 2.4 Persistence

- `HdAccount` gains `receive_next: u32`, `change_next: u32`, and (optionally) a
  cached `used_indices` set. `#[serde(default)]` keeps old wallets valid (they
  start at 0 and scan on first use).

### 2.5 UX

- Show a **"scanning for addresses…"** progress during restore (it queries the
  chain per address); bound the scan (max indices) to avoid a runaway on a
  pathological gap.

## 3. Adversarial review

- **R1 — gap limit must be standard and configurable.** Too small misses used
  addresses (lost funds in the UI); too large wastes scan work. Default 20,
  document, allow override.
- **R2 — scan cost.** Querying the chain per derived address is O(gap) lookups;
  bound the total (e.g. stop at 10,000 indices) and show progress. Don't hang the
  UI.
- **R3 — change chain matters.** Scanning only receive addresses misses change
  outputs; scan both chains (receive + change).
- **R4 — index persistence.** If the next-index isn't persisted, addresses get
  reused (privacy) or skipped. Persist and advance atomically with address use.
- **R5 — old wallets.** `#[serde(default)]` so existing HD wallets load; they
  scan on first use to populate the index.
- **R6 — no consensus change.** Wallet/RPC only.

## 4. Test plan

- Restore a wallet with used addresses at indices 0, 5, 25 (with a gap): the scan
  finds all three; balances match.
- Gap limit: an address at index `GAP_LIMIT` after the last used is not found
  (documented); within the gap is found.
- Change addresses are discovered.
- Next-index persists across restart; no address reuse.
- xpub watch-only scan finds the balance.
- Old wallet files load and scan.
- No consensus/chain change.

## 5. Non-goals

- Not a full BIP44 account hierarchy (single account for v1).
- Not address-type migration (P2PKH only today).
- Not part of the current soak window.

## 6. References

- `vtorrent-wallet/src/hd.rs:16,37,47,65` `HdAccount`, mnemonic, seed.
- `vtorrent-wallet/src/tx_builder.rs` `pubkey_to_vtorrent_address`.
- `vtorrent-btc/src/keys.rs` (BIP84 derivation reference).
- `docs/wallet-backup-recovery-design.md`, `docs/watch-only-wallets-design.md`.
