#!/usr/bin/env python3
"""Compose the repository's verification commands into device, hosted and all groups."""
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GROUPS = ("device", "hosted", "all")
HOSTED_SCRIPTS = ("test:assets", "test:auth", "test:protocol", "test:local", "test:browser", "test:deploy")


def plan(group):
    """Return [(id, argv)] for a group, or None when pnpm is required but missing."""
    discover = [sys.executable, "-B", "-m", "unittest", "discover"]
    device = [
        ("cli-unittest", discover + ["-s", "tests/cli", "-p", "test_*.py"]),
        ("install-unittest", discover + ["-s", "tests/tools", "-p", "test_install.py"]),
    ]
    if group == "device":
        return device
    pnpm = shutil.which("pnpm")
    if pnpm is None:
        return None
    hosted = [(script, [pnpm, "run", script]) for script in HOSTED_SCRIPTS]
    if group == "hosted":
        return hosted
    return device + hosted


def parse(argv):
    group = None
    json_output = False
    for arg in argv:
        if arg == "--json":
            json_output = True
        elif arg in GROUPS and group is None:
            group = arg
        else:
            return None, None
    if group is None:
        return None, None
    return group, json_output


def main(argv=None):
    group, json_output = parse(sys.argv[1:] if argv is None else argv)
    if group is None:
        print("usage: verify.py [--json] {device|hosted|all}", file=sys.stderr)
        return 2
    commands = plan(group)
    if commands is None:
        print("pnpm is required for the hosted checks but was not found on PATH", file=sys.stderr)
        return 2
    env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1", WRANGLER_SEND_METRICS="false")
    results = []
    for command_id, command in commands:
        code = subprocess.run(command, cwd=ROOT, env=env).returncode
        results.append({"id": command_id, "exit_code": code})
        if code != 0:
            print(f"{command_id} failed with exit code {code}", file=sys.stderr)
            status = "fail"
            break
    else:
        status = "pass"
    if json_output:
        print(json.dumps({"version": 1, "group": group, "status": status, "results": results}))
    return 0 if status == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
