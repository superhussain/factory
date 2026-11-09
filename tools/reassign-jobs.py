#!/usr/bin/env python3
"""Reassign jobs between projects on a running factory instance.

Operator-run migration tool: `plan` inventories candidate jobs without writing;
`apply` moves reviewed jobs' artifacts to the new project prefix as frozen
copy-aliases, rewrites only each job's board `project` field via CAS, and moves
the device-side job cache. Every old URL keeps resolving to the same bytes.

Python 3.8 standard library only. The installed runtime is loaded by explicit
path and supplies the API client, CAS board writes and sync guards, so this
tool authenticates as an ordinary device.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys

IMAGE_TYPES = {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
               ".gif": "image/gif", ".webp": "image/webp"}


def load_runtime(path):
    """The installed runtime module from --runtime <path-to-factory.py>."""
    path = os.path.abspath(path)
    if not os.path.isfile(path):
        fail(f"no runtime at {path}")
    spec = importlib.util.spec_from_file_location("factory_runtime_reassign", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fail(msg, code=2):
    print(f"reassign-jobs: {msg}", file=sys.stderr)
    raise SystemExit(code)


def refuse(msg):
    fail("refusing to apply: " + msg)


def is_candidate(job_id):
    """The spec's candidate rule: the board id names the factory project."""
    return "factory" in (job_id or "").lower()


def find_job(board, job_id):
    return next((j for j in board.get("jobs", []) if isinstance(j, dict) and j.get("id") == job_id), None)


def problem(rt, home, job, from_slug):
    """Why this --from job cannot be reassigned, or None."""
    if job.get("stage") not in rt.TERMINAL:
        return "non-terminal"
    device, session, minutes = rt.holder(home)
    if rt.blocking(job, device, session, minutes):
        return "live claim"
    if os.path.isdir(Path(home, "worktrees", from_slug, job["id"])):
        return "existing worktree"
    return None


def fetch_board(rt, home):
    """The live board, or the empty board when none exists yet."""
    status, _, text = rt.api(home, "GET", "/api/board", ok=(200, 404), raw=True)
    return rt.load_board(text.decode("utf-8") if status == 200 else None)


def fetch_projects(rt, home):
    status, _, text = rt.api(home, "GET", "/projects.json")
    listed = json.loads(text)
    if not isinstance(listed, list):
        fail("/projects.json is not a list: redeploy the factory Worker")
    return {p.get("slug") for p in listed if isinstance(p, dict)}


def list_artifacts(rt, home, slug, job_id):
    """Sorted artifact names under jobs/<slug>/<id>/, from the Worker's listing."""
    status, _, text = rt.api(home, "GET", f"/api/jobs/{slug}/{job_id}/?etags=1", ok=(200, 404), raw=True)
    listed = json.loads(text.decode("utf-8")) if status == 200 else []
    if not all(isinstance(o, dict) for o in listed):
        fail(f"the Worker lists no ETags for jobs/{slug}/{job_id}/: redeploy the factory Worker")
    return sorted(o["name"] for o in listed if rt.JOB_REL.fullmatch(o.get("name", "")))


def plan(args, rt, home):
    """Read-only inventory: every --from job labeled MIGRATE, KEEP, or FLAG: <reason>."""
    board = fetch_board(rt, home)
    registered = fetch_projects(rt, home)
    if args.from_slug not in registered:
        fail(f"project {args.from_slug} is not registered on this instance")
    rows = []
    for job in board.get("jobs", []):
        if not isinstance(job, dict) or job.get("project") != args.from_slug:
            continue
        job_id = job.get("id")
        row = {"id": job_id, "stage": job.get("stage"), "label": "KEEP"}
        if is_candidate(job_id):
            reason = problem(rt, home, job, args.from_slug)
            if reason:
                row["label"] = f"FLAG: {reason}"
            else:
                row["label"] = "MIGRATE"
        rows.append(row)
    if args.json:
        print(json.dumps({"from": args.from_slug, "to": args.to_slug, "jobs": rows}, indent=2))
        return 0
    width = max([len(str(r["id"])) for r in rows] + [2])
    print(f"{'id'.ljust(width)}  stage     action")
    for row in rows:
        print(f"{str(row['id']).ljust(width)}  {str(row['stage']).ljust(8)}  {row['label']}")
    migrate = sum(1 for r in rows if r["label"] == "MIGRATE")
    flags = sum(1 for r in rows if r["label"].startswith("FLAG:"))
    print(f"{len(rows)} job(s) in {args.from_slug}: {migrate} MIGRATE, "
          f"{len(rows) - migrate - flags} KEEP, {flags} FLAG; plan made no writes")
    return 0


