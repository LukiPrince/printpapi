#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Builds what the update steps of `setup_smoke.py` need, for the CI smoke job only.

The agent is built a second time as version 99.0.0 (with the test key), together with the two
examples the smoke test uses (`sign_update`, `fake_tray`), into `target-new`. The workspace version
is changed for the build and put back afterwards, and the build is done without `--locked`: the
version lives in Cargo.toml and Cargo.lock, which the members pin. Never run this in a checkout you
want to keep: it rewrites Cargo.toml and Cargo.lock for the time of the build and restores both
(`git checkout`), so uncommitted changes in those two files are lost.

Python 3 stdlib only.
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VERSION = "99.0.0"


def set_version(text, version):
    """`text` of the root Cargo.toml with the workspace version set. Exactly one line changes."""
    new, n = re.subn(r'(?m)^(version\s*=\s*)"[^"]*"(\s*)$', rf'\g<1>"{version}"\g<2>', text, count=1)
    if n != 1:
        raise SystemExit("no workspace version line in Cargo.toml")
    return new


def main():
    cargo = ROOT / "Cargo.toml"
    original = cargo.read_text(encoding="utf-8")
    try:
        cargo.write_text(set_version(original, VERSION), encoding="utf-8")
        subprocess.run(
            ["cargo", "build", "--release", "-p", "printpapi-agent", "--features",
             "printpapi-agent/update-test-key", "--bin", "printpapi-agent", "--examples",
             "--target-dir", "target-new"],
            cwd=ROOT, check=True)
    finally:
        subprocess.run(["git", "checkout", "--", "Cargo.toml", "Cargo.lock"], cwd=ROOT, check=False)
    print(f"built {VERSION} into {ROOT / 'target-new'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
