"""Reassignment tool behavior on the loopback fixture: plan labels without writing,
gated and surgical apply, byte-identical frozen aliases, idempotency and reversal.

Runs under tools/local.mjs --check; reuses install_cli/test_url from test_project_api.py.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import urllib.error
import urllib.request
from datetime import datetime, timezone

from test_project_api import install_cli, test_url

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "tools" / "reassign-jobs.py"
FROM = "legacy"
TO = "factory"


def png_bytes():
    """A real 1x1 RGBA PNG built from stdlib parts."""
    import struct
    import zlib

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    header = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(b"\x00\x00\x00\x00\xff")) + chunk(b"IEND", b""))


def full_job(job_id, **over):
    """A merged job carrying every board field the migration must preserve, plus one
    unknown field, with a stale claim that must survive untouched."""
    job = {
        "id": job_id, "project": FROM, "idea": "Synthetic reassignment job\nBody line.",
        "title": "Synthetic reassignment job",
        "stage": "merged", "round": 2,
        "rounds": [{"spec": "PASS", "code": "CHANGES"}, {"spec": "PASS", "code": "PASS"}],
        "claim": {"device": "old-device", "session": "old", "since": "2026-01-01T00:00:00Z",
                  "seen": "2026-01-01T00:00:00Z"},
        "paseo": {"server": "synthetic-server", "agent": "synthetic-agent"},
        "refs": [{"id": "001-earlier", "kind": "follow-up-of"}],
        "build": {"state": "passed", "runs": ["https://github.com/example/old/actions/runs/5"]},
        "pr": "https://github.com/example/old/pull/7", "handoff": "synthetic handoff",
        "done": "2026-01-02T00:00:00Z", "needs": None, "note": "synthetic note",
        "escalated": True, "automerge": False, "resumes": ["r-1"],
        "capture_id": "00000000-0000-4000-8000-000000000000", "updated": "2026-01-02T00:00:00Z",
        "synthetic-unknown": {"kept": True},
    }
    job.update(over)
    return job


class ReassignTest(unittest.TestCase):
    maxDiff = None

    @classmethod
    def setUpClass(cls):
        cls.url = test_url()
        if cls.url is None:
            raise unittest.SkipTest("set FACTORY_TEST_URL to run local R2 integration")
        cls.factory = install_cli(cls)
        cls.runtime = cls.factory.__file__

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)

    # --- fixture helpers ---------------------------------------------------
    def request(self, path, method="GET", data=None, headers=None):
        req = urllib.request.Request(self.url + path, data, headers or {}, method=method)
        try:
            with urllib.request.build_opener(self.factory.NoRedirect).open(req, timeout=30) as r:
                return r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def seed_project(self, slug, repo):
        body = f"---\nrepo: {repo}\nbase: main\ntitle: Synthetic {slug}\n---\n".encode()
        path = f"/api/projects/{slug}.md"
        status, headers, _ = self.request(path)
        self.assertIn(status, (200, 404))
        cond = {"If-Match": self.factory.strong(headers["ETag"])} if status == 200 else {"If-None-Match": "*"}
        status, _, _ = self.request(path, "PUT", body, {"Content-Type": "text/markdown", **cond})
        self.assertEqual(status, 200)

    def seed_projects(self):
        self.seed_project(FROM, "example/old")
        self.seed_project(TO, "example/new")

    def seed_board(self, jobs):
        # The Worker refuses one write that drops an id and adds a row, so clear first.
        for rows in ([], jobs):
            status, headers, _ = self.request("/api/board")
            self.assertIn(status, (200, 404))
            cond = {"If-Match": self.factory.strong(headers["ETag"])} if status == 200 else {"If-None-Match": "*"}
            payload = json.dumps({"updated": "2026-01-01T00:00:00Z", "jobs": rows}).encode()
            status, _, _ = self.request("/api/board", "PUT", payload, {"Content-Type": "application/json", **cond})
            self.assertEqual(status, 200)

    def seed_artifact(self, slug, job_id, rel, data, ctype="text/markdown"):
        path = f"/api/jobs/{slug}/{job_id}/{rel}"
        status, headers, _ = self.request(path)
        self.assertIn(status, (200, 404))
        cond = {"If-Match": self.factory.strong(headers["ETag"])} if status == 200 else {"If-None-Match": "*"}
        status, _, _ = self.request(path, "PUT", data, {"Content-Type": ctype, **cond})
        self.assertEqual(status, 200, f"cannot seed {rel} for {job_id}")

    def seed_cache(self, home, slug, job_id):
        cache = Path(home, "jobs", slug, job_id)
        (cache / "round-1").mkdir(parents=True)
        (cache / "spec.md").write_text("local synthetic draft\n")
        (cache / "round-1" / "notes.md").write_text("round note\n")
        (cache / ".sync.json").write_text("{}\n")
        return cache

    def home(self, name):
        home = Path(self.temp.name) / name
        home.mkdir()
        (home / "config.md").write_text(f"---\napi_url: {self.url}\n---\n")
        (home / ".env").write_text("")
        return home

    def run_tool(self, home, *argv):
        return subprocess.run(
            [sys.executable, "-B", str(TOOL), "--from", FROM, "--runtime", str(self.runtime), "--home", str(home), *argv],
            capture_output=True, text=True, env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})

    def get(self, path):
        status, headers, body = self.request(path)
        self.assertEqual(status, 200, path)
        return headers, body

    def board(self):
        _, body = self.get("/api/board")
        return json.loads(body)

    def manifest(self, home, ids, slug=FROM):
        """The operator's backup manifest: board.json plus every artifact key, hashed."""
        _, board = self.get("/api/board")
        files = {"board.json": hashlib.sha256(board).hexdigest()}
        for job_id in ids:
            _, listing = self.get(f"/api/jobs/{slug}/{job_id}/?etags=1")
            for name in sorted(o["name"] for o in json.loads(listing)):
                _, data = self.get(f"/api/jobs/{slug}/{job_id}/{name}")
                files[f"jobs/{slug}/{job_id}/{name}"] = hashlib.sha256(data).hexdigest()
        path = Path(self.temp.name, f"manifest-{slug}-{'-'.join(ids) or 'none'}.json")
        path.write_text(json.dumps({"version": 1, "files": files}))
        return path

    def tree_hashes(self, root):
        out = {}
        for path in sorted(Path(root).rglob("*")):
            if path.is_file() and not path.is_symlink():
                out[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
        return out

    # --- tests -------------------------------------------------------------
    def test_plan_labels_and_writes_nothing(self):
        self.seed_projects()
        live = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        jobs = [
            full_job("p1-factory-migrate"),
            full_job("p2-plain"),
            full_job("p3-factory-open", stage="review", done=None),
            full_job("p4-factory-claimed",
                     claim={"device": "other-device", "session": "live", "since": live, "seen": live}),
            full_job("p5-factory-worktree", stage="closed"),
        ]
        self.seed_board(jobs)
        self.seed_artifact(FROM, "p1-factory-migrate", "spec.md", b"# synthetic spec\n")
        self.seed_artifact(FROM, "p1-factory-migrate", "shot.png", png_bytes(), "image/png")
        home = self.home("plan")
        self.seed_cache(home, FROM, "p1-factory-migrate")
        worktree = Path(home, "worktrees", FROM, "p5-factory-worktree")
        worktree.mkdir(parents=True)

        before_board = self.get("/api/board")[1]
        before_tree = self.tree_hashes(home)
        run = self.run_tool(home, "plan", "--json")
        after_board = self.get("/api/board")[1]

        self.assertEqual(run.returncode, 0, run.stderr)
        planed = json.loads(run.stdout)
        self.assertEqual([(j["id"], j["label"]) for j in planed["jobs"]], [
            ("p1-factory-migrate", "MIGRATE"), ("p2-plain", "KEEP"),
            ("p3-factory-open", "FLAG: non-terminal"), ("p4-factory-claimed", "FLAG: live claim"),
            ("p5-factory-worktree", "FLAG: existing worktree")])
        human = self.run_tool(home, "plan")
        self.assertEqual(human.returncode, 0, human.stderr)
        self.assertIn("plan made no writes", human.stdout)
        # Zero writes: board and artifacts byte-identical, cache and worktree unmoved.
        self.assertEqual(before_board, after_board)
        _, etags = self.get(f"/api/jobs/{FROM}/p1-factory-migrate/?etags=1")
        self.assertEqual([o["name"] for o in json.loads(etags)], ["shot.png", "spec.md"])
        self.assertEqual(self.tree_hashes(home), before_tree)

    def test_apply_refusals_change_nothing(self):
        self.seed_projects()
        live = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        self.seed_board([
            full_job("g1-factory-ok"),
            full_job("g2-factory-open", stage="review", done=None),
            full_job("g3-factory-claimed",
                     claim={"device": "other-device", "session": "live", "since": live, "seen": live}),
            full_job("g4-factory-worktree", stage="closed"),
        ])
        self.seed_artifact(FROM, "g1-factory-ok", "spec.md", b"# synthetic spec\n")
        home = self.home("refuse")
        self.seed_cache(home, FROM, "g1-factory-ok")
        Path(home, "worktrees", FROM, "g4-factory-worktree").mkdir(parents=True)
        good = self.manifest(home, ["g1-factory-ok"])
        before_board = self.get("/api/board")[1]
        before_tree = self.tree_hashes(home)

        missing = self.run_tool(home, "apply", "--ids", "g1-factory-ok",
                                "--backup", str(Path(self.temp.name, "absent.json")))
        self.assertEqual(missing.returncode, 2)
        self.assertIn("cannot read backup manifest", missing.stderr)

        no_board = Path(self.temp.name, "no-board.json")
        no_board.write_text(json.dumps({"version": 1, "files": {}}))
        uncovered = self.run_tool(home, "apply", "--ids", "g1-factory-ok", "--backup", str(no_board))
        self.assertEqual(uncovered.returncode, 2)
        self.assertIn("does not cover board.json", uncovered.stderr)

        stale = Path(self.temp.name, "stale.json")
        stale.write_text(json.dumps({"version": 1, "files": {"board.json": "0" * 64}}))
        mismatched = self.run_tool(home, "apply", "--ids", "g1-factory-ok", "--backup", str(stale))
        self.assertEqual(mismatched.returncode, 2)
        self.assertIn("does not match the live object", mismatched.stderr)

        for ids, needle in [("absent-job", "not on the board"), ("g2-factory-open", "non-terminal"),
                            ("g3-factory-claimed", "live claim"), ("g4-factory-worktree", "existing worktree")]:
            refused = self.run_tool(home, "apply", "--ids", ids, "--backup", str(good))
            self.assertEqual(refused.returncode, 2, refused.stderr)
            self.assertIn(needle, refused.stderr)

        # Every refusal left the board, cache and worktree untouched.
        self.assertEqual(self.get("/api/board")[1], before_board)
        self.assertEqual(self.tree_hashes(home), before_tree)

    def test_apply_is_surgical_and_preserves_board(self):
        self.seed_projects()
        self.seed_board([full_job("s1-factory-ok"), full_job("s2-factory-stays")])
        spec = b"# synthetic spec\n\nBody.\n"
        image = png_bytes()
        self.seed_artifact(FROM, "s1-factory-ok", "spec.md", spec)
        self.seed_artifact(FROM, "s1-factory-ok", "shot.png", image, "image/png")
        self.seed_artifact(FROM, "s1-factory-ok", "round-1/notes.md", b"round note\n")
        home = self.home("surgical")
        self.seed_cache(home, FROM, "s1-factory-ok")
        backup = self.manifest(home, ["s1-factory-ok"])
        before_jobs = self.board()["jobs"]
        _, old_spec = self.get(f"/api/jobs/{FROM}/s1-factory-ok/spec.md")

        run = self.run_tool(home, "apply", "--ids", "s1-factory-ok", "--backup", str(backup))
        self.assertEqual(run.returncode, 0, run.stderr)

        after = self.board()
        # Ids and array order unchanged; every field but project preserved verbatim.
        self.assertEqual([j["id"] for j in after["jobs"]], [j["id"] for j in before_jobs])
        for before, migrated in zip(before_jobs, after["jobs"]):
            expected = json.loads(json.dumps(before))
            if before["id"] == "s1-factory-ok":
                expected["project"] = TO
            self.assertEqual(migrated, expected)

        # Old and new artifact URLs resolve to identical bytes; old keys untouched.
        _, new_spec = self.get(f"/api/jobs/{TO}/s1-factory-ok/spec.md")
        self.assertEqual(hashlib.sha256(old_spec).hexdigest(), hashlib.sha256(new_spec).hexdigest())
        _, old_again = self.get(f"/api/jobs/{FROM}/s1-factory-ok/spec.md")
        self.assertEqual(old_again, old_spec)
        status, headers, body = self.request(f"/api/jobs/{TO}/s1-factory-ok/shot.png")
        self.assertEqual((status, body), (200, image))
        # Cache moved with its sync manifest; nothing outside home/jobs changed.
        self.assertTrue((Path(home, "jobs", TO, "s1-factory-ok") / ".sync.json").is_file())
        self.assertEqual((Path(home, "jobs", TO, "s1-factory-ok") / "spec.md").read_text(),
                         "local synthetic draft\n")
        self.assertFalse(Path(home, "jobs", FROM, "s1-factory-ok").exists())
        outside = {k: v for k, v in self.tree_hashes(home).items() if not k.startswith("jobs/")}
        self.assertEqual(list(outside), [".env", "config.md"])

    def test_apply_is_idempotent_and_reversible(self):
        self.seed_projects()
        self.seed_board([full_job("r1-factory-ok")])
        self.seed_artifact(FROM, "r1-factory-ok", "spec.md", b"# synthetic spec\n")
        home = self.home("idem")
        self.seed_cache(home, FROM, "r1-factory-ok")

        first = self.run_tool(home, "apply", "--ids", "r1-factory-ok",
                              "--backup", str(self.manifest(home, ["r1-factory-ok"])))
        self.assertEqual(first.returncode, 0, first.stderr)

        # Re-run with a refreshed manifest: verifies copies, then no-op (no board write).
        _, board_before = self.get("/api/board")
        second = self.run_tool(home, "apply", "--json", "--ids", "r1-factory-ok",
                               "--backup", str(self.manifest(home, ["r1-factory-ok"])))
        self.assertEqual(second.returncode, 0, second.stderr)
        _, board_after = self.get("/api/board")
        self.assertEqual(board_before, board_after)
        record = json.loads(second.stdout)
        self.assertEqual(record["jobs"][0]["board"], "noop")

        # Reversed apply restores the prior board and cache state.
        reversed_run = self.run_tool(home, "--from", TO, "--to", FROM, "apply", "--ids", "r1-factory-ok",
                                     "--backup", str(self.manifest(home, ["r1-factory-ok"], slug=TO)))
        self.assertEqual(reversed_run.returncode, 0, reversed_run.stderr)
        job = next(j for j in self.board()["jobs"] if j["id"] == "r1-factory-ok")
        self.assertEqual(job["project"], FROM)
        self.assertTrue((Path(home, "jobs", FROM, "r1-factory-ok") / ".sync.json").is_file())
        self.assertFalse(Path(home, "jobs", TO, "r1-factory-ok").exists())
        _, alias = self.get(f"/api/jobs/{TO}/r1-factory-ok/spec.md")
        _, original = self.get(f"/api/jobs/{FROM}/r1-factory-ok/spec.md")
        self.assertEqual(hashlib.sha256(alias).hexdigest(), hashlib.sha256(original).hexdigest())

    def test_apply_conflict_changes_nothing(self):
        self.seed_projects()
        self.seed_board([full_job("c1-factory-ok")])
        self.seed_artifact(FROM, "c1-factory-ok", "spec.md", b"# synthetic spec\n")
        divergent = b"# divergent new-prefix object\n"
        self.seed_artifact(TO, "c1-factory-ok", "spec.md", divergent)
        home = self.home("conflict")
        self.seed_cache(home, FROM, "c1-factory-ok")
        before_board = self.get("/api/board")[1]
        before_tree = self.tree_hashes(home)

        run = self.run_tool(home, "apply", "--json", "--ids", "c1-factory-ok",
                            "--backup", str(self.manifest(home, ["c1-factory-ok"])))
        self.assertEqual(run.returncode, 1, run.stderr)
        record = json.loads(run.stdout)
        self.assertEqual(record["flagged"], ["c1-factory-ok"])
        self.assertIn("conflict: jobs/factory/c1-factory-ok/spec.md already exists with "
                      "different bytes; c1-factory-ok was not changed", run.stderr)

        # The job, its cache and both prefixes are untouched.
        self.assertEqual(self.get("/api/board")[1], before_board)
        job = next(j for j in self.board()["jobs"] if j["id"] == "c1-factory-ok")
        self.assertEqual(job["project"], FROM)
        _, still_divergent = self.get(f"/api/jobs/{TO}/c1-factory-ok/spec.md")
        self.assertEqual(still_divergent, divergent)
        self.assertEqual(self.tree_hashes(home), before_tree)