def check_backup(rt, home, from_slug, ids, path):
    """Refuse unless the manifest covers board.json and every live jobs/<from>/<id>/
    key with the exact live sha256. The operator produces it from their own backup
    per docs/recovery.md; writers must be paused so the live state still matches."""
    try:
        manifest = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError, UnicodeError) as e:
        refuse(f"cannot read backup manifest {path}: {e}")
    if not isinstance(manifest, dict) or manifest.get("version") != 1 or not isinstance(manifest.get("files"), dict):
        refuse(f"{path}: backup manifest must be "
               '{"version": 1, "files": {"<key>": "<sha256>"}}')
    files = manifest["files"]

    def covered(key, data):
        if key not in files:
            refuse(f"backup manifest does not cover {key}")
        if files[key] != hashlib.sha256(data).hexdigest():
            refuse(f"backup manifest sha256 for {key} does not match the live object; "
                   "refresh the backup and manifest, or stop")

    status, _, board = rt.api(home, "GET", "/api/board", ok=(200, 404), raw=True)
    covered("board.json", board if status == 200 else b"")
    for job_id in ids:
        for rel in list_artifacts(rt, home, from_slug, job_id):
            key = f"jobs/{from_slug}/{job_id}/{rel}"
            _, _, data = rt.api(home, "GET", f"/{key}", raw=True)
            covered(key, data)


def apply_job(rt, home, args, job_id, result):
    """One reviewed id: create-only artifact copy, CAS board project change, cache move.
    Records the outcome in result. Artifacts are never deleted, so old-prefix URLs keep
    serving the same bytes."""
    record = {"id": job_id, "from": args.from_slug, "to": args.to_slug}
    result["jobs"].append(record)

    def flagged(msg):
        record["flag"] = msg
        result["flagged"].append(job_id)
        print(f"flagged {job_id}: {msg}", file=sys.stderr)

    # (a) copy artifacts to the new prefix, create-only.
    files = []
    for rel in list_artifacts(rt, home, args.from_slug, job_id):
        _, headers, data = rt.api(home, "GET", f"/api/jobs/{args.from_slug}/{job_id}/{rel}", raw=True)
        old_etag = rt.strong(headers.get("ETag"))
        digest = rt.sha(data)
        status, headers, _ = rt.api(home, "PUT", f"/api/jobs/{args.to_slug}/{job_id}/{rel}", data,
                                    (200, 412), {"If-None-Match": "*",
                                                 "Content-Type": IMAGE_TYPES.get(os.path.splitext(rel)[1],
                                                                                 "text/markdown; charset=utf-8")})
        if status == 412:
            _, eh, existing = rt.api(home, "GET", f"/api/jobs/{args.to_slug}/{job_id}/{rel}", raw=True)
            if rt.sha(existing) != digest:
                flagged(f"conflict: jobs/{args.to_slug}/{job_id}/{rel} already exists with "
                        f"different bytes; {job_id} was not changed")
                return
            new_etag = rt.strong(eh.get("ETag"))
        else:
            new_etag = rt.strong(headers.get("ETag"))
        files.append({"rel": rel, "old_etag": old_etag, "new_etag": new_etag, "sha256": digest})
    job = find_job(fetch_board(rt, home), job_id)
    record["stage"] = job.get("stage")
    record["pr"] = job.get("pr")
    record["files"] = files

    # (b) CAS board write changing only the job's project; top-level updated refreshes.
    def change(text):
        board = rt.load_board(text)
        job = find_job(board, job_id)
        if job is None:
            return None, "missing"
        if job.get("project") == args.to_slug:
            return None, "noop"
        job["project"] = args.to_slug
        board["updated"] = rt.now()
        return rt.dump_board(board), "done"

    outcome = rt.cas(home, "/api/board", change)
    if outcome == "missing":
        flagged(f"job {job_id} vanished from the board; {job_id} was not changed")
        return
    record["board"] = outcome

    # (c) move the local cache with its sync manifest; target-exists flags and continues.
    src = Path(rt.safe_job_folder(home, args.from_slug, job_id))
    dst = Path(home, "jobs", args.to_slug, job_id)
    if src.is_dir():
        if dst.exists():
            flagged(f"cache target {dst} already exists; the local cache for {job_id} "
                    "was not moved")
            return
        dst.parent.mkdir(parents=True, exist_ok=True)
        os.rename(src, dst)
        record["cache"] = str(dst)
    result["migrated"].append(job_id)
    print(f"{job_id}: {'migrated' if outcome == 'done' else 'already migrated (no-op)'}"
          f"{f', {len(files)} artifact(s) copied' if files else ''}", file=sys.stderr)


