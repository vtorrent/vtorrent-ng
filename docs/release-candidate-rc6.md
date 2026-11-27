# 2.0.0 release candidate: RC6

**SHA:** `a335a385c6be1ed10f11f171ae563b8ce4e95825` (tag `v2.0.0-rc6`)
**Image:** `vtorrent/node:rc6`, ID `sha256:c9b4bfc8567841f0fe61a5984a575a97597c98876af5e02b72918170bd59869c`
**Daemon:** `aaf45a203f736e200a5bdf74a1d61cdb89236f25838c03ccf5b030a4e160c55f`
**Window:** `2026-11-27T00:46:13Z` to `2026-12-04T00:46:13Z` (`review-rc6`)
**Review:** [RC4 review](2026-11-26_ai-security-review-4ce7d31.md) plus the scoped fix re-reviews.

RC6 closes the last open High finding from the RC4 review: a VTR refund is now admitted locally before it is relayed, and a refund is refused while a claim is pending in the mempool or already confirmed. It is RC5 plus one fix commit; the resumable BTC repair on `main` remains excluded.

Relative to `v2.0.0-rc5`, the diff touches three files (`vtorrent-rpc/src/handlers/swap.rs`, `vtorrent-rpc/src/swap_reconciliation.rs`, `vtorrent-rpc/src/swap_recovery.rs`) and adds a pending-claim predicate with a unit test. No manifest or dependency change.

## Evidence

- Workspace suite: **1,559 passed, 0 failed**; formatting and workspace Clippy with `-D warnings` clean.
- Fleet: three nodes on the recorded image, matching tip at height 2, two staking, three Prometheus targets, zero restarts.
- Evidence directory: `/mnt/temp/vtorrent-ng-evidence/soak-rc6/`.
- Desktop: the RC6 tag CI run builds all four platforms; verify the draft inventory before publication.

RC5 is retired by the H4 finding; neither RC4 nor RC5 counts. Independent human review remains waived for 2.0.0, per the [policy](release-candidate-rc4.md#review-policy-for-200-rc4). The frontier AI review must pass on this SHA before tagging `v2.0.0`. Before publication, verify the final tag resolves to `a335a385c6be1ed10f11f171ae563b8ce4e95825`, the draft has all five installers plus `Cargo.lock`, each installer reports `2.0.0`, and the checksum workflow's ancestry check passes.
