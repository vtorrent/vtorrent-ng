# Node Diagnostics & Health Check ("Doctor") — Design

Status: DRAFT (no consensus change; node + RPC + CLI/UI)
Scope: `vtorrent-node`, `vtorrent-store`, `vtorrent-rpc`, `vtorrent-cli`/UI.
Motivation: when a node misbehaves (won't sync, disagrees on the tip, a wallet
won't unlock, the store looks corrupt), an operator has only logs. There is no
single command that answers "is my node healthy, and if not, why?" This adds a
read-only **doctor** that runs a battery of consistency checks and reports
findings with severity and remediation — reusing the checks the node already
performs internally.

## 1. What exists (checks we can reuse)

- Store integrity: redb checksum verification at open
  (`verify_checksum_helper`), and the store self-heals corrupt tails at startup.
- Chain/store consistency: the event-bridge lag reconciliation
  (`main.rs` reconcile path) already detects a store that isn't a prefix of the
  chain.
- UTXO commitment: `recompute_utxo_root` vs the tip header's `utxo_root`.
- Wallet: decrypt + re-derive (the backup-verify path,
  `docs/wallet-backup-recovery-design.md`).
- Peers: `peer_manager`, ban manager, PEX.

## 2. Design

### 2.1 Checks (read-only)

| Check | What it verifies | Severity if fail |
|---|---|---|
| Store integrity | redb opens, checksums verify | Critical |
| Chain↔store prefix | store is a prefix of the in-memory chain | Critical |
| UTXO commitment | `recompute_utxo_root` == tip `utxo_root` | Critical |
| Replay parity | replaying N blocks reproduces the tip hash | Critical |
| Wallet decrypt | wallet file decrypts; addresses re-derive | Critical |
| Peer health | ≥1 peer, not banned, handshakes succeed | Warning |
| Sync progress | height advancing; not stalled | Warning |
| Disk space | free space for the store + wallet | Warning |
| Clock skew | system time vs median peer time | Warning |
| Config sanity | ports distinct, data dir writable, perms 0600 | Warning |

### 2.2 Report

- A structured report: `{ check, status: ok|warn|fail, detail, remediation }`,
  plus an overall verdict.
- **Read-only by default**; a `--fix` mode may perform *safe* remediations
  (e.g. re-run the store reconciliation, re-derive the UTXO root) but never
  destructive ones without explicit consent.
- Exit code non-zero on any `fail` (so it can gate ops/cron).

### 2.3 Surfaces

- **CLI**: `vtorrent-cli doctor [--json] [--fix]`.
- **RPC**: `GET /api/v1/diagnostics` (read-only).
- **UI**: a "Node health" panel on the Dashboard/Security page, with the
  findings and remediation links.

### 2.4 Continuous mode

- Optionally run the cheap checks periodically and surface failures via
  `docs/notifications-design.md` (e.g. "store diverged", "no peers for 10 min").

## 3. Adversarial review

- **R1 — read-only by default.** A diagnostic that mutates state can cause the
  outage it's meant to diagnose. `--fix` is opt-in and limited to safe,
  idempotent remediations.
- **R2 — don't duplicate the checks.** Reuse the node's own integrity/reconcile
  paths; a second implementation will drift and disagree. Call the same code.
- **R3 — cost.** Replaying blocks or re-deriving the UTXO root is expensive;
  bound it (last N blocks / a sampled check) and make the deep checks opt-in.
- **R4 — false alarms.** A transient one-height lag or a brief peer drop must not
  read as `fail`; use the same re-read/tolerance logic the soak script uses.
- **R5 — secrets.** The wallet check must not print keys; report only
  ok/fail + address count.
- **R6 — no consensus change.** Read-only diagnostics.

## 4. Test plan

- Each check passes on a healthy node; each fails (and reports remediation) on a
  deliberately corrupted fixture (bad store tail, mismatched UTXO root, wrong
  wallet passphrase).
- Transient lag/peer drop does not read as `fail`.
- `--fix` performs only the safe remediations and is idempotent.
- Exit code is non-zero on any `fail`.
- No secrets in the report.

## 5. Non-goals

- Not a monitoring/alerting stack (Prometheus/Grafana already cover metrics).
- Not automatic destructive repair.
- Not part of the current soak window.

## 6. References

- `vtorrent-store/src/store.rs` (checksum verify, self-heal).
- `vtorrent-daemon/src/main.rs` (event-bridge reconciliation).
- `vtorrent-node/src/chain.rs` `recompute_utxo_root`.
- `docs/wallet-backup-recovery-design.md` (wallet verify),
  `docs/notifications-design.md` (continuous alerts).
