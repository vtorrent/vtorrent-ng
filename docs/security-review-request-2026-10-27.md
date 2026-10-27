# Independent security review request

This package requests an independent human security review. The prior internal
model review in `docs/2026-10-26_security-vtorrent-report.md` does not satisfy
the independence gate.

## Review target

Review the candidate `70ea7b9dfb3c65dcfb30cb03ec8c168c5d3cbffd` plus the code
deltas through `287a5054f7884b1c5671cd6c01f4119330f1e441`. The final code
commit is the review head.

| Commit | Full hash | Purpose |
|---|---|---|
| Candidate | `70ea7b9dfb3c65dcfb30cb03ec8c168c5d3cbffd` | Reviewed feature baseline |
| `1901bb9` | `1901bb9c458c5211306c7f79721e111298bd416a` | Two-peer filter-header agreement |
| `513efde` | `513efdecd072707c0060f09ac71cf37ed205583a` | Network-group diversity and failover |
| `c465d1b` | `c465d1b747d5ff7994d4a78fa795ae6bee6ef118` | Five-peer headroom and in-cycle retry |
| `6f617ce` | `6f617ce0dff5ee108b610d5c0b2f534ef3c5d9ef` | BTC SPV health metrics |
| `7ec3b2f` | `7ec3b2fa7c790a23e98277f54de7920f27028495` | Non-blocking metrics during scans |
| `2c381fa` | `2c381fa1f1bff1dd365056412456da708c3785eb` | Persistent compact-filter peer cache |
| `dbd2034` | `dbd20340860815dfb71a7caf6cff3124cf39ca26` | Stale-scan alert fix |
| `aeb103a` | `aeb103a782058960b5a103a455d0335f9f3f1962` | Four fresh-review BTC findings |
| `8639a7f` | `8639a7fc4a62c72420a34c4cd0d34e866c67a42f` | Restart lineage rescan |
| `9b9a4f7` | `9b9a4f73a00c2f816669c1ecb18c270c7df9b9d0` | External taker claim recognition |
| `c3b3847` | `c3b3847356b46befb3cab96a5ce280831c727ce6` | Hide restored outputs until rescan |
| `287a505` | `287a5054f7884b1c5671cd6c01f4119330f1e441` | Stuck-scan alert |

Docs-only commits in the delta are evidence, not implementation changes.

## Verification

```bash
git clone https://github.com/vtorrent/vtorrent-ng
cd vtorrent-ng
git checkout 287a5054f7884b1c5671cd6c01f4119330f1e441
git rev-parse HEAD
bash docs/security-review-2026-10-26/verify-fixes.sh
cargo test --workspace --all-features
```

The last full workspace run on `287a505` passed 48 groups, 1,118 tests, zero
failures, and eight ignored tests.

## Scope emphasis

- BTC SPV peer agreement, failover, peer cache, and filter-header trust.
- Wallet restart lineage and reorg reconciliation.
- Mainnet swap claim verification and settlement observation.
- RPC authorization on sensitive routes.
- The R1 cold-staking rule and wallet encryption, with explicit verdicts.

## Deliverables

- Findings with severity, reproduction, and a concrete fix.
- Explicit verdict on the R1 cold-staking rule and wallet encryption.
- A residual-risk section for anything accepted.

## Evidence

- `docs/security-review-brief.md` and `docs/security-review-handoff.md`.
- `docs/2026-10-26_security-vtorrent-report.md` and its local proofs.
- `docs/btc-spv-soak-plan.md` and `docs/soak-log.md` for probe and soak
  evidence, including the 60-cycle active swap soak.
- `docs/btc-spv-multinode-reorg-test.md` for real-Bitcoin Core tests,
  including the separate-node taker claim.

## Status

- The exact final-source `287a505` probe is running; its 24 h window ends
  2026-10-28T10:23:13Z.
- Production alert delivery and the fleet-level active soak remain open.
- Please confirm scope, timeline, and access needs before starting.