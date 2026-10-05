# Human-Readable Names (VTR Name Service) — Design

Status: DRAFT (registry convention; no consensus change if OP_RETURN-based)
Scope: `vtorrent-node`/`vtorrent-rpc` (registry + resolver), `vtorrent-wallet`,
UI.
Motivation: addresses are base58 `V…` strings — unmemorable and error-prone to
share. A name service (`alice.vtr` → address) makes payments, torrent
publishers, and swap counterparties human-friendly. It reuses the same
OP_RETURN-registry pattern as torrent discovery, so it costs no state.

## 1. What exists

- Base58Check addresses (`V…`), `Address::parse` / `p2pkh_script_pubkey`.
- OP_RETURN data carriers (`vtorrent-script` `build_op_return`).
- No name resolution anywhere.

## 2. Design

### 2.1 Registration (on-chain, OP_RETURN)

- A **registration** is a tx with an OP_RETURN:
  ```
  "VTRN1" | name_len | name | address(21) | flags(1)
  ```
  - `name` is normalized (lowercase, ASCII/punycode), length-bounded, with a
    reserved-character policy.
  - `address` is the target (P2PKH today; could be P2CS/P2SH later).
- **First-come-first-served** by block order; a re-registration (update) must be
  signed by the **current owner** (the address that registered it), so names are
  transferable/updatable only by their owner.
- Cost = a tx fee → spam-resistant. With
  `docs/op-return-utxo-exclusion-design.md`, the registration output is pruned
  from the UTXO set → no state bloat.

### 2.2 Resolution

- A **resolver** scans OP_RETURN registrations (like the torrent indexer) and
  builds `name → (address, owner, height)`. Reuse the read-only indexer pattern
  (`docs/block-explorer-design.md`).
- **Reverse lookup** (address → primary name) is optional and opt-in.
- Resolution is **local** by default (the node's own view); a hosted resolver is
  an ops decision.

### 2.3 Wallet / UI

- **Send to a name**: the wallet resolves `alice.vtr` → address, shows the
  resolved address + the name's owner, and warns if the name was recently
  (re)registered (freshness/anti-hijack).
- **Register/update** a name from the wallet (with the privacy warning: the
  registration links the name to the funding address on-chain).
- **Display**: show names where known (address book, history, payment requests).

### 2.4 Anti-squatting / policy

- Names are scarce and first-come; squatting is inevitable. Options: a
  **registration fee** (burn or treasury), **auctions**, or **length-based
  pricing** (shorter = pricier). Recommend a **fee + length pricing** for v1;
  auctions are a follow-up.
- A **reserved list** (protocol names) to prevent confusion/phishing.

## 3. Adversarial review

- **R1 — name→address is a phishing surface.** A look-alike name can redirect
  payments. Show the resolved address prominently, warn on fresh registrations,
  and never hide the address behind the name. (Same class as the payment-URI
  warning, `docs/payment-requests-design.md` R2.)
- **R2 — ownership must be enforced.** Only the current owner can update a name,
  or anyone can hijack it. Bind updates to the registering address's signature.
- **R3 — normalization must be canonical.** Case, Unicode/punycode, and
  confusable characters must normalize to one canonical form, or two names
  collide/impersonate. Define and test the normalization.
- **R4 — no state bloat.** OP_RETURN registrations must be pruned (with the
  OP_RETURN exclusion); otherwise names bloat the UTXO set forever.
- **R5 — squatting.** First-come names will be squatted; a fee/length pricing
  mitigates but doesn't solve it. State it.
- **R6 — resolver trust.** A local resolver is trustless (scan the chain); a
  hosted one is a trust/ops decision. Default local.
- **R7 — no consensus change** for the OP_RETURN convention; making names
  *consensus-enforced* (e.g. rejecting conflicting registrations in-block) would
  be a change — keep it a convention + resolver.

## 4. Test plan

- Register → resolve; update by owner succeeds; update by non-owner rejected.
- Normalization: case/Unicode variants collapse to one name; confusables flagged.
- Freshness warning on a recently re-registered name.
- Registration output is pruned (with OP_RETURN exclusion); no UTXO growth.
- Send-to-name resolves and shows the address; look-alike warning fires.
- Reserved names rejected.

## 5. Non-goals

- Not a DNS replacement / not resolving external names.
- Not auctions (follow-up).
- Not part of the current soak window.

## 6. References

- `vtorrent-core` `Address::parse` / `p2pkh_script_pubkey`.
- `vtorrent-script` `build_op_return`; `docs/op-return-utxo-exclusion-design.md`.
- `docs/torrent-discovery-design.md` (same OP_RETURN-registry pattern),
  `docs/block-explorer-design.md` (read-only indexer),
  `docs/payment-requests-design.md` (phishing warning).
