#!/usr/bin/env python3
"""Build an isolated baseline daemon with this workspace's dependency lock."""

import argparse
import shutil
import subprocess
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkout", type=Path)
    parser.add_argument("install_root", type=Path)
    args = parser.parse_args()
    workspace = Path(__file__).resolve().parent.parent
    checkout = args.checkout.resolve()
    if checkout == workspace:
        parser.error("the baseline must use a separate checkout")
    revision = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=checkout, text=True
    ).strip()
    if revision != "f3850b1ba444d7eceda89207261e85c1c794f8e5":
        parser.error("the baseline checkout must be at f3850b1")
    if subprocess.check_output(
        ["git", "status", "--porcelain", "--untracked-files=no"], cwd=checkout, text=True
    ).strip():
        parser.error("the baseline checkout must be clean")
    shutil.copyfile(workspace / "Cargo.lock", checkout / "Cargo.lock")
    subprocess.run(
        [
            "cargo", "install", "--offline", "--locked",
            "--path", str(checkout / "vtorrent-daemon"),
            "--root", str(args.install_root.resolve()),
            "--target-dir", str(workspace / "target"),
            "--features", "test-util",
        ],
        check=True,
    )


if __name__ == "__main__":
    main()
