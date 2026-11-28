# 2.0.0 release candidate: RC11

**SHA:** `704d8ca20d8a742606d7a5898ae04e23e2cb2cb5` (tag `v2.0.0-rc11`)
**Daemon:** `06e66325a7ed7901f638024637918c2017aecafff82009bad9635e7239f31b3e` (byte-identical to RC10)
**Soak:** `review-rc10`, `2026-11-27T16:40:52Z` to `2026-12-04T16:40:52Z` (daemon evidence carries under the byte-identical-component clause)
**Launch floor:** `2026-12-05T00:00:00Z`
**Review:** [security review](2026-11-26_ai-security-review-4ce7d31.md) and the RC5-RC10 scoped reviews.

RC11 changes only desktop copy: the backup screen claimed a backup includes BTC state, swap recovery, and the cold-stake registry, while the backup is now wallet-only. The create, restore, and welcome text now say what is included and that recovery data must be preserved separately.

`vtorrent/node:rc11` was rebuilt from this SHA and its daemon SHA-256 is identical to RC10's, so the running `review-rc10` window remains valid for the daemon. The RC11 desktop build is the evidence for the UI delta. No consensus, wallet, RPC, torrent, or swap code changed.

## Evidence

- UI lint and **45 tests** pass; workspace suite and Clippy were clean on the parent commit and no Rust file changed here.
- Desktop: the RC11 tag CI run builds all four platforms; verify its draft before publication.
- Docs pushed with this record.

The final `v2.0.0` tag must resolve to `704d8ca20d8a742606d7a5898ae04e23e2cb2cb5`, the draft must contain all five installers plus `Cargo.lock`, each installer must report 2.0.0, and the checksum workflow's ancestry check must pass. Independent human review remains waived for 2.0.0, per the [policy](release-candidate-rc4.md#review-policy-for-200).
