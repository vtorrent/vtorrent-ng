# RC4 release decision, 2026-11-26

**Hold publication.** GPT-6.2 Astra performed this supplementary review after helping prepare RC4. It is not independent sign-off or a new audit of consensus or wallet code.

| Check | Result |
|---|---|
| Candidate identity | The remote `v2.0.0-rc4` tag and successful Actions run `45738941138` both resolve to `4ce7d315e3d5cb03c1f2c7c40a555cb87fe2e79e`. |
| Release inventory | GitHub release ID `458928488` is a draft. All five installers and `Cargo.lock` were uploaded. All six locally inspected files matched the SHA-256 digests on the draft; the lockfile also matched the tagged source. See the [RC4 record](release-candidate-rc4.md) for internal version checks and the AppImage evidence limit. Digest agreement is not a signature. |
| Tag ancestry | Failed against remote `main` at `d2f3e56`. A normal merge at `fc40d67` preserved the tag. Remote refs were checked after pushing; `git merge-base --is-ancestor v2.0.0-rc4 origin/main` now succeeds. The content delta from the merge's first parent was the desktop bundle version only. The release target remains RC4, not the merge commit. |
| Partial soak | At 07:31:59 UTC the first 6,025 seconds yielded 200, 201, and 200 raw `up` samples for candidate nodes 1–3. Zero down samples; longest gaps including window edges: 30.00, 30.00, and 30.01 seconds. All three containers were running the recorded image with zero restarts. These results measure scrape availability, not uninterrupted application correctness. |

## Remaining publication gates

- Finish the RC4 window on December 3 at 05:51:34 UTC. Check each expected instance for missing scrapes and outages, and reconcile chain progress, tip agreement, BTC sync and restarts with the operations log.
- Obtain independent human review. RC4 build success and this review do not close that gate.
- For the final draft, verify `isDraft == true`, resolve the final tag to the exact RC4 SHA, and check the inventory and digests of the **final tag's** assets. The runbook's current shell block checks names only. A release-triggered checksum job cannot prevent an incorrect publication. Resolve signing policy before publication.
