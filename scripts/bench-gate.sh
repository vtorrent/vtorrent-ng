#!/usr/bin/env bash
# Benchmark regression gate: fail if any consensus-hotpath benchmark's median
# regresses more than 25% against the committed baseline.
#
# Baselines live in vtorrent-node/benches/baselines/<name>/estimates.json
# (criterion's `new/estimates.json` output, copied after a quiet-machine run).
#
# Usage: scripts/bench-gate.sh          (CI: compare vs committed baseline)
#        scripts/bench-gate.sh --save   (maintainer: refresh committed baseline)

set -euo pipefail

TOLERANCE="${BENCH_TOLERANCE:-1.25}" # 25% per mainnet-readiness §1
CRITERION_DIR="target/criterion"
BASELINE_DIR="vtorrent-node/benches/baselines"

if [[ "${1:-}" == "--save" ]]; then
    echo "── Running benchmarks to capture new baseline ──"
    cargo bench -p vtorrent-node --bench consensus_hotpath
    rm -rf "$BASELINE_DIR"
    mkdir -p "$BASELINE_DIR"
    # Criterion nests by group/function/parameter: <group>/<fn>/<param>/new/
    while IFS= read -r est; do
        rel="${est#"$CRITERION_DIR"/}"          # e.g. merkle_root/compute/100tx/new/estimates.json
        rel_path="${rel%/new/estimates.json}"   # e.g. merkle_root/compute/100tx
        mkdir -p "$BASELINE_DIR/$rel_path"
        cp "$est" "$BASELINE_DIR/$rel_path/estimates.json"
    done < <(find "$CRITERION_DIR" -path '*/new/estimates.json')
    echo "Baseline refreshed: $(find "$BASELINE_DIR" -name estimates.json | wc -l) benchmarks"
    exit 0
fi

echo "── Running benchmarks for gate comparison ──"
cargo bench -p vtorrent-node --bench consensus_hotpath

if [[ ! -d "$BASELINE_DIR" ]]; then
    echo "No committed baseline at $BASELINE_DIR — skipping gate (first run)."
    exit 0
fi

python3 - "$BASELINE_DIR" "$CRITERION_DIR" "$TOLERANCE" <<'PYEOF'
import json
import sys
from pathlib import Path

baseline_dir, criterion_dir, tolerance = Path(sys.argv[1]), Path(sys.argv[2]), float(sys.argv[3])

def median(path):
    data = json.loads(Path(path).read_text())
    return data["median"]["point_estimate"]

regressions = []
compared = 0
for est in sorted(baseline_dir.rglob("estimates.json")):
    rel = est.relative_to(baseline_dir)  # e.g. merkle_root/compute/100tx/estimates.json
    name = str(rel.parent)
    new = criterion_dir / name / "new" / "estimates.json"
    if not new.exists():
        print(f"SKIP {name}: no fresh result")
        continue
    base_ns = median(est)
    now_ns = median(new)
    ratio = now_ns / base_ns if base_ns > 0 else 1.0
    compared += 1
    status = "OK" if ratio <= tolerance else "REGRESSION"
    print(f"{status:10s} {name}: baseline {base_ns:.1f}ns -> {now_ns:.1f}ns ({ratio:.2f}x)")
    if ratio > tolerance:
        regressions.append((name, ratio))

print(f"\nCompared {compared} benchmarks (tolerance {tolerance:.2f}x)")
if regressions:
    for name, ratio in regressions:
        print(f"FAIL: {name} regressed {ratio:.2f}x (> {tolerance:.2f})")
    sys.exit(1)
print("PASS: no benchmark regressed beyond tolerance")
PYEOF
