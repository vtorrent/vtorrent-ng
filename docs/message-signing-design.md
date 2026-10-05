# Message Signing & Proof of Ownership — Design

Status: DRAFT (no consensus change; wallet + RPC + UI)
Scope: `vtorrent-wallet`, `vtorrent-core` (recover), `vtorrent-rpc`, UI.
Motivation: there is no way to **prove you control an address** without moving
coins. Message signing (sign an arbitrary message with an address's key; anyone
can verify) is standard, and the primitives already exist — `secp256k1` has the
`recovery` feature and `sign_ecdsa_recoverable` is used in the DEX
(`dex.rs:282`). This enables: proving ownership for support/airdrops, signing
agreements/attestations, and verifying a counterparty in swaps/OTC.

## 1. What exists

- `secp256k1` with `recovery` (`Cargo.toml:34`); `sign_ecdsa_recoverable` used in
  `dex.rs:282`.
- `PrivateKey::from_wif` (`keys.rs`), address derivation
  (`pubkey_to_vtorrent_address`).
- No `sign_message` / `verify_message`.

## 2. Design

### 2.1 Sign

- `sign_message(wif, message) -> signature`:
  - Hash the message with a **domain-separated prefix** (e.g.
    `"\x18vTorrent Signed Message:\n" + len + message`) so a signed message can't
    be replayed as a transaction signature (the classic Bitcoin mistake).
  - `sign_ecdsa_recoverable` over the digest; output
    `base64(header || r || s)` (recovery id in the header), compressed-pubkey
    convention.
- The wallet signs with the address's key (never exposes the key).

### 2.2 Verify

- `verify_message(address, message, signature) -> bool`:
  - Recover the pubkey from the signature + digest, derive the address, compare.
  - Reject malformed signatures; be strict about the prefix and encoding.

### 2.3 Address types

- **P2PKH**: recover → pubkey → address; straightforward.
- **P2CS (cold staking)**: the *spending* key proves ownership (the staking key
  is hot and must not be treated as ownership). Document which key signs.
- **P2SH/multisig**: a message signature can't prove a multisig address (no single
  key); either skip or define an M-of-N message-signing scheme (larger).

### 2.4 Uses

- **Proof of ownership**: for support, airdrops, or verifying a counterparty.
- **Signed attestations**: e.g. a torrent publisher signing their announce, or a
  swap counterparty signing terms.
- **Login / auth**: sign a server nonce to authenticate (pairs with
  `docs/mobile-companion-design.md` pairing).

### 2.5 UI

- A **Sign message** panel (pick an address, enter a message, show the
  signature) and a **Verify** panel (paste address/message/signature → valid?).

## 3. Adversarial review

- **R1 — domain separation is mandatory.** Without a distinct prefix, a signed
  "message" could be replayed as a transaction signature (or vice versa). Use a
  vTorrent-specific prefix and test that a tx sighash can't be signed as a
  message.
- **R2 — verify must recover, not just check.** Verification must recover the
  pubkey and derive the address; checking a signature against an assumed pubkey
  is weaker and can be spoofed by a crafted key.
- **R3 — which key for P2CS.** The *spending* key proves ownership; the staking
  key must not. Document and enforce.
- **R4 — multisig can't single-sign.** Don't imply a multisig address can sign a
  message; define or exclude.
- **R5 — encoding strictness.** Base64/hex, recovery-id range, and length must be
  validated; a lenient parser invites malleability.
- **R6 — no consensus change.** Wallet/RPC/UI only.

## 4. Test plan

- Sign/verify round-trip for a P2PKH address; wrong address fails; tampered
  message fails.
- Domain separation: a tx sighash signed as a message does **not** verify as a
  valid tx signature (and vice versa).
- P2CS: the spending key verifies; the staking key does not.
- Malformed signatures (bad base64, bad recovery id, wrong length) rejected.
- No consensus/chain change.

## 5. Non-goals

- Not a multisig message-signing scheme (follow-up).
- Not on-chain (messages are off-chain).
- Not part of the current soak window.

## 6. References

- `Cargo.toml:34` `secp256k1` `recovery` feature.
- `vtorrent-rpc/src/handlers/dex.rs:282` `sign_ecdsa_recoverable` (existing use).
- `vtorrent-core/src/keys.rs` `PrivateKey::from_wif`;
  `vtorrent-wallet/src/tx_builder.rs` `pubkey_to_vtorrent_address`.
- `docs/cold-staking-p2cs-design.md` (spending vs staking key),
  `docs/mobile-companion-design.md` (auth).
