#!/usr/bin/env python3
"""Copy old <source>/factory-* PR screenshots into per-project folders in the configured R2 bucket.

Usage:
  copy-pr-screenshots.py plan --home <factory home> --source-project <slug> --out <plan.tsv>
  copy-pr-screenshots.py apply --home <factory home> --source-project <slug> --plan <plan.tsv>
Reads CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID, PR_SCREENSHOTS_BUCKET and PR_SCREENSHOTS_ORIGIN
from <home>/.env. plan maps each <source>/factory-<job-id>/<rest> key to
<project>/factory-<job-id>/<rest> using the host's job folders. apply copies each row without
overwriting, then checks the bytes on the public origin.
"""
import argparse
import http.client
import importlib.util
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

_spec = importlib.util.spec_from_file_location("pr_screenshots", Path(__file__).resolve().parents[1] / "skills" / "factory" / "runtime" / "pr_screenshots.py")
_uploader = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_uploader)
SLUG = _uploader.SLUG

HEADER = "source\tdestination\treason"
OK = {"copied", "in place", "exists-same"}
TIMEOUT = 300
READ_HINT = " (check that CLOUDFLARE_API_TOKEN can read the PR_SCREENSHOTS_BUCKET R2 bucket)"


def credentials(home):
    config = _uploader.publisher(Path(home) / ".env")
    if isinstance(config, str):
        print(config, file=sys.stderr)
        return None
    return config


def bucket_url(account, bucket):
    return f"https://api.cloudflare.com/client/v4/accounts/{account}/r2/buckets/{bucket}/objects"


def fetch(url, headers, data=None, method="GET"):
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
        return response.read(), response.headers.get("Content-Type")


def reason(error):
    if isinstance(error, urllib.error.HTTPError):
        return f"HTTP {error.code}"
    cause = error.reason if isinstance(error, urllib.error.URLError) else error
    if isinstance(cause, TimeoutError):
        return f"timed out after {TIMEOUT} s"
    return f"network error: {cause}"


def key_re(project):
    return re.compile(rf"^{re.escape(project)}/factory-([^/]+)/(.+)$")


def key_match(key, project):
    if any(segment in (".", "..") for segment in key.split("/")):
        return None
    return key_re(project).match(key)


def list_keys(account, token, bucket, project):
    keys = []
    cursor = None
    while True:
        query = {"prefix": f"{project}/factory-", "per_page": "1000"}
        if cursor:
            query["cursor"] = cursor
        body, _ = fetch(f"{bucket_url(account, bucket)}?{urllib.parse.urlencode(query)}", {"Authorization": f"Bearer {token}"})
        page = json.loads(body)
        keys += [item["key"] for item in page["result"]]
        info = page.get("result_info") or {}
        if not info.get("is_truncated"):
            return keys
        cursor = info["cursor"]


def project_for(home, job_id, source):
    jobs = Path(home) / "jobs"
    matches = sorted(
        p.name
        for p in (jobs.iterdir() if jobs.is_dir() else [])
        if (p / job_id).is_dir() and any(f.is_file() for f in (p / job_id).iterdir())
    )
    if not matches:
        return "other", "unknown"
    if len(matches) > 1:
        return "other", f"ambiguous: {','.join(matches)}"
    if matches[0] == source:
        return source, "in place"
    return matches[0], "job folder"


def plan(args):
    creds = credentials(args.home)
    if not creds:
        return 1
    account, token, bucket, _ = creds
    try:
        keys = list_keys(account, token, bucket, args.source_project)
    except (OSError, http.client.HTTPException) as e:
        hint = READ_HINT if isinstance(e, urllib.error.HTTPError) and e.code == 403 else ""
        print(f"listing failed: {reason(e)}{hint}", file=sys.stderr)
        return 1
    except (ValueError, KeyError, TypeError, AttributeError):
        print("listing failed: bad response", file=sys.stderr)
        return 1
    rows = []
    for key in keys:
        match = key_match(key, args.source_project)
        if not match:
            print(f"skipped: {key}", file=sys.stderr)
            continue
        project, basis = project_for(args.home, match.group(1), args.source_project)
        rows.append((key, f"{project}/{key[len(args.source_project) + 1:]}", basis))
    Path(args.out).write_text("".join(f"{line}\n" for line in [HEADER, *("\t".join(row) for row in rows)]), encoding="utf-8")
    counts = [sum(1 for row in rows if row[2].startswith(kind)) for kind in ("job folder", "in place", "unknown", "ambiguous")]
    print(f"planned {len(rows)} rows: {counts[0]} job folder, {counts[1]} in place, {counts[2]} unknown, {counts[3]} ambiguous")
    return 0


def copy_row(account, token, bucket, origin, source, destination):
    api = {"Authorization": f"Bearer {token}"}
    step = "source read"
    try:
        data, content_type = fetch(f"{bucket_url(account, bucket)}/{urllib.parse.quote(source, safe='/')}", api)
        step = "destination check"
        try:
            existing, _ = fetch(f"{bucket_url(account, bucket)}/{urllib.parse.quote(destination, safe='/')}", api)
            return ("exists-same" if existing == data else "exists-different"), ""
        except urllib.error.HTTPError as e:
            if e.code != 404:
                raise
        step = "upload"
        fetch(
            f"{bucket_url(account, bucket)}/{urllib.parse.quote(destination, safe='/')}",
            {**api, "Content-Type": content_type or "application/octet-stream"},
            data=data,
            method="PUT",
        )
        step = "public check"
        public, _ = fetch(f"{origin}/{urllib.parse.quote(destination, safe='/')}", {"User-Agent": "factory-copy-pr-screenshots"})
        return ("copied" if public == data else "mismatch"), ""
    except (OSError, http.client.HTTPException) as e:
        return "failed", f"{step}: {reason(e)}"


def apply(args):
    creds = credentials(args.home)
    if not creds:
        return 1
    lines = Path(args.plan).read_text(encoding="utf-8").splitlines()
    rows = []
    for n, line in enumerate(lines[1:], start=1):
        fields = line.split("\t")
        source, destination = (fields + ["", ""])[:2]
        slug = destination.split("/", 1)[0]
        if not key_match(source, args.source_project) or not SLUG.fullmatch(slug) or destination != f"{slug}/{source[len(args.source_project) + 1:]}":
            print(f"bad plan row {n}", file=sys.stderr)
            return 1
        rows.append((source, destination))

    failed = False
    for source, destination in rows:
        if destination == source:
            status, detail = "in place", ""
        else:
            status, detail = copy_row(*creds, source, destination)
        failed = failed or status not in OK
        print(f"{status}\t{source}\t{creds[3]}/{destination}" + (f"\t{detail}" if detail else ""))
    return 1 if failed else 0


def main(argv=None):
    parser = argparse.ArgumentParser(prog="copy-pr-screenshots.py")
    commands = parser.add_subparsers(dest="command", required=True)
    plan_parser = commands.add_parser("plan")
    plan_parser.add_argument("--home", required=True)
    plan_parser.add_argument("--source-project", required=True, metavar="SLUG")
    plan_parser.add_argument("--out", required=True)
    plan_parser.set_defaults(run=plan)
    apply_parser = commands.add_parser("apply")
    apply_parser.add_argument("--home", required=True)
    apply_parser.add_argument("--source-project", required=True, metavar="SLUG")
    apply_parser.add_argument("--plan", required=True)
    apply_parser.set_defaults(run=apply)
    args = parser.parse_args(argv)
    return args.run(args)


if __name__ == "__main__":
    sys.exit(main())
