# 2.0.0 release candidate: RC12

**SHA:** `1df8e5d44102ee008591afa1fd5dd5e02a3e70d3` (tag `v2.0.0-rc12`)
**Daemon:** `06e66325a7ed7901f638024637918c2017aecafff82009bad9635e7239f31b3e` (byte-identical to RC10 and RC11)
**Soak:** `review-rc10`, `2026-11-27T16:40:52Z` to `2026-12-04T16:40:52Z` (daemon evidence carries under the byte-identical-component clause)
**Launch floor:** `2026-12-05T00:00:00Z`

RC12 hardens desktop flows only:

- Disabling 2FA now requires the wallet passphrase in addition to a current code; a wrong passphrase leaves 2FA enabled.
- Restore no longer reuses the TOTP code consumed by the restore to unlock the restored wallet. It returns to the welcome screen for a fresh code.
- The cold-stake screen now says the button copies the spending-key path and that the file itself must be copied offline with a file manager.

No daemon, consensus, wallet-crate, RPC, torrent, or swap code changed. `vtorrent/node:rc12` rebuilt from this SHA has the same daemon digest as RC10, so the running window remains valid.

## Evidence

- Tauri suite **33 passed**, including a wrong-passphrase disable regression; UI lint and **45 tests** pass.
- Desktop: the RC12 tag CI run builds all four platforms; verify its draft before publication.

The final `v2.0.0` tag must resolve to `1df8e5d44102ee008591afa1fd5dd5e02a3e70d3`, the draft must contain all five installers plus `Cargo.lock`, each installer must report 2.0.0, and the checksum ancestry check must pass. Independent human review remains waived for 2.0.0, per the [policy](release-candidate-rc4.md#review-policy-for-200).

## Review coverage note

The third review pass was performed as a maintainer self-review because the external AI review agents hit a provider usage limit. It covered the RC10 fix diff and desktop flows. SPV sync, DEX, governance, and overlay did not receive an independent pass in this round; earlier passes and the documented residuals in the RC10 record remain the evidence for those areas.
