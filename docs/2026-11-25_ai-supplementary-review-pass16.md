# Pass 16 — candidate recovery and release evidence

Candidate: `be78829`; desktop matrix: `f85cddd`; window: `review-be78829`.
Disposition: keep soaking; hold public release. This is a supplementary AI review, not independent sign-off. Scope: desktop BTC repair, launch instructions, checksum publication and soak continuity. Static findings below; no wallet fault injection or release rehearsal was run.

| Priority | Finding and consequence | Closure evidence |
|---|---|---|
| High — launch procedure | `docs/mainnet-launch-runbook.md:8–12` selects pass 15, but lines 42–52, 61–68 and 125–126 direct operators to review, rebuild, launch or verify pass 13. Following the numbered procedure could select a retired image. | Point all executable instructions to one approved SHA and image. Resolve the exact-release-commit requirement in `docs/mainnet-readiness.md:190–196` versus the desktop CI-only child before tagging. Have a second operator identify the artifacts from the runbook alone. |
| Medium — wallet recovery | `vtorrent-tauri/src/commands/node.rs:130–143` creates mainnet state and then regtest state. If interrupted after the first write, `repair_btc_state` in `vtorrent-tauri/src/commands/wallet.rs` refuses the retry because one file exists. This can leave regtest unavailable through the supported repair path; it does not imply mainnet funds were lost. | Design a resumable repair that can distinguish interrupted initialization from prior wallet history. Verify interrupted repair can finish without changing the first file; unknown existing state must still be refused. Until then, preserve partial state for assisted recovery. |
| Medium — release verification | `.github/workflows/release-checksums.yml:8–10,40–54` runs after publication and checks that *some* assets exist, not that the promised platform set is complete. `.github/workflows/build.yml:313–321` uploads `Cargo.lock` as an asset, so a nonempty manifest alone does not establish that any installer is present. | Before making the draft public, compare the asset inventory to the four-platform build, record digests against the approved tag, and verify the required signing policy. A missing platform should stop publication even when other assets exist. |
| Medium — soak evidence | `scripts/soak-observe-candidate.sh:90–122` evaluates `up` at five-minute steps and applies the coverage threshold across all series. This can miss a short scrape failure or concentrate missing data on one node. | At sign-off, check raw `up` samples for each expected instance and environment, including window edges. Record down samples, longest gap, start times and image identity. Document the acceptable per-node gap in advance. |

## Direct continuity check (partial window only)

Queried local Prometheus at `2026-11-25T07:15:52Z` using an instant range-vector query for `up{job="vtorrent-daemon",environment="review-be78829"}` spanning from `01:19:30Z`. The candidate scrape interval is 30 seconds. Counts differ by one because the targets need not scrape at the same instant.

| Instance | Raw samples | Values other than 1 | Largest gap including window edges |
|---|---:|---:|---:|
| `candidate1:22525` | 712 | 0 | 30 s |
| `candidate2:22525` | 713 | 0 | 30 s |
| `candidate3:22525` | 713 | 0 | 30 s |

Docker inspection showed all three containers using image `sha256:fa0856a025bba41c5a9d54140c3471fb67d732a9259e603d2fb069a853815f99`, started before the window with restart counts zero. These observations support scrape availability so far, not continuous application correctness or completion of 168 hours. The independent review and production restart decision remain open. If a wallet fix is selected for release, evaluate it as a new candidate under the freeze policy.
