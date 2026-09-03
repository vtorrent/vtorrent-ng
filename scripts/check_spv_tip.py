#!/usr/bin/env python3
"""Check SPV tip convergence across 3 nodes — phase C of spv-reorg-soak."""
import argparse, json, sys, urllib.request

def rpc_get(url, path):
    try:
        with urllib.request.urlopen(f"{url}{path}", timeout=5) as r:
            return json.loads(r.read())
    except Exception as e:
        print(f"rpc {url}{path} failed: {e}", file=sys.stderr)
        return None

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--expect-converged", action="store_true")
    ap.add_argument("--rpc", nargs="*", default=["http://localhost:22625","http://localhost:22627","http://localhost:22629"])
    args = ap.parse_args()
    tips = []
    unavailable = []
    for url in args.rpc:
        data = rpc_get(url, "/api/v1/info")
        if data:
            tip = data.get("best_block_hash")
            if not isinstance(tip, str) or len(tip) != 64:
                print(f"{url}: malformed best_block_hash", file=sys.stderr)
                unavailable.append(url)
                continue
            tips.append(tip)
            print(f"{url}: {tips[-1]}")
        else:
            print(f"{url}: no data (node down?)")
            unavailable.append(url)
    if unavailable:
        print(f"{len(unavailable)} node(s) unavailable", file=sys.stderr)
        sys.exit(2)
    if args.expect_converged and len(set(tips)) != 1:
        print("tips diverged — reorg in progress or fork", file=sys.stderr)
        sys.exit(1)
    print("check_spv_tip: ok")
    sys.exit(0)

if __name__ == "__main__":
    main()
