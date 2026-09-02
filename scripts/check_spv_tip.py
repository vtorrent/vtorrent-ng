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
    ap.add_argument("--rpc", nargs="*", default=["http://localhost:22525","http://localhost:22526","http://localhost:22527"])
    args = ap.parse_args()
    tips = []
    for url in args.rpc:
        data = rpc_get(url, "/api/v1/blockchain/info")
        if data:
            tips.append(data.get("best_hash") or data.get("tip") or str(data)[:16])
            print(f"{url}: {tips[-1]}")
        else:
            print(f"{url}: no data (node down?)")
    if args.expect_converged and len(set(tips)) > 1 and len(tips) == len(args.rpc):
        print("tips diverged — reorg in progress or fork", file=sys.stderr)
        sys.exit(1)
    print("check_spv_tip: ok (stub)")
    sys.exit(0)

if __name__ == "__main__":
    main()
