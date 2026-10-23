# Security review handoff

Use this checklist when sending the review package. The scope, threat model,
and deliverables are in `docs/security-review-brief.md`.

## Package

| Item | Value |
| --- | --- |
| Pinned commit | `86aae3a8fdc505f58742c54a1c9f99a97eca5efc` |
| Brief | Latest `docs/security-review-brief.md` from `main`; it pins this commit |
| Toolchain | Rust 1.90.0 (`rust-toolchain.toml`) |
| Dependency lock | Archive the `Cargo.lock` used for the build; it is gitignored |
| Fleet image | Core soak: `vtorrent/node:batch-e7691fb`; release build: rebuild from `86aae3a` |
| Soak window | 2026-10-21T02:47:54Z to 2026-10-28T02:47:54Z |

`86aae3a` is `bab2456` plus the Playwright browser E2E setup, the live
cold-stake browser test, the Vite dev-origin CORS allowance, the Tauri path
capability fix, the durable cold-stake registry persistence fix, the desktop
restart/persistence parity fixes, the wallet-path confinement, import
overwrite guard, version bump, IPC error surfacing, and staking ownership
warnings, and the live desktop journey, restart, and BTC discovery evidence.
The frontier-model review's ten findings and three follow-up concerns are addressed across `61f4be4`, `cb765ce`, `f272e64`, `3975693`, `6f82b63`, `07a6881`, `326ce27`, `39da204`, `afe5974`, and `2287026`; verified 2FA enrollment, the torrent control availability, and the reviewed send quote are in `86aae3a`. The external review should verify them.

Verify the commit resolves before sending:

```bash
git clone https://github.com/vtorrent/vtorrent-ng
git -C vtorrent-ng checkout 86aae3a8fdc505f58742c54a1c9f99a97eca5efc
git -C vtorrent-ng rev-parse HEAD
```

The source tree at the pinned commit contains the implementation and evidence.
The brief, this checklist, and the release notes are supplied from `main` so
the reviewer sees the corrected references; they do not change the
implementation.

## Evidence to include

- `docs/security-review-brief.md` and this checklist.
- `docs/security-review-internal-2026-10-09.md` and the fix-status documents.
- `docs/governance-step3-self-review-2026-10-11.md` and
  `docs/governance-preactivation-validation-2026-10-12.md`.
- `docs/governance-reorg-load-test.md`, `docs/governance-pos-fork-proof-test.md`,
  and `docs/governance-interrupted-reorg-test.md`.
- `docs/governance-proof-request-cost.md`,
  `docs/governance-proof-snapshot-reconstruction.md`,
  `docs/governance-proof-reconstruction-budget.md`,
  `docs/governance-proof-retries.md`, and
  `docs/governance-proof-quota-recovery.md`.
- `docs/btc-spv-multinode-reorg-test.md`.
- `scripts/verify-genesis.py` and the recorded verifier output.
- The soak observations for the window, once complete.

## Verification commands

Run these at the pinned commit:

```bash
cargo test --workspace --all-features --release
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 scripts/verify-genesis.py
```

The real-bitcoind tests are ignored by default and need the
`vtr-btc-regtest` container with block filters enabled:

```bash
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery independent_daemons_agree_on_btc_spv_tip_and_reorg \
    -- --ignored --nocapture
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery multi_node_htlc_swap_settles_across_real_btc_spv \
    -- --ignored --nocapture
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery btc_spv_drops_a_reorged_out_funding_output \
    -- --ignored --nocapture
```

The proof quota recovery test also needs the container and takes about three
minutes:

```bash
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery daemon_proof_quota_expires_and_automatic_retry_completes \
    -- --ignored --nocapture
```

## Limitations to disclose

- Governance is inactive; activation is unscheduled.
- SPV PoS header acceptance remains fail-closed.
- The full swap test uses one VTR daemon; separate maker and taker VTR nodes
  remain open.
- BTC SPV has not been soaked at mainnet-scale header counts.
- A real-bitcoind reorged-out claim test is not covered; the settlement-state
  downgrade is unit-tested.
- Cold proof reconstruction is unavailable beyond the retained undo window.
  The retry queue is in memory and resets on restart.
- BTC wallet discovery defaults to the retained header floor. `--btc-birthday`
  sets an explicit creation height for older funds.
- The proof admission budget bounds request counts, not CPU milliseconds.

## Pre-send checks

- [ ] Commit hash resolves on `origin`.
- [ ] Every document referenced by the brief exists at the pinned commit.
- [ ] The full release-mode workspace suite passed at `ff6f472`; the changed
      crate suites and Clippy passed at `669ee36`, which adds the review
      remediation fixes. The BTC birthday flag, desktop commands, desktop
      health fields, shared BTC sync loop, desktop chain persistence, and the
      cold-stake/BTC/compact-block/desktop hardening compile and pass Clippy;
      the full workspace suite passed at the pinned commit.
- [ ] The independent genesis verifier passes.
- [ ] The evidence index and the limitations above are included.
- [ ] Contact, timeline, and confidentiality terms are agreed.
- [ ] The reviewer can build without private credentials or fleet access.

## Cover note template

```text
Subject: vTorrent 2.0.0 security review request

Hello <name>,

We are requesting an independent security review of vTorrent 2.0.0 before
mainnet activation. The scope and deliverables are in the attached brief.

Pinned source: 86aae3a8fdc505f58742c54a1c9f99a97eca5efc
Repository: https://github.com/vtorrent/vtorrent-ng

The review focuses on wallet custody, consensus and governance, the stake-proof
service, RPC auth, and the atomic-swap HTLC flow. The package includes the
internal reviews, adversarial test evidence, and reproduction commands.

Please confirm availability, the proposed timeline, and any access or
confidentiality requirements. We will keep the pinned commit unchanged during
the engagement and record any later implementation hash explicitly.

Regards,
<name>
```
