# 2.0.0 release candidate: RC14

**SHA:** `b4c5c60218db5992098b9306bfa62be49c60a410` (tag `v2.0.0-rc14`)
**Image:** `vtorrent/node:rc14`, ID `sha256:7ff2a11fb8ffa92fa495cb95dfb695d5cf509affb0deae2afb237d1a523dc553`
**Daemon:** `ed862b9b6a16f0eb921595456a0489b7837cc9f72a9a3e73b640c41c2ebdcc88`
**Window:** `2026-11-28T01:19:36Z` to `2026-12-05T01:19:36Z` (`review-rc14`)
**Launch floor:** `2026-12-06T00:00:00Z`

RC14 closes the third review pass blockers:

- The remaining unbounded peer sends (version and command paths) now use the 10-second bound; the previous commit only covered verack and pong.
- A VTR refund records whether it was ever relayed. The latch is cleared before `btc_claim` only when the refund was never relayed and has no replacements, so a relayed refund that can still confirm is never discarded.
- Queued compact filters are capped at 64 MiB, not 1000 messages of up to 4 MiB.
- The launch floor moved to 2026-12-06 to leave margin after this window.
- The desktop reports version 2.0.0; the release lockfile matches.

## Accepted residuals

- BTC filter-header chains have no independent anchor; two colluding peers can hide a claim. This is inherent to the current BIP157 design and remains the top SPV risk.
- A BTC filter scan longer than 30 minutes can lose partial progress and restart from the same cursor.
- A fresh node can be eclipsed or stalled by attacker peers; it cannot be made to accept a wrong consensus chain.
- The proof service can block the node event loop; DEX gossip is unauthenticated and local orders are not broadcast; PEX `getaddr` is not egress-charged.
- Disk-full piece writes can loop; slow-loris piece ownership is not defeated.
- Restore refuses when a swap or cold-stake recovery directory exists, by design; the UI copy asks users to preserve those files.
- Restoring a pre-2FA backup can downgrade 2FA if the attacker can write the data directory and knows the backup passphrase; 2FA does not protect the backup.

## Evidence

- Workspace suite: **1,555 passed, 0 failed**; Clippy with `-D warnings` clean after the unused-import fix.
- Fleet: three nodes on the recorded image, matching tip at height 2, two staking, three Prometheus targets, zero restarts.
- Evidence directory: `/mnt/temp/vtorrent-ng-evidence/soak-rc14/`.
- Desktop: [RC14 run 46092519345](https://github.com/vtorrent/vtorrent-ng/actions/runs/46092519345) passed on all four platforms. Draft release 461263039 contains both DMGs, the NSIS installer, the deb, the AppImage, and `Cargo.lock`; all six files matched their recorded SHA-256 digests, the lockfile matched the tagged source, both DMGs and the NSIS fields report 2.0.0, the deb reports 2.0.0, and the AppImage executable shares the deb executable's ELF build ID.

RC12 and earlier candidates are retired. Independent human review remains waived for 2.0.0, per the [policy](release-candidate-rc4.md#review-policy-for-200). The final `v2.0.0` tag must resolve to `b4c5c60218db5992098b9306bfa62be49c60a410`, contain all five installers plus `Cargo.lock`, report 2.0.0 in every installer, and pass the checksum ancestry check.
