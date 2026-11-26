# 2.0.0 release candidate: RC5

**SHA:** `daf5a81b55ce5ed963b2be67490deaa59fdba480` (tag `v2.0.0-rc5`)
**Image:** `vtorrent/node:rc5`, ID `sha256:ce23bdabc8d4f5019cd4ee6693e73c76fa2c0b2bba242712be0947b1ddad2608`
**Daemon:** `f0c24865b548ac52236f7fff434b3c9c03241134096fa167b40bb4bf629e3d99`
**Window:** `2026-11-26T23:53:51Z` to `2026-12-03T23:53:51Z` (`review-rc5`)
**Review:** [RC4 security review](2026-11-26_ai-security-review-4ce7d31.md#remediation-preview), frontier AI, scoped re-review of the fix diff pending.

RC5 closes the RC4 release blockers: the unauthenticated handshake panic, the pre-launch timestamp warp, the preimage-selector miss, the blocking peer send, the ignored VTR claim conflict, and the refund-over-claim race. It is built from RC4 plus `daf5a81`; the resumable BTC repair on `main` remains excluded.

Relative to `v2.0.0-rc4`, `git diff --stat` shows seven files and no dependency or manifest change. The only intentional consensus change is the launch-time floor: production blocks must be dated at or after `LAUNCH_TIME - 2h`, with `LAUNCH_TIME = 2026-11-26T00:00:00Z`. Regtest fixtures bypass the floor through `allow_pow`. That constant must be no later than the actual launch; rebuild if launch slips materially.

## Evidence

- Workspace suite on the fix commit: **1,558 passed, 0 failed**; formatting and workspace Clippy with `-D warnings` clean.
- Desktop: RC5 tag CI run builds all four platforms; the draft is verified before publication.
- Fleet: three nodes on the recorded image, matching tip at height 2, two staking, Prometheus three targets, zero restarts.
- Evidence directory: `/mnt/temp/vtorrent-ng-evidence/soak-rc5/`.

RC4 is retired by the security review and its window does not count. No predecessor window counts for RC5. Independent human review remains waived for 2.0.0, per the [RC4 policy](release-candidate-rc4.md#review-policy-for-200-rc4); the frontier AI review must pass on this SHA before tagging `v2.0.0`.

Before publication, verify the final tag resolves to `daf5a81`, the draft contains all five installers plus `Cargo.lock`, each installer reports `2.0.0`, and the checksum workflow's ancestry check passes.
