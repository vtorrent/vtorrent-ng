# Backlog Review — Design Coverage, Gaps, Priorities

Status: review of the design backlog in `docs/` (see `docs/roadmap.md`).
Purpose: assess what the 40+ designs cover, find the **gaps**, flag the **bugs**
they surfaced, and prioritize.

## 1. Coverage (well-covered areas)

- **Consensus/chain:** op-return exclusion, cold staking (P2CS), governance,
  state rent, multi-asset swaps, network-upgrade, fast-sync, light-client.
- **Security/custody:** watch-only, hardware signing, multisig, backup/recovery,
  passphrase rotation, message signing, HD discovery, tx preview.
- **Wallet/UX:** earnings view, notifications, swap UX, payment requests,
  wallet organization, export/statements, staking efficiency/pools, mobile.
- **Torrent economy:** incentive verification, reputation, micropayments,
  discovery, creator, streaming, seeding policy, peer encryption, tracker server.
- **DEX/privacy/ops:** order book, privacy, name service, fee market, diagnostics,
  reindex/rescan, explorer, webhooks.

## 2. Gaps (not covered by any design)

| Gap | Why it matters | Effort |
|---|---|---|
| **i18n / localization** | UI is English-only (only `toLocaleString` for numbers); a global product needs translation + RTL + locale-aware formatting. | Medium |
| **Auto-update & code signing** | Tauri desktop has **no updater and no signing/notarization**; users can't get security fixes, and unsigned builds are a supply-chain risk. | Medium |
| **Threat model & test vectors** | No written threat model or cross-implementation test vectors; consensus/script correctness relies on the reference impl alone. | Medium |
| **Accessibility (a11y)** | Only ~12 `aria`/`role` usages; keyboard/screen-reader support is thin for a wallet. | Medium |
| **Observability/alerting runbook** | Metrics exist (`/metrics`), but no alert rules / SLOs beyond the soak scripts. | Low |
| **Mobile push / native** | Covered only as a PWA sketch; push + secure-enclave signing are open. | High |

**Already covered (do not re-add):** fuzzing exists (`fuzz/` — script engine, P2P
codec, tx deser, BTC PSBT); RPC rate limiting + metrics exist; Tor/I2P transport
exists; `ut_metadata` (BEP-9) exists.

## 3. Bugs the designs surfaced

**Verified (fix now):**

3. **WIF in the browser** — `TradePage.tsx` took a raw `takerWif` and passed it
   to `vtrClaim`. **Real, active — FIXED** in `a050bd1`: the handler signs with
   the unlocked wallet key (passphrase + optional OTP); the UI sends a
   passphrase. A non-empty `taker_wif` remains an explicit CLI/test override.
4. **`prefer_onion` silently falls back to clearnet** — `transport.rs` dials
   clearnet when the Tor connect fails. (`privacy-design.md` R2.)
   **Real, active — FIXED** in `33dfe3e` (`strict_onion` refuses the fallback).
5. **Torrent incentives are unverifiable** — self-reported bytes drive real
   on-chain payments. (`incentive-verification-design.md`.) **Real, active.**

**Latent (verify before "fixing" — the RPC wallet is single-key today):**

1. **Incomplete history** — `get_transactions` (`handlers/wallet.rs:594`) queries
   only the change address. **Latent**: the RPC wallet is single-key
   (`HotWalletData { wif }`), so it's currently correct; it becomes a bug the
   moment multi-address/HD is wired. Fix alongside `wallet-organization-design.md`.
2. **HD restore can't find addresses** — `HdAccount` (`hd.rs:16`) has no
   derivation index / gap-limit scan. **Latent**: HD is not wired to the RPC
   wallet yet; becomes a bug when it is. Fix with `hd-discovery-design.md`.

**Lesson:** verify "bugs" against the code before asserting them — two of the
five were latent, not active.

## 4. Prioritization

- **P0 — correctness/security bugs:** the five above (1–5). Small, high-impact.
- **P0 — consensus batch (post-soak):** `network-upgrade` → `op-return` +
  `cold-staking` (one fresh genesis) → `governance` → `state-rent`.
- **P1 — quick wins (no consensus):** earnings view, notifications, wallet
  organization, payment requests, tx preview, diagnostics, HD discovery.
- **P2 — infrastructure:** fast-sync, light-client, explorer, webhooks,
  reindex/rescan, tracker server, peer encryption.
- **P3 — larger:** multi-asset swaps, mobile native, staking pools, name service,
  privacy (Dandelion++), i18n, auto-update/signing, threat model, a11y.

## 5. Risks / over-scope

- **42 designs is a backlog, not a plan.** Shipping them all is years of work;
  the roadmap's "first three" is the actionable slice. Beware designing more
  than building.
- **Consensus changes are the expensive ones.** Batch the fresh-genesis cluster
  into **one** activation; each extra activation is a fork + soak.
- **Some designs overlap** (e.g. notifications vs webhooks; explorer vs
  diagnostics) — merge implementation, keep the docs separate.
- **Verify before building:** several designs rest on assumptions (P2SH spend
  path enabled, CLTV semantics, `ut_metadata` correctness) that must be confirmed
  in code first.

## 6. Recommended next actions

1. **Fix the P0 bugs** (1–5) — small, and they block the related designs.
2. **Ship the first three** from `roadmap.md` §9.
3. **Add the missing docs** for the gaps: a **threat model**, **i18n**, and
   **auto-update/signing** design (the three most load-bearing gaps).
4. **Then** the consensus batch, with `network-upgrade` first.

## 7. References

- `docs/roadmap.md` (index + sequencing).
- The individual design docs referenced above.
- `fuzz/` (existing fuzzing), `vtorrent-rpc/src/metrics.rs`, `ratelimit.rs`.
