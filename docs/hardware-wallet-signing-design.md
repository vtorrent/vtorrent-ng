# Hardware-Wallet Signing — Design

Status: DRAFT (no consensus change; wallet + RPC + UI + device transport)
Scope: `vtorrent-wallet` (signer abstraction), `vtorrent-rpc`/`vtorrent-tauri`
(flow), a device transport (platform-specific).
Motivation: sign with keys that never touch the host — the spending key for
cold staking, and cold storage generally. Pairs with
`docs/cold-staking-p2cs-design.md` and `docs/watch-only-wallets-design.md`.

## 1. Problem

Signing is in-process: `tx_builder.rs::sign_input` (`:129`) takes a raw secret
and calls secp256k1 locally; `sign_with_wif` (`:208`) and `send_vtr` assume the
key is in the wallet. There is **no PSBT and no device path**, so a user cannot
sign with a hardware wallet, and a cold-staking spending key must live on the
host to withdraw.

## 2. Design

### 2.1 A `Signer` abstraction

Introduce a trait so the wallet can delegate signing without knowing the key:

```rust
pub trait Signer: Send + Sync {
    /// Sign one input's sighash, returning a DER sig + sighash byte.
    fn sign_sighash(
        &self,
        input_index: usize,
        sighash: &[u8; 32],
        subscript: &[u8],
        derivation: Option<&DerivationPath>,
    ) -> Result<Vec<u8>>;
}
```

- `LocalSigner` wraps the current in-process path (no behaviour change).
- `DeviceSigner` forwards to a hardware device.
- `tx_builder` takes `&dyn Signer` instead of a WIF; `sign_with_wif` becomes
  `LocalSigner`.

### 2.2 PSBT-lite

A serialisable partially-signed transaction (the device needs more than a raw
sighash to display and verify):

```rust
pub struct PartiallySignedTx {
    pub tx: Transaction,                 // unsigned (empty scriptSigs)
    pub inputs: Vec<PsbtInput>,          // prevout script/value, derivation, sighash type
    pub outputs: Vec<PsbtOutput>,        // address, amount (for on-device display)
}
```

Flow: build unsigned tx → `PartiallySignedTx` → device signs each input →
assemble scriptSigs → broadcast. The device shows destination + amount before
signing (anti-blind-signing).

### 2.3 Transport

- Device-specific (USB/HID, or a bridge like Ledger's). **Feature-gated** and
  kept out of the core crates; the wallet only sees `DeviceSigner`.
- Version/negotiate the app protocol; never send key material to the host.

## 3. Synergy

- **Cold staking**: the spending key lives on the device; the hot node stakes
  with the staking key; withdrawals are signed on-device.
- **Watch-only**: the host tracks balances (watch-only) and builds unsigned txs;
  the device signs.

## 4. Adversarial review

- **R1 — anti-blind-signing.** The device must display and the user must confirm
  destination + amount; otherwise a compromised host can have the device sign a
  different tx. This is the core security property.
- **R2 — sighash parity.** The device's sighash for each script type (P2PKH,
  P2CS, HTLC) must match `Transaction::sighash` exactly. Test parity against
  `LocalSigner` for every supported type.
- **R3 — no key material on the host.** The transport carries unsigned txs and
  signatures only; the app must never request or log a seed/xpriv.
- **R4 — transport trust.** USB/HID is host-mediated; treat the device as the
  trust anchor and the host as untrusted. Validate what the device returns
  (pubkey matches the expected script, sig verifies) before assembling.
- **R5 — scope.** Large and platform-specific. Start with P2PKH + P2CS; defer
  multisig/HTLC. Do not block cold staking on it — cold staking works with a
  software spending key first.

## 5. Test plan

- `Signer` trait with a mock device: build → sign → assemble → verify.
- Sighash parity: `DeviceSigner` (mock) == `LocalSigner` for P2PKH and P2CS.
- Reject a device signature that doesn't verify against the expected script.
- PSBT round-trip (serialise/deserialise) preserves inputs/outputs.
- No key material crosses the `Signer` boundary (compile-time: the trait has no
  key accessor).

## 6. Non-goals

- Not a specific vendor integration (Ledger/Trezor) in the core crates.
- Not changing the transaction format or consensus.
- Not part of the current soak window.

## 7. References

- `vtorrent-wallet/src/tx_builder.rs:129` `sign_input`, `:208` `sign_with_wif`,
  `:375` `sign_custom_transaction`.
- `vtorrent-rpc/src/handlers/wallet.rs:343` `send_vtr`.
- `docs/cold-staking-p2cs-design.md`, `docs/watch-only-wallets-design.md`.
