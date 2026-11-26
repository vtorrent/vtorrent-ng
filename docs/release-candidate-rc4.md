# 2.0.0 release candidate: RC4 (retired)

Review and build `4ce7d315e3d5cb03c1f2c7c40a555cb87fe2e79e` (tag `v2.0.0-rc4`). Do not substitute `main`. Relative to `be78829`, the only changes are the explicit desktop Rust target install and the Tauri bundle version `2.0.0`. The resumable BTC repair merged in `896f6e4` is excluded.

[RC4 CI](https://github.com/vtorrent/vtorrent-ng/actions/runs/45738941138) passed on all four desktop targets. The draft contains both DMGs, the Windows NSIS installer, Linux deb and AppImage, and the tagged lockfile. Both app plists, the NSIS version fields and the deb metadata report 2.0.0. The AppImage executable shares the deb executable's ELF build ID (indirect version evidence).

The no-cache build from the tagged source produced `vtorrent/node:4ce7d31`, image ID `sha256:a1d75b05bec0f22aa9f8d661ad7810bbd3392d7e2cd4c83962c21d208e64e35b`. Its daemon SHA-256 is `58462cc05ab1877264736734699d974d1c9e7396ea5ae988a626e3b504fede5f`, identical to the pass-15 daemon.

RC4 was retired by the security review; `v2.0.0-rc5` supersedes it. The `review-4ce7d31` 168-hour window runs from **2026-11-26T05:51:34Z to 2026-12-03T05:51:34Z**. All three nodes started on the recorded image with the same height-2 tip and zero restarts. Nodes 2 and 3 were staking. Prometheus had three healthy targets; BTC SPV was not yet synced. Evidence: `/mnt/temp/vtorrent-ng-evidence/soak-4ce7d31/`. The observer runs every six hours and the local freshness watchdog hourly. Keep the raw per-instance scrape history for final verification. No predecessor window counts. No independent human security review was performed.

Before publication, the checksum workflow requires the tagged commit to be an ancestor of `origin/main`. Verify with `git merge-base --is-ancestor v2.0.0-rc4 origin/main`; this was false at selection. Resolve ancestry without moving the approved tag or substituting a later commit. Do not publish to discover this failure.

## Review policy for 2.0.0

On 2026-11-26 the maintainer chose frontier AI review for the 2.0.0 mainnet candidate and waived the independent human reviewer requirement for this release only. The maintainer retains the final release decision. This does not change any later protocol activation policy.

GPT-6.2 Astra helped prepare RC4 and then performed the security review in `docs/2026-11-26_ai-security-review-4ce7d31.md`. That review found two Critical and four High defects and returned a hold verdict; see that record for the blocking list and required fixes. Before tagging, record the model, reviewed SHA, scope, tests, unresolved findings, and verdict for the security scope in `docs/security-review-request-2026-10-27.md`. Distinguish evidence verified on RC4 from evidence inherited from predecessors. An unresolved release-blocking finding keeps the release in draft. Include the AI review scope and absence of an independent human audit in the final release notes.
