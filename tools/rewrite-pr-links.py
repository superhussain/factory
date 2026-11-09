#!/usr/bin/env python3
"""Point pre-cutover board PR links at the archive repo.

Operator-run after a public cutover restarted PR numbers on --repo. A job's board `pr`
on --repo is rewritten to the same number on --archive only when that archive PR's head
branch is factory/<job id>. Dry run by default; --apply writes once through CAS.

Python 3.8 standard library only. The installed runtime is loaded by explicit path and
supplies the API client and CAS board writes, so this tool authenticates as a device.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys

LIMIT = 1000


def load_runtime(path):
    """The installed runtime module from --runtime <path-to-factory.py>."""
    path = os.path.abspath(path)
    if not os.path.isfile(path):
        fail(f"no runtime at {path}")
    spec = importlib.util.spec_from_file_location("factory_runtime_rewrite", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fail(msg, code=2):
    print(f"rewrite-pr-links: {msg}", file=sys.stderr)
    raise SystemExit(code)


def run(argv):
    return subprocess.run(argv, capture_output=True, text=True)


def pr_number(url, repo):
    """The PR number when url is exactly a PR on repo, else None."""
    m = re.fullmatch(r"https://github\.com/" + re.escape(repo) + r"/pull/(\d+)", url or "")
    return int(m.group(1)) if m else None


def archive_heads(archive):
    """{PR number: head branch} for every PR on the archive repo."""
    result = run(["gh", "pr", "list", "-R", archive, "--state", "all", "--limit", str(LIMIT),
                  "--json", "number,headRefName"])
    if result.returncode != 0:
        fail(f"gh pr list -R {archive} failed: {result.stderr.strip()}", 1)
    listed = json.loads(result.stdout)
    if len(listed) >= LIMIT:
        fail(f"archive lists {LIMIT} or more pull requests; nothing changed", 1)
    return {pr["number"]: pr["headRefName"] for pr in listed}


def plan(board, repo, archive, heads):
    """Rows {id, old, new} for jobs whose --repo PR number heads factory/<id> on the archive."""
    rows = []
    for job in board.get("jobs", []):
        n = pr_number(job.get("pr"), repo)
        if n is not None and heads.get(n) == "factory/" + job.get("id", ""):
            rows.append({"id": job["id"], "old": job["pr"],
                         "new": f"https://github.com/{archive}/pull/{n}"})
    return rows


def apply_rewrites(rt, home, rows):
    """One CAS board write; a row is rewritten only if its pr still equals old. Returns the count."""
    def change(text):
        board = rt.load_board(text)
        olds = {row["id"]: row for row in rows}
        count = 0
        for job in board["jobs"]:
            row = olds.get(job.get("id"))
            if row and job.get("pr") == row["old"]:
                job["pr"] = row["new"]
                count += 1
        if not count:
            return None, 0
        board["updated"] = rt.now()
        return rt.dump_board(board), count

    return rt.cas(home, "/api/board", change)


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="rewrite-pr-links.py",
        description="Rewrite board PR links whose archive PR head is factory/<job id> "
                    "to the archive repo. Dry run unless --apply.")
    parser.add_argument("--runtime", required=True, metavar="PATH",
                        help="path to the installed runtime factory.py to load")
    parser.add_argument("--home", metavar="PATH",
                        help="factory home (default: the runtime's own discovery)")
    parser.add_argument("--repo", required=True, metavar="OWNER/NAME",
                        help="the repo whose PR numbers restarted")
    parser.add_argument("--archive", required=True, metavar="OWNER/NAME",
                        help="the repo that holds the pre-cutover PRs")
    parser.add_argument("--apply", action="store_true", help="write the planned links")
    args = parser.parse_args(argv)
    rt = load_runtime(args.runtime)
    home = Path(args.home).resolve() if args.home else Path(rt.factory_home())
    if not home.is_dir():
        fail(f"no factory home at {home}")
    heads = archive_heads(args.archive)
    status, _, text = rt.api(home, "GET", "/api/board", ok=(200, 404), raw=True)
    board = rt.load_board(text.decode("utf-8") if status == 200 else None)
    rows = plan(board, args.repo, args.archive, heads)
    planned = {row["id"] for row in rows}
    for job in board.get("jobs", []):
        if job.get("id") not in planned and pr_number(job.get("pr"), args.repo) is not None:
            print(f"[keep] {job['id']}: {job['pr']}")
    for row in rows:
        print(f"[plan] {row['id']}: {row['old']} -> {row['new']}")
    if not rows:
        print("No board PR links point at archived pull requests.")
    elif args.apply:
        print(f"Rewrote {apply_rewrites(rt, home, rows)} link(s) on the board.")
    else:
        print(f"{len(rows)} link(s) to rewrite. Dry run: nothing changed. "
              "Re-run with --apply to act.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
