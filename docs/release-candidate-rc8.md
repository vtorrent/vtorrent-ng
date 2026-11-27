# 2.0.0 release candidate: RC8

**SHA:** `1667ab8df9c31289979098f64edea37738ff5495` (tag `v2.0.0-rc8`)
**Image:** `vtorrent/node:rc8`, ID `sha256:02fff5d6613a9781b56b51d5fabcfffbfcc476b913892b3d6c32683913006137`
**Daemon:** `fce2339b5599d07c54322684a4537c7c8a9833f88da5cab651af307f6c4a9a89`
**Window:** `2026-11-27T01:52:23Z` to `2026-12-04T01:52:23Z` (`review-rc8`)
**Review:** [RC4 review](2026-11-26_ai-security-review-4ce7d31.md) with the scoped re-reviews.

RC8 completes the refund rollback: the recovery record previously refused to drop a prepared refund, so clearing an unadmitted refund could not be persisted. A dedicated `persist_failed_refund_clear` path allows exactly the `Some -> None` refund transition after failed local admission, and only that transition. RC7 is retired.

Relative to `v2.0.0-rc7`, the diff touches two files in `vtorrent-rpc` and no manifest.

## Evidence

- Workspace suite: **1,559 passed, 0 failed**; formatting and workspace Clippy with `-D warnings` clean.
- Fleet: three nodes on the recorded image, matching tip at height 2, two staking, three Prometheus targets, zero restarts.
- Evidence directory: `/mnt/temp/vtorrent-ng-evidence/soak-rc8/`.
- Desktop: the RC8 tag CI run builds all four platforms; verify the draft inventory before publication.

RC7 and earlier candidates are retired and do not count. Independent human review remains waived for 2.0.0, per the [policy](release-candidate-rc4.md#review-policy-for-200). No further Medium or Low findings will be fixed before launch unless release-blocking. Before publication, verify the final tag resolves to `1667ab8df9c31289979098f64edea37738ff5495`, the draft has all five installers plus `Cargo.lock`, each installer reports `2.0.0`, and the checksum workflow's ancestry check passes.
