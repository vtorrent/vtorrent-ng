# Watch-Only Wallets — Design

Status: DRAFT (no consensus change; wallet + RPC + UI)
Scope: `vtorrent-wallet` (key model), `vtorrent-rpc` (import/query), `vtorrent-ui`.
Motivation: monitor balances without holding keys — for cold-staking setups
(the hot node watches the cold wallet), for an accountant/observer, and for
importing an external address. Pairs directly with
`docs/cold-staking-p2cs-design.md`.

## 1. Problem

`WalletKeyEntry` (`wallet.rs:25`) requires a `wif` (`Zeroizing<String>`, not
optional), so every tracked address is spendable. There is no way to watch an
address you don't control, and no way to run a node that tracks a cold wallet's
balance without its keys.

## 2. Design

### 2.1 Key model

Add a separate list rather than making `wif` optional — keeps the invariant that
`WalletData.keys` are spendable:

```rust
pub struct WatchOnlyEntry {
    pub address: String,
    pub label: Option<String>,
    /// Optional extended public key this address was derived from (HD watch-only).
    pub xpub: Option<String>,
    pub created_at: u64,
    /// Cached balance in satoshis (updated by the node sync layer).
    pub balance: u64,
}
```

`WalletData` gains `watch_only: Vec<WatchOnlyEntry>` (`#[serde(default)]` for
back-compat).

### 2.2 Import

- **Single address**: import a `V…` address (and optionally a label).
- **HD xpub**: import an extended public key; derive receive/change addresses
  from it (no seed). Reuse the BIP32 derivation in `hd.rs`/`keys.rs` with the
  public-only path. This is what lets a hot node watch a cold HD wallet.
- Validation: reject anything that isn't a valid vTorrent address or a
  well-formed xpub; never accept a WIF here (that's the normal import path).

### 2.3 Balance / history

- The node already resolves balances and UTXOs **by address** (the UTXO set is
  keyed by scriptPubKey), so watch-only addresses need no new chain logic —
  the sync layer sums them like any other address.
- `get_addresses` (`handlers/wallet.rs:146`) returns watch-only entries too,
  flagged `watch_only: true`; `get_balance` includes them (or reports them
  separately — decide, §4).

### 2.4 Signing / sending

- Watch-only addresses are **never** signable. `wallet/send` must reject a
  source that is watch-only with a clear error; the UI disables Send for them.
- This is the security property: importing an address can never grant spend
  authority.

## 3. Cold-staking synergy

- The **hot staking node** imports the cold wallet's P2CS address (and/or its
  xpub) as watch-only, plus the **staking key** for block production. It can
  produce blocks and *see* the balance but cannot move funds.
- The **cold wallet** (offline) holds the spending key and can withdraw.

## 4. Open questions

- **Balance inclusion**: should watch-only balances be part of the headline
  wallet balance, or a separate "Watched" section? Mixing them can mislead
  ("I have X" when X isn't spendable). Recommend a **separate** total.
- **xpub gap limit**: standard 20-address gap; decide and document.
- **Encryption**: watch-only entries need no passphrase, but the wallet file is
  encrypted as a whole — confirm they load while locked (they should, since
  there's no secret).
- **Persistence/migration**: `#[serde(default)]` keeps old wallet files valid.

## 5. Adversarial review

- **R1 — watch-only must never sign.** The whole point. Enforce at the wallet
  API (no key → no sign) *and* reject in `wallet/send`; test an attempt to spend
  from a watch-only address.
- **R2 — don't leak the seed via xpub.** An xpub is public by design, but the
  import path must not accept a seed/mnemonic and silently derive keys. Keep the
  two import paths distinct.
- **R3 — balance confusion.** Separate watched vs spendable totals (R4 above),
  or the UI will overstate available funds.
- **R4 — address reuse / gap limit.** HD watch-only must scan the gap limit or
  balances will silently miss addresses.
- **R5 — no consensus surface.** Read-only tracking; no chain-format change.

## 6. Test plan

- Import address and xpub; balances resolve; UTXOs listed.
- `wallet/send` from a watch-only source is rejected.
- Old wallet files (no `watch_only`) load unchanged.
- Locked wallet still shows watch-only balances.
- xpub derivation matches the corresponding seed-derived addresses.

## 7. Non-goals

- Not hardware-wallet signing (separate item; watch-only is its prerequisite for
  showing balances).
- Not a block explorer.
- Not part of the current soak window.

## 8. References

- `vtorrent-wallet/src/wallet.rs:25` `WalletKeyEntry`, `:58` `WalletData`.
- `vtorrent-wallet/src/hd.rs` (BIP32/39), `keys.rs` (derivation).
- `vtorrent-rpc/src/handlers/wallet.rs:146` `get_addresses`; `wallet/send`.
- `docs/cold-staking-p2cs-design.md` (hot/cold key split).
