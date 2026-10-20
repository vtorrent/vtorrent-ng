#!/usr/bin/env python3
"""Run the real soak observer against isolated fake curl/docker evidence."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def main():
    root = Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix="vtr-soak-review-", dir="/tmp/opencode") as directory:
        directory = Path(directory)
        curl = directory / "curl"
        curl.write_text('''#!/usr/bin/env python3
import json, os, sys
url = next(arg for arg in sys.argv[1:] if arg.startswith("http"))
if "query_range" in url:
    if os.environ["AUDIT_PROM_CASE"] == "invalid-json":
        print("not JSON")
        sys.exit(0)
    body = {"status": "success", "data": {"result": []}}
elif url.endswith("/info"):
    body = {"block_height": 11, "best_block_hash": "0" * 64, "connections": 2, "syncing": False, "mempool_size": 0}
elif url.endswith("/staking/status"):
    body = {"enabled": True, "eligible_utxos": 4, "blocks_staked": 11}
elif url.endswith("/btc/status"):
    body = {"initialized": False, "best_height": 0, "synced": False}
else:
    raise RuntimeError(url)
print(json.dumps(body, separators=(",", ":")))
''')
        docker = directory / "docker"
        docker.write_text('''#!/usr/bin/env python3
import sys, time
operation = sys.argv[1]
if operation == "stats":
    print("100MiB / 1GiB")
elif operation == "inspect":
    print(0)
elif operation == "logs":
    now = int(time.time())
    print(f"Block accepted height=10 hash=abc timestamp={now - 120}")
    print(f"Block accepted height=11 hash=def timestamp={now - 59}")
else:
    raise RuntimeError(operation)
''')
        curl.chmod(0o700)
        docker.chmod(0o700)
        false_green = False
        for case in ("empty-result", "invalid-json"):
            env = dict(os.environ, PATH=f"{directory}:{os.environ['PATH']}", AUDIT_PROM_CASE=case)
            result = subprocess.run(
                ["bash", str(root / "scripts/soak-observe.sh"), str(int(time.time()) - 600)],
                env=env, text=True, capture_output=True, timeout=20,
            )
            print(json.dumps({"case": case, "exit": result.returncode,
                "false_green": result.returncode == 0,
                "unavailable_reported": "Prometheus unavailable" in result.stdout}))
            false_green |= result.returncode == 0
        return 1 if false_green else 0


if __name__ == "__main__":
    raise SystemExit(main())
