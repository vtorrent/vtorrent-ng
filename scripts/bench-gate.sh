#!/usr/bin/env bash
# Benchmark regression gate: fail if any consensus-hotpath benchmark's median
# regresses more than 25%. CI measures the previous and current revisions on
# the same runner; local runs can compare against the committed baseline.
#
# Baselines live in vtorrent-node/benches/baselines/<name>/estimates.json
# (criterion's `new/estimates.json` output, copied after a quiet-machine run).
#
# Usage: scripts/bench-gate.sh                       (compare vs committed baseline)
#        scripts/bench-gate.sh --save                (refresh committed baseline)
#        scripts/bench-gate.sh --compare-ref <ref>   (compare on the same machine)

set -euo pipefail

TOLERANCE="${BENCH_TOLERANCE:-1.25}" # 25% per mainnet-readiness §1
REPO_ROOT="$(git rev-parse --show-toplevel)"
TARGET_DIR="$REPO_ROOT/target"
CRITERION_DIR="$TARGET_DIR/criterion"
BASELINE_DIR="$REPO_ROOT/vtorrent-node/benches/baselines"
cd "$REPO_ROOT"

capture_results() {
    local destination="$1"
    mkdir -p "$destination"
    while IFS= read -r est; do
        local rel="${est#"$CRITERION_DIR"/}"
        local rel_path="${rel%/new/estimates.json}"
        mkdir -p "$destination/$rel_path"
        cp "$est" "$destination/$rel_path/estimates.json"
    done < <(find "$CRITERION_DIR" -path '*/new/estimates.json')
}

if [[ "${1:-}" == "--save" ]]; then
    echo "── Running benchmarks to capture new baseline ──"
    rm -rf "$CRITERION_DIR"
    CARGO_TARGET_DIR="$TARGET_DIR" cargo bench -p vtorrent-node --bench consensus_hotpath
    rm -rf "$BASELINE_DIR"
    capture_results "$BASELINE_DIR"
    echo "Baseline refreshed: $(find "$BASELINE_DIR" -name estimates.json | wc -l) benchmarks"
    exit 0
fi

if [[ "${1:-}" == "--compare-ref" ]]; then
    if [[ -z "${2:-}" ]]; then
        echo "--compare-ref requires a Git revision"
        exit 2
    fi

    TEMP_DIR="$(mktemp -d)"
    BASELINE_WORKTREE="$TEMP_DIR/worktree"
    MACHINE_BASELINE="$TEMP_DIR/baselines"
    cleanup() {
        git -C "$REPO_ROOT" worktree remove --force "$BASELINE_WORKTREE" >/dev/null 2>&1 || true
        rm -rf "$TEMP_DIR"
    }
    trap cleanup EXIT

    cargo generate-lockfile
    git -C "$REPO_ROOT" worktree add --detach "$BASELINE_WORKTREE" "$2"
    cp "$REPO_ROOT/Cargo.lock" "$BASELINE_WORKTREE/Cargo.lock"
    rm -rf "$CRITERION_DIR"
    echo "── Running baseline benchmarks from $2 ──"
    (
        cd "$BASELINE_WORKTREE"
        CARGO_TARGET_DIR="$TARGET_DIR" cargo bench -p vtorrent-node --bench consensus_hotpath
    )
    capture_results "$MACHINE_BASELINE"
    BASELINE_DIR="$MACHINE_BASELINE"
    rm -rf "$CRITERION_DIR"
else
    rm -rf "$CRITERION_DIR"
fi

echo "── Running benchmarks for gate comparison ──"
CARGO_TARGET_DIR="$TARGET_DIR" cargo bench -p vtorrent-node --bench consensus_hotpath

if [[ ! -d "$BASELINE_DIR" ]]; then
    echo "No committed baseline at $BASELINE_DIR — skipping gate (first run)."
    exit 0
fi

compare_results() {
python3 - "$BASELINE_DIR" "$CRITERION_DIR" "$TOLERANCE" <<'PYEOF'
import json
import sys
from pathlib import Path

baseline_dir, criterion_dir, tolerance = Path(sys.argv[1]), Path(sys.argv[2]), float(sys.argv[3])

def median(path):
    data = json.loads(Path(path).read_text())
    return data["median"]["point_estimate"]

regressions = []
missing = []
compared = 0
for est in sorted(baseline_dir.rglob("estimates.json")):
    rel = est.relative_to(baseline_dir)  # e.g. merkle_root/compute/100tx/estimates.json
    name = str(rel.parent)
    new = criterion_dir / name / "new" / "estimates.json"
    if not new.exists():
        print(f"MISSING    {name}: no fresh result")
        missing.append(name)
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
if missing:
    for name in missing:
        print(f"FAIL: {name} has no fresh benchmark result")
    sys.exit(2)
if regressions:
    for name, ratio in regressions:
        print(f"FAIL: {name} regressed {ratio:.2f}x (> {tolerance:.2f})")
    sys.exit(1)
print("PASS: no benchmark regressed beyond tolerance")
PYEOF
}

if compare_results; then
    exit 0
else
    comparison_status=$?
fi
if [[ "$comparison_status" -eq 2 ]]; then
    exit 1
fi

echo "── Re-running benchmarks to confirm detected regressions ──"
rm -rf "$CRITERION_DIR"
CARGO_TARGET_DIR="$TARGET_DIR" cargo bench -p vtorrent-node --bench consensus_hotpath
compare_results
