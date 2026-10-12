#!/usr/bin/env python3
"""Compare retained snapshots with compact governance undo journals."""

import argparse
import json
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/examples/governance_memory")
    parser.add_argument("--proposals", type=int, default=32)
    parser.add_argument("--voters", type=int, default=1000)
    parser.add_argument("--depth", type=int, default=100)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--stake-transfers", action="store_true")
    parser.add_argument("--votes-per-block", type=int, default=0)
    parser.add_argument("--compact-only", action="store_true")
    parser.add_argument("--budget-mib", type=int, default=256)
    parser.add_argument("--limit-mib", type=int, default=2048)
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    samples = {"compact_undo": []} if args.compact_only else {"full_snapshots": [], "compact_undo": []}
    for _ in range(args.runs):
        for kind in samples:
            command = [args.binary, "--proposals", str(args.proposals), "--voters", str(args.voters),
                       "--depth", str(args.depth), "--budget-mib", str(args.budget_mib),
                       "--limit-mib", str(args.limit_mib), "--votes-per-block", str(args.votes_per_block)]
            if args.stake_transfers:
                command.append("--stake-transfers")
            if kind == "full_snapshots":
                command.append("--full-snapshots")
            result = subprocess.run(command, text=True, capture_output=True)
            if result.returncode:
                raise SystemExit(f"probe failed ({kind}): {result.stderr}")
            data = json.loads(result.stdout)
            if (not data["completed"] or data["journal_kind"] != kind
                    or data["released_requested_bytes"] != 0):
                raise SystemExit(f"incomplete or unreleased workload: {data}")
            samples[kind].append(data)
    metrics = ["seed_requested_bytes", "steady_requested_bytes", "reorg_requested_bytes",
               "peak_requested_bytes", "steady_rss_kib", "reorg_rss_kib", "observed_peak_rss_kib"]
    summary = {"proposals": args.proposals, "voters_per_proposal": args.voters,
               "depth": args.depth, "stake_transfers": args.stake_transfers,
               "votes_per_block": args.votes_per_block,
               "runs_per_kind": args.runs, "completed_runs": args.runs * len(samples)}
    for kind, rows in samples.items():
        summary[kind] = {metric: {"median": statistics.median(row[metric] for row in rows),
                                 "min": min(row[metric] for row in rows),
                                 "max": max(row[metric] for row in rows)} for metric in metrics}
        summary[kind]["all_fit_component_budget"] = all(row["governance_fits_budget"] for row in rows)
        summary[kind]["all_released"] = True
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
