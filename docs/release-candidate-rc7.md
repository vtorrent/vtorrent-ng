# 2.0.0 release candidate: RC7

**SHA:** `2efd07c4727fb46be2aeb18f675c995c49cd38b2` (tag `v2.0.0-rc7`)
**Image:** `vtorrent/node:rc7`, ID `sha256:5f1039f6573b1c4c59055d597dedc457e9dd05cb58b0f3cbc33ff05b1761e874`
**Daemon:** `bfa0dd1a7b93ed6bd1ded36f13e0d02c14567dc47c39683acbca2ae778790cad`
**Window:** `2026-11-27T01:20:33Z` to `2026-12-04T01:20:33Z` (`review-rc7`)
**Review:** [RC4 review](2026-11-26_ai-security-review-4ce7d31.md) with the scoped re-reviews.

RC7 closes the RC6 residual: if a VTR refund cannot be admitted locally, the recorded refund is rolled back so it cannot block the maker's BTC claim. It is RC6 plus one fix commit. All other RC5/RC6 fixes are unchanged.

Relative to `v2.0.0-rc6`, the diff is one handler file plus the recorded test state. No manifest or dependency change.

## Evidence

- Workspace suite: **1,559 passed, 0 failed**; formatting and workspace Clippy with `-D warnings` clean.
- Fleet: three nodes on the recorded image, matching tip at height 2, two staking, three Prometheus targets, zero restarts.
- Evidence directory: `/mnt/temp/vtorrent-ng-evidence/soak-rc7/`.
- Desktop: the RC7 tag CI run builds all four platforms; verify the draft inventory before publication.

RC6 and earlier candidates are retired and do not count. Independent human review remains waived for 2.0.0, per the [policy](release-candidate-rc4.md#review-policy-for-200). No further Medium or Low review findings will be fixed before launch unless they are release-blocking; they are recorded in the review record. Before publication, verify the final tag resolves to `2efd07c4727fb46be2aeb18f675c995c49cd38b2`, the draft has all five installers plus `Cargo.lock`, each installer reports `2.0.0`, and the checksum workflow's ancestry check passes.
