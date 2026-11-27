# 2.0.0 release candidate: RC10

**SHA:** `fc7747ba01730d8e2bd8d64eb514658382f1fd43` (tag `v2.0.0-rc10`)
**Image:** `vtorrent/node:rc10`, ID `sha256:554917132538a2d3b0b1ce069af6206c1714381a3beea4baf39e9be7106f7ca3`
**Daemon:** `06e66325a7ed7901f638024637918c2017aecafff82009bad9635e7239f31b3e`
**Window:** `2026-11-27T16:40:52Z` to `2026-12-04T16:40:52Z` (`review-rc10`)
**Launch floor:** `2026-12-05T00:00:00Z`
**Review:** [RC4 review](2026-11-26_ai-security-review-4ce7d31.md) plus the RC5-RC10 scoped reviews and the second full pass.

RC10 closes the second review pass blockers:

- `LAUNCH_TIME` is now the future launch moment and the height-1 bootstrap block must meet it exactly, so nobody can pre-mine a mainnet chain before launch.
- Every peer send is bounded, so a non-reading peer cannot wedge a task and exhaust inbound slots.
- A VTR refund is refused after the maker has taken the BTC leg, a recorded refund that never reached the chain or mempool is cleared before `btc_claim`, and `OP_PUSHDATA1/2` preimages are recognized.
- Torrent metainfo paths are validated against traversal, duplicates, and file/directory collisions.
- App backups are wallet-only and restorable; recovery data is preserved separately.

## Accepted residuals

- Disk-full or persistent piece-write failure can re-download in a loop; no free-space circuit breaker.
- Slow-loris piece ownership can defeat the stall timeout; no endgame.
- Wallet SPV scan can briefly credit a block that leaves the active chain mid-scan.
- 2FA disable requires only a valid code, not the passphrase; restore then reuses the consumed code.
- No `stop_node` command; restore and wallet switching use the restart path.
- Lock/unlock leaves staking display state set until the node restarts.
- Live multi-node torrent transfer in the fleet needs a tracker; the E2Es cover the path.
- Installed desktop journeys on Windows and macOS are not exercised.

## Evidence

- Workspace suite: **1,555 passed, 0 failed**; formatting and workspace Clippy with `-D warnings` clean.
- Fleet: three nodes on the recorded image, matching tip at height 2, two staking, three Prometheus targets, zero restarts.
- Evidence directory: `/mnt/temp/vtorrent-ng-evidence/soak-rc10/`.
- Desktop: [RC10 run 46035465769](https://github.com/vtorrent/vtorrent-ng/actions/runs/46035465769) passed on all four platforms. Draft release 460968226 contains both DMGs, the NSIS installer, the deb, the AppImage, and `Cargo.lock`; all six files matched their recorded SHA-256 digests, the lockfile matched the tagged source, both DMGs and the NSIS fields report 2.0.0, the deb reports 2.0.0, and the AppImage executable shares the deb executable's ELF build ID.

RC9 and earlier candidates are retired. Independent human review remains waived for 2.0.0, per the [policy](release-candidate-rc4.md#review-policy-for-200). Publication requires the completed RC10 window, the final draft checks, and the same SHA for `v2.0.0`.