def apply(args, rt, home):
    """Only explicit reviewed ids; gated on the verified backup manifest."""
    ids = [i.strip() for i in args.ids.split(",") if i.strip()]
    if not ids:
        refuse("--ids needs at least one job id")
    if len(set(ids)) != len(ids):
        refuse("--ids has duplicate entries")
    registered = fetch_projects(rt, home)
    if args.from_slug not in registered or args.to_slug not in registered:
        refuse(f"both projects must be registered; {args.from_slug} and {args.to_slug} "
               f"are not both in /projects.json")
    board = fetch_board(rt, home)
    for job_id in ids:
        job = find_job(board, job_id)
        if job is None:
            refuse(f"job {job_id} is not on the board")
        if job.get("project") == args.to_slug:
            continue  # idempotent re-run: verified against the manifest, then no-op
        if job.get("project") != args.from_slug:
            refuse(f"job {job_id} is not in project {args.from_slug}")
        reason = problem(rt, home, job, args.from_slug)
        if reason:
            refuse(f"job {job_id}: {reason}")
    check_backup(rt, home, args.from_slug, ids, args.backup)

    result = {"from": args.from_slug, "to": args.to_slug, "jobs": [],
              "migrated": [], "flagged": []}
    for job_id in ids:
        apply_job(rt, home, args, job_id, result)
    record = json.dumps(result, indent=2) + "\n"
    if args.json:
        print(record)
    else:
        print(f"{len(result['migrated'])} migrated, {len(result['flagged'])} flagged; "
              f"old artifact URLs keep serving the same bytes as frozen copies")
    return 1 if result["flagged"] else 0


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="reassign-jobs.py",
        description="Reassign factory jobs between projects: read-only plan, then a "
                    "backup-manifest-gated apply that copies artifacts (frozen aliases), "
                    "rewrites the board project and moves the device cache.")
    parser.add_argument("--runtime", required=True, metavar="PATH",
                        help="path to the installed runtime factory.py to load")
    parser.add_argument("--home", metavar="PATH",
                        help="factory home (default: the runtime's own discovery)")
    parser.add_argument("--from", dest="from_slug", required=True, metavar="SLUG",
                        help="project to move jobs out of")
    parser.add_argument("--to", dest="to_slug", default="factory", metavar="SLUG",
                        help="project to move jobs into (default: factory)")
    sub = parser.add_subparsers(dest="command", required=True)
    plan_parser = sub.add_parser("plan", help="label every --from job; makes no writes")
    plan_parser.add_argument("--json", action="store_true", help="print the plan as JSON")
    plan_parser.set_defaults(func=plan)
    apply_parser = sub.add_parser("apply", help="migrate the reviewed ids only")
    apply_parser.add_argument("--ids", required=True, metavar="IDS",
                              help="comma-separated reviewed job ids")
    apply_parser.add_argument("--backup", required=True, metavar="MANIFEST",
                              help='backup manifest {"version":1,"files":{"<key>":"<sha256>"}} '
                                   "covering board.json and all jobs/<from>/<id>/ keys")
    apply_parser.add_argument("--json", action="store_true",
                              help="print the result record as JSON on stdout")
    apply_parser.set_defaults(func=apply)
    args = parser.parse_args(argv)
    rt = load_runtime(args.runtime)
    home = Path(args.home).resolve() if args.home else Path(rt.factory_home())
    if not home.is_dir():
        fail(f"no factory home at {home}")
    return args.func(args, rt, home)


if __name__ == "__main__":
    raise SystemExit(main())
