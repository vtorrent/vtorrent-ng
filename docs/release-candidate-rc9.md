# 2.0.0 release candidate: RC9

**SHA:** `aa2cf21d044f74135b86ac09d33c934b7c1c4ae5` (tag `v2.0.0-rc9`)
**Image:** `vtorrent/node:rc9`, ID `sha256:d9e10763db6e6dd132545a311ebcf6137145cef9ec31a2c1e3d32ac8072e9272`
**Daemon:** `c849b087455d1648c2724bc9b2314541e2f41ae5c185dba618f8b1663e289506`
**Window:** `2026-11-27T12:34:57Z` to `2026-12-04T12:34:57Z` (`review-rc9`)
**Review:** [RC4 review](2026-11-26_ai-security-review-4ce7d31.md) with the scoped re-reviews.

RC9 aligns displayed torrent credit with the settlement rate. Settlement already paid 0.5 VTR/GB, while the earned display and the seeding constant used 1 VTR/GB, so the UI promised double what a peer could receive. Payout behavior is unchanged; only the displayed credit and the payment trigger use the settlement rate now. RC8 is retired.

Relative to `v2.0.0-rc8`, the diff is `vtorrent-torrent/src/incentive.rs` plus one documentation note. No manifest or dependency change.

## Functional evidence

- Torrent seeder E2E on the RC9 binary: **passed in 2.10 s**.
- Torrent incentive E2E on the RC9 binary: **passed in 26.20 s**, payment **585,937 sats**, restart did not repay. The payment is unchanged from RC8, confirming the pricing fix did not alter settlement.
- Real-bitcoind HTLC swap test on the RC8 code: **passed in 23.64 s** with real BTC funding, claim, and VTR claim transactions. The RC9 delta does not touch swap code.
- Desktop browser: Playwright smoke **passed**; the cold-stake browser journey is skipped because it requires live app IPC. Installed-app journeys on Windows and macOS are not exercised.
- Live multi-node torrent transfer in the soak fleet is not exercised: the fleet has no tracker or DHT, so it proves listener operation only. The two E2Es above cover the transfer path.

## Window evidence

- Workspace suite: **1,559 passed, 0 failed**; formatting and workspace Clippy with `-D warnings` clean.
- Fleet: three nodes on the recorded image, matching tip at height 2, two staking, three Prometheus targets, zero restarts.
- Evidence directory: `/mnt/temp/vtorrent-ng-evidence/soak-rc9/`.
- Desktop: [RC9 run 45999682956](https://github.com/vtorrent/vtorrent-ng/actions/runs/45999682956) passed on all four platforms. Draft release 460736329 contains both DMGs, the NSIS installer, the deb, the AppImage, and `Cargo.lock`; all six files matched their recorded SHA-256 digests, the lockfile matched the tagged source, both DMGs and the NSIS fields report 2.0.0, the deb reports 2.0.0, and the AppImage executable shares the deb executable's ELF build ID.

RC8 and earlier candidates are retired and do not count. Independent human review remains waived for 2.0.0, per the [policy](release-candidate-rc4.md#review-policy-for-200). No further Medium or Low findings will be fixed before launch unless release-blocking. Before publication, verify the final tag resolves to `aa2cf21d044f74135b86ac09d33c934b7c1c4ae5`, the draft has all five installers plus `Cargo.lock`, each installer reports `2.0.0`, and the checksum workflow's ancestry check passes.
