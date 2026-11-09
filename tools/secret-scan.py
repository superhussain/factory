#!/usr/bin/env python3
"""Scan the tracked tree for secrets with gitleaks, or for private identifiers.

Usage: secret-scan.py [--private-patterns FILE] [--history OUT]
Default: copies `git ls-files` to a temp dir and runs `gitleaks dir` on it with
<repo>/.gitleaks.toml. Exit 0 clean, 1 findings, 2 gitleaks missing.
--private-patterns FILE: one regex per line; prints `<file>:<line>: private
pattern <n>` per hit, never the match. Keep FILE outside the tree.
--history OUT: runs `gitleaks git` over all history into the JSON report OUT
and prints `history: <n> findings`.
"""
import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def tracked(repo):
    out = subprocess.run(["git", "ls-files", "-z"], cwd=repo, check=True, capture_output=True).stdout
    return [name for name in out.decode().split("\0") if name and (repo / name).is_file()]


def private_scan(repo, patterns_file):
    patterns = [(n, re.compile(line)) for n, line in enumerate(Path(patterns_file).read_text().splitlines(), 1) if line.strip()]
    hits = 0
    for name in tracked(repo):
        text = (repo / name).read_bytes().decode("utf-8", errors="ignore")
        for lineno, line in enumerate(text.splitlines(), 1):
            for n, pattern in patterns:
                if pattern.search(line):
                    print(f"{name}:{lineno}: private pattern {n}")
                    hits += 1
    return 1 if hits else 0


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--private-patterns", metavar="FILE")
    parser.add_argument("--history", metavar="OUT")
    args = parser.parse_args(argv)
    repo = Path(subprocess.run(["git", "rev-parse", "--show-toplevel"], check=True, capture_output=True, text=True).stdout.strip())

    if args.private_patterns:
        return private_scan(repo, args.private_patterns)
    if not shutil.which("gitleaks"):
        print("gitleaks not found; run 'mise install'", file=sys.stderr)
        return 2
    if args.history:
        subprocess.run(["gitleaks", "git", "--redact", "--no-banner", "--report-format", "json", "--report-path", args.history, "--exit-code", "0"], cwd=repo, check=True)
        print(f"history: {len(json.loads(Path(args.history).read_text()))} findings")
        return 0
    with tempfile.TemporaryDirectory() as tmp:
        for name in tracked(repo):
            (Path(tmp) / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(repo / name, Path(tmp) / name, follow_symlinks=False)
        return subprocess.run(["gitleaks", "dir", ".", "--config", str(repo / ".gitleaks.toml"), "--redact", "--no-banner", "--exit-code", "1"], cwd=tmp).returncode


if __name__ == "__main__":
    sys.exit(main())
