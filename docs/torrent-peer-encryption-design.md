# Torrent Peer Encryption (MSE/PE) — Design

Status: DRAFT (no consensus change; torrent peer wire)
Scope: `vtorrent-torrent` (peer wire, engine), config/UI.
Motivation: the torrent handshake is **plaintext** (`peer_wire.rs:1-60`,
`"BitTorrent protocol"`), so traffic is trivially identifiable — ISPs and
network observers can **throttle, fingerprint, or block** it, and peers are
linkable. Message Stream Encryption / Protocol Encryption (MSE/PE) is the
standard fix and is table stakes for a modern torrent client.

## 1. What exists

- Plaintext handshake + wire (`peer_wire.rs`).
- `sha1` + `rand` deps; no crypto for the handshake.
- `vtorrent-onion` for transport anonymity (Tor/I2P) — orthogonal (MSE hides
  *what* the traffic is; onion hides *who*).

## 2. Design

### 2.1 MSE/PE handshake

- Implement the MSE/PE handshake (BitTorrent's de-facto standard):
  - Diffie-Hellman (768-bit MODP group) key exchange with a random `SKEY`.
  - RC4 stream cipher (two directions) with a **discard** of the first N bytes
    (1024) to avoid RC4's weak keystream prefix.
  - `HASH('req1', S)`, `HASH('req2', SKEY) xor HASH('req3', S)`, `HASH('req2', SKEY)`
    for the info-hash obfuscation, and `VC` verification constant.
- After the handshake, the normal BitTorrent wire runs **inside** the RC4 stream.
- **Info-hash obfuscation**: the `req2`/`req3` construction hides the info-hash
  from a passive observer (so a sniffer can't tell *which* torrent).

### 2.2 Negotiation

- Advertise MSE support in the handshake (the reserved bits / the MSE handshake
  itself); **prefer encrypted**, fall back to plaintext for peers that don't
  support it (configurable: `prefer` / `require` / `disable`).
- `require` mode refuses plaintext peers (stronger, fewer peers).

### 2.3 Crypto choices

- MSE/PE specifies **RC4** (legacy but interoperable). RC4 is weak, but the
  threat model here is **traffic classification/throttling**, not confidentiality
  against a powerful adversary — and interoperability requires RC4. Document the
  limitation honestly.
- Use a vetted DH implementation; never roll the group math by hand.

### 2.4 Config / UI

- A setting: **Encryption: prefer / require / off**, default **prefer**.
- Show the encryption status per peer in the UI (encrypted vs plaintext).

## 3. Adversarial review

- **R1 — RC4 is weak; be honest.** MSE/PE's RC4 is not strong confidentiality; its
  purpose is defeating naive DPI/throttling. Don't market it as "secure." For
  real anonymity, use `docs/privacy-design.md` (Tor/I2P).
- **R2 — the DH must be correct.** A botched DH (bad group, weak exponent,
  missing discard) breaks interoperability or security. Use a vetted
  implementation and test against a reference client.
- **R3 — the discard is mandatory.** Skipping the RC4 keystream discard
  (1024 bytes) leaks the weak prefix and can break the handshake. Test.
- **R4 — info-hash obfuscation.** The `req2`/`req3` construction must be exact, or
  the handshake fails or leaks the info-hash. Test against a known vector.
- **R5 — fallback policy.** `prefer` falls back to plaintext (defeats the point
  against an active blocker); `require` is stronger but reduces peers. Make the
  trade-off explicit (same class as `docs/privacy-design.md` R2).
- **R6 — no consensus change.** Torrent peer wire only.

## 4. Test plan

- MSE handshake against a reference implementation (or a known test vector):
  key exchange, `VC`, info-hash obfuscation, RC4 discard.
- Post-handshake wire round-trips inside the RC4 stream.
- `prefer` falls back to plaintext; `require` refuses plaintext peers.
- A passive observer can't read the info-hash (obfuscation test).
- No consensus/chain change.

## 5. Non-goals

- Not a replacement for Tor/I2P anonymity (orthogonal).
- Not a new cipher (MSE/PE uses RC4 for interop).
- Not part of the current soak window.

## 6. References

- `vtorrent-torrent/src/peer_wire.rs:1-60` plaintext handshake/wire.
- `vtorrent-torrent/Cargo.toml` (sha1/rand; no MSE crypto yet).
- `docs/privacy-design.md` (transport anonymity, orthogonal).
