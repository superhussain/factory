#!/usr/bin/env python3
"""Move the private repository aside and publish the current tree as one commit in a fresh public repo.

Usage: public-cutover.py --repo OWNER/factory --archive OWNER/factory-private
  --author-name NAME --author-email EMAIL --project SLUG --runtime PATH/factory.py
  --home FACTORY_HOME [--checkout PATH]... [--apply | --repoint-only]

Read-only checks run first, before the plan and before --apply. Without --apply
it prints the plan and changes nothing. --apply renames the repo to the archive
name, creates the public repo, waits until a human has set every workflow secret
and variable value, pushes one commit, repoints each checkout, then watches
deploy-main and ci. --repoint-only skips the checks and only repoints the given
checkouts: it adds an archive remote, detaches at the new origin/main and renames
every local branch origin/main cannot reach to archive/<name>. Only names of
secrets and variables are read; values never pass through this script.
Exit 0 ok, 1 a check or step failed, 2 bad usage.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MESSAGE = "Initial public release"
CI_BRANCH = "cutover-ci-check"
RECOVERY = "See docs/public-cutover.md, Recovery."


def run(argv, cwd=None, env=None):
    return subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True)


def required_names(root):
    """Secret and variable names the workflows reference, without the built-in GITHUB_TOKEN."""
    text = "".join(path.read_text() for path in sorted(Path(root).glob(".github/workflows/*.yml")))
    secrets = set(re.findall(r"\bsecrets\.([A-Za-z_][A-Za-z0-9_]*)", text)) - {"GITHUB_TOKEN"}
    variables = set(re.findall(r"\bvars\.([A-Za-z_][A-Za-z0-9_]*)", text))
    return secrets, variables


def missing_names(repo):
    secrets, variables = required_names(ROOT)
    missing = []
    for kind, wanted in (("secret", secrets), ("variable", variables)):
        result = run(["gh", kind, "list", "-R", repo, "--json", "name"])
        have = {item["name"] for item in json.loads(result.stdout or "[]")} if result.returncode == 0 else set()
        missing += sorted(wanted - have)
    return missing


def report(ok, name, detail=""):
    print(f"[{'ok' if ok else 'fail'}] {name}{': ' + detail if detail else ''}")
    return ok


def checks(args):
    status = run(["git", "status", "--porcelain"], cwd=ROOT)
    heads = run(["git", "rev-parse", "HEAD", "origin/main"], cwd=ROOT).stdout.split()
    clean = status.returncode == 0 and not status.stdout.strip() and len(heads) == 2 and heads[0] == heads[1]
    results = [report(clean, "tree", "clean at origin/main" if clean else "dirty, or HEAD is not origin/main")]
    results.append(report(run(["gh", "auth", "status"]).returncode == 0, "auth"))
    jobs = run(["python3", args.runtime, "builds", args.project], env={**os.environ, "FACTORY_HOME": args.home})
    busy = json.loads(jobs.stdout) if jobs.returncode == 0 else {"in_flight": ["unknown"]}
    moving = busy.get("in_flight", []) + busy.get("stale", [])
    results.append(report(not moving, "jobs", ", ".join(moving) or "none in flight"))
    prs = run(["gh", "pr", "list", "-R", args.repo, "--state", "open", "--json", "number"])
    if prs.returncode != 0:
        results.append(report(False, "PRs", "could not list PRs"))
    else:
        open_prs = [str(pr["number"]) for pr in json.loads(prs.stdout)]
        results.append(report(not open_prs, "PRs", f"open {', '.join(open_prs)}" if open_prs else "none open"))
    free = run(["gh", "repo", "view", args.archive, "--json", "name"]).returncode != 0
    results.append(report(free, "archive name", f"{args.archive} is {'free' if free else 'taken'}"))
    results.append(report(run(["python3", "tools/secret-scan.py"], cwd=ROOT).returncode == 0, "gitleaks"))
    missing = missing_names(args.repo)
    secrets, variables = required_names(ROOT)
    results.append(report(not missing, "names", f"missing {', '.join(missing)}" if missing
                          else ", ".join(sorted(secrets | variables))))
    for path in args.checkout:
        dirty = run(["git", "-C", path, "status", "--porcelain"])
        results.append(report(dirty.returncode == 0 and not dirty.stdout.strip(), "checkout", path))
    return all(results)


def act(argv, cwd=None, env=None):
    print(f"[run] {' '.join(argv)}")
    result = run(argv, cwd=cwd, env=env)
    if result.returncode != 0:
        print(f"[fail] {' '.join(argv)}")
        if result.stderr.strip():
            print(result.stderr.strip())
        print(RECOVERY)
        raise SystemExit(1)
    return result.stdout.strip()


def repoint(args, path):
    git = ["git", "-C", path]
    origin = act([*git, "remote", "get-url", "origin"])
    if run([*git, "remote", "get-url", "archive"]).returncode != 0:
        act([*git, "remote", "add", "archive", origin.replace(args.repo, args.archive)])
    act([*git, "fetch", "origin", "--prune"])
    act([*git, "switch", "--detach", "origin/main"])
    # A private branch left under its own name would be reused and pushed to the public origin.
    branches = act([*git, "for-each-ref", "--format=%(refname:short)", "refs/heads"]).split()
    private = [name for name in branches if not name.startswith("archive/")
               and run([*git, "merge-base", "--is-ancestor", name, "origin/main"]).returncode != 0]
    for name in private:
        act([*git, "branch", "-m", name, f"archive/{name}"])
    renamed = ", ".join(f"{name} -> archive/{name}" for name in private)
    print(f"[ok] {path}: {'renamed ' + renamed if private else 'no private branches left'}")


def watch_run(repo, workflow, branch):
    for _ in range(20):
        runs = json.loads(act(["gh", "run", "list", "-R", repo, "--workflow", workflow, "--branch", branch,
                               "--json", "databaseId", "--limit", "1"]) or "[]")
        if runs:
            return run(["gh", "run", "watch", str(runs[0]["databaseId"]), "-R", repo, "--exit-status"]).returncode == 0
        time.sleep(15)
    return False


def cutover(args):
    archive_name = args.archive.split("/")[1]
    secrets, variables = required_names(ROOT)
    url = f"https://github.com/{args.repo}.git"
    act(["gh", "repo", "rename", archive_name, "-R", args.repo, "--yes"])
    act(["gh", "repo", "create", args.repo, "--public"])
    missing = sorted(secrets | variables)
    while missing:
        for name in missing:
            print(f"[human] gh {'secret' if name in secrets else 'variable'} set {name} -R {args.repo}")
        try:
            while input("Type done once every value is set: ").strip() != "done":
                pass
        except KeyboardInterrupt:
            print(f"\nStopped before the push. {RECOVERY}")
            return 1
        missing = missing_names(args.repo)
        report(not missing, "names", f"missing {', '.join(missing)}" if missing else f"present on {args.repo}")
    author = {"GIT_AUTHOR_NAME": args.author_name, "GIT_AUTHOR_EMAIL": args.author_email,
              "GIT_COMMITTER_NAME": args.author_name, "GIT_COMMITTER_EMAIL": args.author_email}
    env = {**os.environ, **author}
    tree = act(["git", "rev-parse", "HEAD^{tree}"], cwd=ROOT)
    commit = act(["git", "commit-tree", tree, "-m", MESSAGE], cwd=ROOT, env=env)
    act(["git", "push", url, f"{commit}:refs/heads/main"], cwd=ROOT)
    for path in args.checkout:
        repoint(args, path)
    project = Path(args.home) / "projects" / f"{args.project}.md"
    text = project.read_text()
    line = f"repo: {args.repo}"
    if re.search(rf"^{re.escape(line)}$", text, re.M):
        print(f"[ok] project file: {line} unchanged")
    else:
        project.write_text(re.sub(r"^repo: .*$", line, text, count=1, flags=re.M))
        act(["python3", args.runtime, "push", args.project], env={**os.environ, "FACTORY_HOME": args.home})
    if not report(watch_run(args.repo, "deploy-main.yml", "main"), "deploy", "deploy-main.yml on main"):
        return 1
    check = act(["git", "commit-tree", tree, "-p", commit, "-m", "Check CI after the public cutover"], cwd=ROOT, env=env)
    act(["git", "push", url, f"{check}:refs/heads/{CI_BRANCH}"], cwd=ROOT)
    act(["gh", "pr", "create", "-R", args.repo, "--head", CI_BRANCH, "--base", "main",
         "--title", "Check CI after the public cutover", "--body", "Empty commit; closed once ci passes."])
    ci = report(watch_run(args.repo, "ci.yml", CI_BRANCH), "CI", "ci on a pull request")
    act(["gh", "pr", "close", CI_BRANCH, "-R", args.repo, "--delete-branch"])
    return 0 if ci else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description="Cut the private repository over to a fresh public one.")
    for flag in ("--repo", "--archive", "--author-name", "--author-email", "--project", "--runtime", "--home"):
        parser.add_argument(flag, required=True)
    parser.add_argument("--checkout", action="append", default=[], help="a local checkout to repoint; repeatable")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--apply", action="store_true", help="perform the cutover")
    mode.add_argument("--repoint-only", action="store_true", help="only repoint the given checkouts")
    args = parser.parse_args(argv)
    if args.archive.split("/")[0] != args.repo.split("/")[0]:
        parser.error("--archive must have the same owner as --repo")
    if args.repoint_only and not args.checkout:
        parser.error("--repoint-only needs at least one --checkout")
    if args.repoint_only:
        for path in args.checkout:
            repoint(args, path)
        return 0
    if not checks(args):
        print("Checks failed: nothing changed.")
        return 1
    if args.apply:
        return cutover(args)
    secrets, variables = required_names(ROOT)
    print(f"[plan] gh repo rename {args.archive.split('/')[1]} -R {args.repo} --yes")
    print(f"[plan] gh repo create {args.repo} --public")
    for name in sorted(secrets):
        print(f"[human] gh secret set {name} -R {args.repo}")
    for name in sorted(variables):
        print(f"[human] gh variable set {name} -R {args.repo}")
    print(f'[plan] git commit-tree <HEAD tree> -m "{MESSAGE}"  (author {args.author_email})')
    print(f"[plan] git push https://github.com/{args.repo}.git <commit>:refs/heads/main")
    print("Dry run: nothing changed. Re-run with --apply to act.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
