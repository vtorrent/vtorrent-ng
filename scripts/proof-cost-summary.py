#!/usr/bin/env python3
"""Summarize the canonical_proof_request_cost experiment's JSON report."""

import json
import re
import statistics
import sys
from pathlib import Path


def summary(values):
    values = sorted(values)
    return {
        "median": statistics.median(values),
        "min": min(values),
        "max": max(values),
    }


def main():
    reports = json.loads(Path(sys.argv[1]).read_text())
    result = []
    for peers in sorted({row["peers"] for row in reports}):
        rows = [row for row in reports if row["peers"] == peers]
        timings = []
        for row in rows:
            for line in row["reconstruction"]:
                match = re.fullmatch(
                    r"[0-9a-f]{64} wait_us=(\d+) hold_us=(\d+) found=true", line
                )
                if match is None:
                    raise ValueError(f"Invalid reconstruction record: {line!r}")
                timings.append(tuple(map(int, match.groups())))
        group = {"peers": peers, "runs": len(rows), "utxos": rows[0]["utxos"]}
        for phase in ("cold", "warm"):
            group[phase] = {
                "elapsed_us": summary([r[phase]["elapsed_us"] for r in rows]),
                "rpc_p95_us": summary([r[phase]["rpc_info"]["p95_us"] for r in rows]),
                "rpc_max_us": max(r[phase]["rpc_info"]["max_us"] for r in rows),
                "proof_median_us": summary([r[phase]["proofs"]["median_us"] for r in rows]),
                "daemon_cpu_ticks": summary([r[phase]["daemon_cpu_ticks"] for r in rows]),
                "peak_rss_kib": summary([r[phase]["observed_peak_rss_kib"] for r in rows]),
                "proofs": sum(r[phase]["proofs"]["count"] for r in rows),
                "errors": sum(r[phase]["errors"] for r in rows),
            }
        group["reconstruction_wait_us"] = summary([t[0] for t in timings])
        group["reconstruction_hold_us"] = summary([t[1] for t in timings])
        result.append(group)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
