import contextlib
import io
import json
import os
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
TOKEN = "synthetic-token-0123456789"
API_URL = "https://factory.example.invalid"
PICKUP_TAIL = "the next pickup retries (or run factory.py pickup)."

SPEC = """# Spec: Sample feature

## Goal

Something.

## Tasks

1. **First step**
   - Files: `src/one.py` (new), `src/two.py`
   - Change: do one.

2. **Second step.**
   - Files: `/etc/passwd`, `../up.py`
   - Change: do two.

3. Third step:
   - Change: do three.

## Docs to update

Nothing.
"""


class FakeTracer:
    """A Tracer stand-in on 127.0.0.1 with an ephemeral port and an optional path prefix."""

    def __init__(self, prefix="", token=TOKEN):
        self.prefix, self.token = prefix, token
        self.scope_locked = False
        self.delay = 0
        self.stop_answer = (409, {"detail": "No build is running."})
        self.features = {}
        self.requests = []

    def __enter__(self):
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def handle_any(self):
                length = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(length)
                body = json.loads(raw) if raw else None
                fake.requests.append((self.command, self.path, dict(self.headers), body))
                if fake.delay:
                    time.sleep(fake.delay)
                status, out = fake.answer(self.command, self.path, self.headers.get("Authorization"), body)
                data = json.dumps(out).encode()
                try:
                    self.send_response(status)
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except ConnectionError:
                    pass  # The client gave up first, as test_slow_tracer_times_out_once makes it.
            do_GET = do_POST = do_PUT = do_PATCH = handle_any

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}{self.prefix}"
        threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True).start()
        return self

    def __exit__(self, *exc):
        self.server.shutdown()
        self.server.server_close()

    def answer(self, method, path, auth, body):
        if auth != f"Bearer {self.token}":
            return 401, {"detail": "bad token"}
        if not path.startswith(self.prefix + "/api/"):
            return 404, {"detail": "no route"}
        parts = path[len(self.prefix) + 1:].split("/")[1:]  # after "api"
        if parts == ["features"]:
            if method == "GET":
                return 200, list(self.features.values())
            held = self.features.get(body["id"])
            if (held and held["ticket"]) or any(f["ticket"] == body["ticket"] for f in self.features.values()):
                return 409, {"detail": "exists"}
            feature = dict(held or {"tasks": [], "agents": []}, **body)
            self.features[body["id"]] = feature
            return 201, feature
        feature = self.features.get(parts[1])
        if feature is None:
            return 404, {"detail": "no feature"}
        if len(parts) == 2:
            return 200, feature
        if parts[2] == "agents":
            feature["agents"] = [a for a in feature["agents"] if a["id"] != parts[3]] + [dict(body, id=parts[3])]
            return 200, body
        if parts[2:] == ["build-runs", "stop"]:
            return self.stop_answer
        task = next((t for t in feature["tasks"] if t["id"] == parts[3]), None)
        if parts[2] == "tasks" and len(parts) == 4:
            if method == "PUT":
                task = task or {"id": parts[3], "test_runs": [], "agent": None}
                feature["tasks"] = [t for t in feature["tasks"] if t is not task] + [dict(task, **body)]
                return 200, task
            if self.scope_locked:
                return 409, {"detail": "scope is not approved"}
            task.update(body)
            return 200, task
        if parts[4] == "test-results":
            task["test_runs"].append(dict(body, id=f"r{len(task['test_runs']) + 1}"))
            return 201, body
        return 404, {"detail": "no route"}


class TracerCase(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.addCleanup(patch.stopall)
        self.home = Path(temp.name).resolve() / "home"
        self.home.mkdir()
        self.token_file = Path(temp.name) / "token"
        self.token_file.write_text(TOKEN + "\n")
        (self.home / "config.md").write_text(f"---\ndevice: test\napi_url: {API_URL}\n---\n")
        self.job = {"id": "001-sample", "project": "sample", "stage": "spec", "jira": "PLAN-1234",
                    "title": "Sample feature"}
        self.board = {"jobs": [self.job]}
        self.board_writes = 0
        self.pipeline = {"schema": 1, "tracer": {"enabled": True}}
        self.folder = self.home / "jobs" / "sample" / "001-sample"
        self.folder.mkdir(parents=True)
        (self.folder / "spec.md").write_text(SPEC)
        patch.object(factory, "api", side_effect=self.api).start()

    def api(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if path == "/api/pipeline/global":
            if self.pipeline is None:
                return 404, {}, "not found"
            return 200, {"ETag": "p"}, json.dumps(self.pipeline)
        assert path == "/api/board", path
        text = json.dumps(self.board)
        if method == "PUT":
            self.board = json.loads(body)
            self.board_writes += 1
            return 200, {"ETag": "x"}, ""
        return 200, {"ETag": "x"}, text

    def configure(self, fake, **extra):
        entry = dict({"url": fake.url, "token_file": str(self.token_file), "service": "sample",
                      "language": "Python"}, **extra)
        (self.home / "tracer.json").write_text(json.dumps({"version": 1, "projects": {"sample": entry}}))

    def board_patch(self, patch_body):
        with patch.object(factory.sys, "stdin", io.StringIO(json.dumps(patch_body))):
            return invoke(factory, ["--home", str(self.home), "board", str(self.home), "001-sample"])

    def sync(self, *args):
        return invoke(factory, ["--home", str(self.home), "tracer", "sync", *args])

    def review(self, n, verdicts):
        d = self.folder / f"round-{n}"
        d.mkdir(exist_ok=True)
        for kind, verdict in verdicts.items():
            (d / f"review-{kind}.md").write_text(f"VERDICT: {verdict}\n\nBody.\n")


class ConfigTest(TracerCase):
    def write(self, value):
        (self.home / "tracer.json").write_text(value if isinstance(value, str) else json.dumps(value))

    def entry(self, **change):
        entry = {"url": "https://tracer.example.invalid/base/", "token_file": str(self.token_file),
                 "service": "sample"}
        entry.update(change)
        return {"version": 1, "projects": {"sample": entry}}

    def refused(self, value, needle):
        self.write(value)
        code, out, err = self.sync()
        self.assertEqual(code, 2, err)
        self.assertIn(needle, err)
        if not needle.startswith("tracer token_file"):
            self.assertIn(str(self.home / "tracer.json"), err)
        self.assertNotIn(TOKEN, out + err)

    def test_config_validation(self):
        self.assertEqual(factory.read_tracer(self.home), {})
        self.write(self.entry(language="Python"))
        self.assertEqual(factory.read_tracer(self.home)["sample"], {
            "url": "https://tracer.example.invalid/base", "token_file": str(self.token_file),
            "service": "sample", "language": "Python"})
        self.refused("{not json", "not a readable regular JSON file")
        self.refused({"version": 2, "projects": {}}, "version")
        self.refused({"version": 1, "projects": {}, "extra": 1}, "version")
        self.refused(self.entry(extra=1), "unknown key extra")
        self.refused(self.entry(url="http://tracer.example.invalid"), "sample.url")
        self.refused(self.entry(url="https://user@tracer.example.invalid"), "sample.url")
        self.refused(self.entry(url="https://tracer.example.invalid/?q=1"), "sample.url")
        self.refused(self.entry(token_file="relative/token"), "absolute")
        self.refused(self.entry(token_file=str(self.token_file.parent / "missing")), "tracer token_file for sample")
        self.refused(self.entry(service="Bad Name"), "sample.service")
        self.refused(self.entry(language="x" * 41), "sample.language")
        link = self.token_file.parent / "link"
        os.symlink(self.token_file, link)
        self.refused(self.entry(token_file=str(link)), "tracer token_file for sample")
        empty = self.token_file.parent / "empty"
        empty.write_text(" \n")
        self.refused(self.entry(token_file=str(empty)), "tracer token_file for sample")
        spaced = self.token_file.parent / "spaced"
        spaced.write_text("two words\n")
        self.refused(self.entry(token_file=str(spaced)), "tracer token_file for sample")

    def test_base_url_with_a_path_works(self):
        with FakeTracer(prefix="/tracer") as fake:
            self.configure(fake)
            code, out, err = self.sync()
            self.assertEqual((code, err), (0, ""), err)
            self.assertIn("PLAN-1234 as Tracer feature plan-1234", out)
            self.assertTrue(all(r[1].startswith("/tracer/api/") for r in fake.requests))


class BoardHookTest(TracerCase):
    def test_board_without_config_makes_no_tracer_call(self):
        code, out, err = self.board_patch({"stage": "build"})
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out)["stage"], "build")
        self.assertEqual(self.board_writes, 1)

    def test_board_stage_patch_mirrors_feature(self):
        with FakeTracer() as fake:
            self.configure(fake)
            code, out, err = self.board_patch({"stage": "spec"})
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual(json.loads(out)["id"], "001-sample")
            feature = fake.features["plan-1234"]
            self.assertEqual(feature["ticket"], "PLAN-1234")
            self.assertEqual(feature["title"], "Sample feature")
            self.assertEqual(feature["services"], [{"name": "sample", "language": "Python"}])
            self.assertEqual(feature["reading"], f"Built by the factory as job 001-sample: {API_URL}/#001-sample. "
                             "The factory builds this feature, so approving scope in Tracer starts a second build.")
            posted = next(r[3] for r in fake.requests if r[0] == "POST" and r[1] == "/api/features")
            self.assertNotIn("start_gather", posted)
            self.assertEqual(feature["agents"], [{"id": "factory-builder", "name": "Factory builder",
                                                  "role": "Builds 001-sample from its spec"}])
            tasks = {t["id"]: t for t in feature["tasks"]}
            self.assertEqual(sorted(tasks), ["t1", "t2", "t3"])
            self.assertEqual([tasks[k]["title"] for k in sorted(tasks)], ["First step", "Second step", "Third step"])
            self.assertEqual(tasks["t1"]["path"], "src/one.py")
            self.assertNotIn("path", tasks["t2"])
            self.assertIn("do one.", tasks["t1"]["what"])
            self.assertEqual([tasks[k]["depends_on"] for k in sorted(tasks)], [[], ["t1"], ["t2"]])
            self.assertEqual(tasks["t3"]["why"], "Task 3 of the factory spec for 001-sample.")
            self.assertEqual({t["agent"] for t in tasks.values()}, {"factory-builder"})
            self.assertEqual({t["status"] for t in tasks.values()}, {"not_started"})
            for _, _, headers, _ in fake.requests:
                self.assertEqual(headers["Authorization"], f"Bearer {TOKEN}")

    def test_follow_patch_validation(self):
        code, out, err = self.board_patch({"tracer": {"follow": True}})
        self.assertEqual((code, err), (0, ""), err)
        self.assertEqual(self.board["jobs"][0]["tracer"], {"follow": True})
        for bad in ({"url": "x"}, {"feature": "f"}, {"follow": False}, {"follow": True, "url": "x"}, "yes", []):
            writes = self.board_writes
            code, _, err = self.board_patch({"tracer": bad})
            self.assertEqual(code, 2, bad)
            self.assertIn('tracer must be {"follow": true} or null', err)
            self.assertEqual(self.board_writes, writes)
        self.assertEqual(self.board_patch({"tracer": {"follow": None}})[0], 0)
        self.assertEqual(self.board_patch({"tracer": None})[0], 0)
        self.assertNotIn("tracer", self.board["jobs"][0])
        code, _, err = self.board_patch({"tracer": {"building": True}})
        self.assertEqual(code, 2)
        self.assertIn("tracer.building", err)
        code, _, err = self.board_patch({"tracer": {"tasks": []}})
        self.assertEqual(code, 2)
        self.assertIn("tracer.tasks", err)

    def test_follow_patch_mirrors_and_links(self):
        with FakeTracer() as fake:
            self.configure(fake)
            code, out, err = self.board_patch({"tracer": {"follow": True}})
            self.assertEqual((code, err), (0, ""), err)
            self.assertIn("plan-1234", fake.features)
            job = self.board["jobs"][0]
            self.assertEqual(job["tracer"], {"follow": True, "feature": "plan-1234",
                                             "url": f"{fake.url}/#/f/plan-1234"})
            self.assertEqual(job["updated"], json.loads(out)["updated"])
            self.assertEqual(self.board_writes, 2)
            self.board_patch({"stage": "build"})
            self.assertEqual(self.board_writes, 3)

    def test_follow_pass_once(self):
        self.job.update(stage="build", tracer={"follow": True})
        del self.job["jira"]
        (self.folder / "spec.md").unlink()
        pulled = []

        def pull(home, slug, job_id):
            pulled.append((slug, job_id))
            (home / "jobs" / slug / job_id / "spec.md").write_text(SPEC)
            print("pulled")
        patch.object(factory, "pull_job", side_effect=pull).start()
        factory.tracer_follow_pass(self.home)
        self.assertEqual((pulled, self.board_writes), ([], 0))
        with FakeTracer() as fake:
            self.configure(fake)
            with contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_follow_pass(self.home)
            self.assertEqual((out.getvalue(), err.getvalue()), ("", ""))
            self.assertEqual(pulled, [("sample", "001-sample")])
            self.assertEqual(fake.features["factory-1"]["ticket"], "FACTORY-1")
            self.assertEqual(self.board["jobs"][0]["tracer"], {"follow": True, "feature": "factory-1",
                                                               "url": f"{fake.url}/#/f/factory-1"})
            calls = len(fake.requests)
            factory.tracer_follow_pass(self.home)
            self.assertEqual(len(fake.requests), calls)
            self.assertEqual(len(pulled), 1)

    def test_board_patch_without_stage_does_not_mirror(self):
        with FakeTracer() as fake:
            self.configure(fake)
            code, _, err = self.board_patch({"note": "hello"})
            self.assertEqual((code, err), (0, ""))
            self.assertEqual(fake.requests, [])

    def test_unconfigured_project_and_unmirrored_stage_are_silent(self):
        with FakeTracer() as fake:
            self.configure(fake)
            self.assertEqual(self.board_patch({"stage": "idea"})[::2], (0, ""))
            self.board["jobs"][0]["project"] = "other"
            self.assertEqual(self.board_patch({"stage": "build"})[::2], (0, ""))
            self.assertEqual(fake.requests, [])

    def test_unreachable_tracer_keeps_board_patch(self):
        with FakeTracer() as fake:
            self.configure(fake)
        code, out, err = self.board_patch({"stage": "build"})
        self.assertEqual(code, 0)
        self.assertEqual(self.board["jobs"][0]["stage"], "build")
        self.assertEqual(len(err.splitlines()), 1)
        self.assertTrue(err.startswith("factory.py: warning: Tracer mirror of sample/001-sample failed: can't reach Tracer at "))
        self.assertIn("run factory.py tracer sync sample/001-sample to retry.", err)
        self.assertEqual(json.loads(out)["stage"], "build")

    def test_slow_tracer_times_out_once(self):
        with FakeTracer() as fake:
            fake.delay = 1.5
            self.configure(fake)
            with patch.object(factory, "TRACER_TIMEOUT_SECONDS", 0.3):
                started = time.monotonic()
                code, _, err = self.board_patch({"stage": "build"})
                took = time.monotonic() - started
            self.assertEqual(code, 0)
            self.assertLess(took, 1.2)
            self.assertEqual(len(err.splitlines()), 1)
            self.assertIn("failed: can't reach Tracer", err)
            self.assertEqual(len(fake.requests), 1)

    def test_another_projects_broken_entry_stays_silent(self):
        (self.home / "tracer.json").write_text(json.dumps({"version": 1, "projects": {
            "other": {"url": "ftp://x", "token_file": "/x", "service": "x"}}}))
        self.assertEqual(self.board_patch({"stage": "build"})[::2], (0, ""))

    def test_warning_does_not_double_a_final_period(self):
        with patch.object(factory, "tracer_mirror", side_effect=SystemExit(5)), \
                patch.object(factory, "read_tracer", return_value={"sample": {}}):
            code, _, err = self.board_patch({"stage": "build"})
        self.assertNotIn("..", err)

    def test_broken_config_warns_without_failing_the_patch(self):
        (self.home / "tracer.json").write_text("{nope")
        code, _, err = self.board_patch({"stage": "build"})
        self.assertEqual(code, 0)
        self.assertEqual(len(err.splitlines()), 1)
        self.assertIn("failed:", err)



class GatherTest(TracerCase):
    def setUp(self):
        super().setUp()
        self.job.update(stage="idea", idea="Export samples as CSV\nWith a header row.")
        del self.job["jira"], self.job["title"]
        (self.folder / "spec.md").unlink()

    def gather(self, fake):
        self.configure(fake)
        code, _, err = self.board_patch({"tracer": {"follow": True, "gather": True}})
        self.assertEqual((code, err), (0, ""), err)

    def hold_at_gate(self):
        self.board["jobs"][0].update(stage="needs-human", note="gate: tracer-scope", needs="waiting on Tracer")

    def test_gather_patch_validation(self):
        code, _, err = self.board_patch({"tracer": {"follow": True, "gather": True}})
        self.assertEqual((code, err), (0, ""), err)
        self.assertEqual(self.board["jobs"][0]["tracer"], {"follow": True, "gather": True})
        for bad in ({"gather": False}, {"scope": "approved"}, {"stopped": True}):
            writes = self.board_writes
            code, _, err = self.board_patch({"tracer": bad})
            self.assertEqual(code, 2, bad)
            self.assertIn('optionally with "gather": true; only factory.py sets tracer.feature, '
                          'tracer.url, tracer.scope, tracer.building, tracer.stopped and tracer.tasks', err)
            self.assertEqual(self.board_writes, writes)

    def test_gather_patch_starts_gather(self):
        with FakeTracer() as fake:
            self.gather(fake)
            posted = next(r[3] for r in fake.requests if r[0] == "POST" and r[1] == "/api/features")
            self.assertIs(posted["start_gather"], True)
            self.assertEqual(posted["ticket"], "FACTORY-1")
            self.assertEqual(posted["title"], "Export samples as CSV")
            self.assertEqual(posted["ticket_text"], "Export samples as CSV\nWith a header row.")
            self.assertEqual(posted["services"], [{"name": "sample", "language": "Python"}])
            self.assertEqual(posted["reading"], f"Gathered for factory job 001-sample: {API_URL}/#001-sample. "
                             "Approve or decline scope here. The factory builds this feature from its own spec "
                             "and stops any Tracer build that approval starts.")
            self.assertEqual(self.board["jobs"][0]["tracer"], {
                "follow": True, "gather": True, "feature": "factory-1",
                "url": f"{fake.url}/#/f/factory-1", "scope": "gathering"})

    def test_gathering_skips_task_mirror(self):
        with FakeTracer() as fake:
            self.gather(fake)
            (self.folder / "spec.md").write_text(SPEC)
            before = len(fake.requests)
            code, _, err = self.board_patch({"stage": "spec"})
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual([r[0] for r in fake.requests[before:]], ["GET"])
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual(out, "sample/001-sample: skipped: Tracer is gathering scope\n")
            self.assertFalse([r for r in fake.requests if r[0] == "PUT"])

    def test_follow_pass_starts_gather_at_idea(self):
        self.job["tracer"] = {"follow": True, "gather": True}
        pull = patch.object(factory, "pull_job").start()
        with FakeTracer() as fake:
            self.configure(fake)
            with contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_follow_pass(self.home)
            self.assertEqual((out.getvalue(), err.getvalue()), ("", ""))
            pull.assert_not_called()
            self.assertIs(fake.features["factory-1"]["start_gather"], True)
            self.assertEqual(self.board["jobs"][0]["tracer"]["scope"], "gathering")

    def test_scope_approval_starts_build(self):
        with FakeTracer() as fake:
            self.gather(fake)
            self.hold_at_gate()
            factory.tracer_follow_pass(self.home)
            self.assertEqual(self.board["jobs"][0]["note"], "gate: tracer-scope")
            fake.features["factory-1"]["scope_approved_at"] = "2026-01-02T00:00:00Z"
            with patch.object(factory, "now", return_value="2026-01-02T00:00:01Z"):
                factory.tracer_follow_pass(self.home)
            job = self.board["jobs"][0]
            self.assertEqual((job["stage"], job["note"], job["tracer"]["scope"]),
                             ("build", "spec approved in Tracer", "approved"))
            self.assertNotIn("needs", job)
            self.assertEqual((job["updated"], self.board["updated"]), ("2026-01-02T00:00:01Z",) * 2)
            calls = len(fake.requests)
            factory.tracer_follow_pass(self.home)
            self.assertEqual(len(fake.requests), calls)

    def test_scope_approval_before_gate_keeps_stage(self):
        with FakeTracer() as fake:
            self.gather(fake)
            factory.tracer_follow_pass(self.home)
            fake.features["factory-1"]["scope_approved_at"] = "2026-01-02T00:00:00Z"
            factory.tracer_follow_pass(self.home)
            job = self.board["jobs"][0]
            self.assertEqual((job["stage"], job["tracer"]["scope"]), ("idea", "approved"))
            self.assertNotIn("note", job)

    def test_scope_decline_releases_gate(self):
        with FakeTracer() as fake:
            self.gather(fake)
            self.hold_at_gate()
            fake.features["factory-1"]["closed"] = True
            factory.tracer_follow_pass(self.home)
            job = self.board["jobs"][0]
            self.assertEqual((job["note"], job["tracer"]["scope"]), ("gate: spec-approval", "declined"))
            self.assertEqual(job["needs"], "Tracer scope for factory-1 was declined: run /factory approve-spec "
                             "001-sample to build anyway, or close the job")

    def test_gate_patch_after_approval_starts_build(self):
        with FakeTracer() as fake:
            self.gather(fake)
            fake.features["factory-1"]["scope_approved_at"] = "2026-01-02T00:00:00Z"
            factory.tracer_follow_pass(self.home)
            for note in ("gate: spec-approval", "gate: tracer-scope"):
                with self.subTest(note=note):
                    self.board["jobs"][0]["stage"] = "spec"
                    code, out, err = self.board_patch({"stage": "needs-human", "note": note, "needs": "approve"})
                    self.assertEqual((code, err), (0, ""), err)
                    self.assertEqual(json.loads(out)["stage"], "build")
                    job = self.board["jobs"][0]
                    self.assertEqual((job["stage"], job["note"]), ("build", "spec approved in Tracer"))
                    self.assertNotIn("needs", job)
                    self.assertNotIn("claim", job)

    def test_gate_patch_without_approval_holds(self):
        for scope, feature in (("gathering", {}), ("declined", {"closed": True})):
            with self.subTest(scope=scope), FakeTracer() as fake:
                self.board["jobs"][0] = dict(self.job)
                self.gather(fake)
                fake.features["factory-1"].update(feature)
                factory.tracer_follow_pass(self.home)
                self.board["jobs"][0]["stage"] = "spec"
                code, _, err = self.board_patch(
                    {"stage": "needs-human", "note": "gate: spec-approval", "needs": "approve"})
                self.assertEqual((code, err), (0, ""), err)
                job = self.board["jobs"][0]
                self.assertEqual((job["stage"], job["note"], job["needs"], job["tracer"]["scope"]),
                                 ("needs-human", "gate: spec-approval", "approve", scope))

    def test_poll_ignores_a_board_feature_that_is_not_the_jobs(self):
        with FakeTracer() as fake:
            self.gather(fake)
            self.hold_at_gate()
            fake.features["factory-7"] = {"id": "factory-7", "ticket": "FACTORY-7", "tasks": [], "agents": [],
                                          "scope_approved_at": "2026-01-02T00:00:00Z"}
            self.board["jobs"][0]["tracer"]["feature"] = "factory-7"
            before = len(fake.requests)
            factory.tracer_follow_pass(self.home)
            self.assertEqual(fake.requests[before:], [])
            job = self.board["jobs"][0]
            self.assertEqual((job["note"], job["tracer"]["scope"]), ("gate: tracer-scope", "gathering"))

    def test_poll_refuses_a_feature_with_another_ticket(self):
        with FakeTracer() as fake:
            self.gather(fake)
            self.hold_at_gate()
            fake.features["factory-1"].update(ticket="FACTORY-7", scope_approved_at="2026-01-02T00:00:00Z")
            with contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_follow_pass(self.home)
            self.assertIn("Tracer feature factory-1 for job 001-sample belongs to FACTORY-7", err.getvalue())
            job = self.board["jobs"][0]
            self.assertEqual((job["note"], job["tracer"]["scope"]), ("gate: tracer-scope", "gathering"))
            fake.features["factory-1"]["ticket"] = None
            with contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_follow_pass(self.home)
            self.assertIn("belongs to no ticket", err.getvalue())

    def stops(self, fake):
        return [r for r in fake.requests if r[1] == "/api/features/factory-1/build-runs/stop"]

    def approve(self, fake):
        self.gather(fake)
        self.hold_at_gate()
        fake.features["factory-1"]["scope_approved_at"] = "2026-01-02T00:00:00Z"

    def test_scope_approval_stops_tracer_build(self):
        with FakeTracer() as fake:
            fake.stop_answer = (202, {"detail": "Stopped from Tracer."})
            self.approve(fake)
            factory.tracer_follow_pass(self.home)
            self.assertEqual([(r[0], r[3]) for r in self.stops(fake)], [("POST", {})])
            job = self.board["jobs"][0]
            self.assertEqual((job["stage"], job["tracer"]["scope"], job["tracer"]["stopped"]),
                             ("build", "approved", True))
            calls = len(fake.requests)
            factory.tracer_follow_pass(self.home)
            self.assertEqual(len(fake.requests), calls)

    def test_stop_retries_while_companion_starts(self):
        with FakeTracer() as fake:
            fake.stop_answer = (409, {"detail": "The companion is starting this build. Try again shortly."})
            self.approve(fake)
            with contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_follow_pass(self.home)
            self.assertEqual(err.getvalue(), "")
            self.assertEqual(len(self.stops(fake)), 1)
            self.assertNotIn("stopped", self.board["jobs"][0]["tracer"])
            fake.stop_answer = (202, {"detail": "Stopped from Tracer."})
            factory.tracer_follow_pass(self.home)
            self.assertEqual(len(self.stops(fake)), 2)
            self.assertIs(self.board["jobs"][0]["tracer"]["stopped"], True)

    def test_stop_done_when_nothing_to_stop(self):
        for answer in ((409, {"detail": "No build is running."}), (404, {"detail": "no feature"})):
            self.board = {"jobs": [dict(self.job)]}
            with FakeTracer() as fake:
                fake.stop_answer = answer
                self.approve(fake)
                factory.tracer_follow_pass(self.home)
                self.assertEqual(len(self.stops(fake)), 1, answer)
                self.assertIs(self.board["jobs"][0]["tracer"]["stopped"], True, answer)

    def test_stop_failure_warns(self):
        with FakeTracer() as fake:
            fake.stop_answer = (500, {"detail": "boom"})
            self.approve(fake)
            with contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_follow_pass(self.home)
            self.assertEqual(len(err.getvalue().splitlines()), 1)
            self.assertIn("Tracer build stop of sample/001-sample failed: Tracer POST "
                          "api/features/factory-1/build-runs/stop returned 500: boom", err.getvalue())
            self.assertIn(f"The board change was saved; {PICKUP_TAIL}", err.getvalue())
            self.assertNotIn("tracer sync", err.getvalue())
            job = self.board["jobs"][0]
            self.assertEqual(job["tracer"]["scope"], "approved")
            self.assertNotIn("stopped", job["tracer"])

    def test_patch_with_pending_stop_mirrors_stage(self):
        task = ("PUT", "/api/features/factory-1/tasks/t1")
        with FakeTracer() as fake:
            fake.stop_answer = (409, {"detail": "The companion is starting this build. Try again shortly."})
            self.approve(fake)
            factory.tracer_follow_pass(self.home)
            (self.folder / "spec.md").write_text(SPEC)
            fake.stop_answer = (500, {"detail": "boom"})
            code, _, err = self.board_patch({"stage": "review"})
            self.assertEqual(code, 0)
            self.assertEqual(len(err.splitlines()), 1)
            self.assertIn(f"The board change was saved; {PICKUP_TAIL}", err)
            self.assertNotIn(task, [r[:2] for r in fake.requests])
            fake.stop_answer = (202, {"detail": "Stopped from Tracer."})
            code, _, err = self.board_patch({"stage": "review"})
            self.assertEqual((code, err), (0, ""))
            self.assertEqual(len(self.stops(fake)), 3)
            self.assertIs(self.board["jobs"][0]["tracer"]["stopped"], True)
            sent = [r[:2] for r in fake.requests]
            last_stop = max(i for i, r in enumerate(sent) if r[1] == "/api/features/factory-1/build-runs/stop")
            self.assertIn(task, sent[last_stop + 1:])

    def test_patch_with_starting_stop_still_mirrors(self):
        task = ("PUT", "/api/features/factory-1/tasks/t1")
        with FakeTracer() as fake:
            fake.stop_answer = (409, {"detail": "The companion is starting this build. Try again shortly."})
            self.approve(fake)
            factory.tracer_follow_pass(self.home)
            (self.folder / "spec.md").write_text(SPEC)
            code, _, err = self.board_patch({"stage": "review"})
            self.assertEqual((code, err), (0, ""))
            self.assertEqual(len(self.stops(fake)), 2)
            self.assertNotIn("stopped", self.board["jobs"][0]["tracer"])
            sent = [r[:2] for r in fake.requests]
            last_stop = max(i for i, r in enumerate(sent) if r[1] == "/api/features/factory-1/build-runs/stop")
            self.assertIn(task, sent[last_stop + 1:])

    def test_gather_failure_names_the_step(self):
        with FakeTracer() as fake:
            self.configure(fake)
        code, _, err = self.board_patch({"tracer": {"follow": True, "gather": True}})
        self.assertEqual(code, 0)
        self.assertEqual(len(err.splitlines()), 1)
        self.assertTrue(err.startswith("factory.py: warning: Tracer gather of sample/001-sample failed: can't reach Tracer at "))
        self.assertIn(PICKUP_TAIL, err)
        self.assertNotIn("tracer sync", err)

    def test_ungathered_job_never_stops(self):
        self.job.update(stage="build", tracer={"follow": True})
        (self.folder / "spec.md").write_text(SPEC)
        with FakeTracer() as fake:
            self.configure(fake)
            factory.tracer_follow_pass(self.home)
            fake.features["factory-1"]["scope_approved_at"] = "2026-01-02T00:00:00Z"
            factory.tracer_follow_pass(self.home)
            self.assertEqual(self.board_patch({"stage": "review"})[0], 0)
            self.assertTrue(fake.requests)
            self.assertEqual(self.stops(fake), [])
            self.assertNotIn("stopped", self.board["jobs"][0]["tracer"])

    def test_gather_needs_follow(self):
        with FakeTracer() as fake:
            self.configure(fake)
            code, _, err = self.board_patch({"tracer": {"gather": True}, "stage": "idea"})
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual(fake.requests, [])

class MappingTest(TracerCase):
    def test_ticket_label(self):
        for value in (None, "", " \x1b\u202e "):
            self.assertEqual(factory.tracer_ticket_label(value), "no ticket")
        self.assertEqual(factory.tracer_ticket_label("PLAN-\x1b9\n\u202e"), "PLAN-9")
        self.assertEqual(factory.tracer_ticket_label(42), "42")

    def test_spec_tasks(self):
        tasks = factory.spec_tasks(SPEC)
        self.assertEqual([t["title"] for t in tasks], ["First step", "Second step", "Third step"])
        self.assertEqual([t["path"] for t in tasks], ["src/one.py", None, None])
        self.assertTrue(tasks[2]["what"].startswith("- Change: do three."))
        self.assertEqual(factory.spec_tasks("# T\n\n## 6. Tasks\n\n1. One\n2. Two\n"),
                         [{"title": "One", "what": "One", "path": None}, {"title": "Two", "what": "Two", "path": None}])
        self.assertEqual(len(factory.spec_tasks("## Tasks, in order\n\n1. One\n\n## Next\n\n1. Not a task\n")), 1)
        self.assertEqual(factory.spec_tasks("## Goal\n\nNo tasks here.\n"), [])
        self.assertEqual(factory.spec_tasks("## Tasks\n\n1. `*`\n")[0]["title"], "Step")
        self.assertEqual(len(factory.spec_tasks("1. x\n## Tasks\n\n1. y" + "\n2. z" * 3)), 4)

    def test_stage_statuses(self):
        def statuses(stage, count=3, existing=(), **extra):
            return factory.task_statuses(dict(self.job, stage=stage, **extra), self.folder, count, list(existing))
        self.assertEqual(statuses("spec"), ["not_started"] * 3)
        self.assertEqual(statuses("build"), ["in_progress", "not_started", "not_started"])
        self.assertEqual(statuses("build", existing=["done", "done", None]), ["done", "done", "in_progress"])
        self.assertEqual(statuses("build", existing=["done", "in_progress", "done"]), ["done", "in_progress", "done"])
        self.assertEqual(statuses("review"), ["verifying"] * 3)
        self.assertEqual(statuses("approve"), ["verifying"] * 3)
        self.assertEqual(statuses("needs-human"), ["blocked"] * 3)
        self.assertEqual(statuses("pr-open"), ["verifying"] * 3)
        self.assertEqual(statuses("pr-open", escalated=True), ["blocked"] * 3)
        self.assertEqual(statuses("merged"), ["done"] * 3)
        for stage in ("idea", "closed", None):
            self.assertIsNone(statuses(stage))
        (self.folder / "round-1").mkdir()
        self.assertEqual(statuses("build", existing=["done", None, None]), ["in_progress"] * 3)

    def test_tracer_ids(self):
        self.assertEqual(factory.tracer_ids({"id": "001-a", "jira": "plan-12"}), ("PLAN-12", "plan-12"))
        for jira in (None, "", "PLAN-0", "12-34", "PLAN-1 x"):
            self.assertEqual(factory.tracer_ids({"id": "001-a", "jira": jira}), ("FACTORY-1", "factory-1"))
        self.assertEqual(factory.tracer_ids({"id": "042-sample-export"}), ("FACTORY-42", "factory-42"))
        self.assertEqual(factory.tracer_ids({"id": "abc"}), ("ABC", "abc"))


class MirrorTest(TracerCase):
    ALL_PASS = {"security": "PASS", "ux": "PASS", "ui": "PASS", "code": "PASS"}

    def test_review_rounds_post_once(self):
        self.job["stage"] = "review"
        self.review(1, {"security": "PASS", "ux": "CHANGES", "ui": "PASS", "code": "PASS"})
        self.review(2, dict(self.ALL_PASS, ux="SKIPPED", ui="SKIPPED"))
        (self.folder / "round-3").mkdir()
        (self.folder / "round-3" / "review-security.md").write_text("VERDICT: PASS\n")
        self.review(4, dict(self.ALL_PASS, code="MAYBE"))
        self.assertEqual([n for n, _ in factory.review_rounds(self.folder)], [1, 2])
        with FakeTracer() as fake:
            self.configure(fake)
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertIn("2 review rounds posted", out)
            runs = next(t for t in fake.features["plan-1234"]["tasks"] if t["id"] == "t3")["test_runs"]
            self.assertEqual(runs[0], {
                "status": "failed", "passed": 3, "failed": 1, "failures": ["ux review (round 1)"],
                "output": f"factory review round 1\nsecurity PASS · ux CHANGES · ui PASS · code PASS\n"
                          f"{API_URL}/#001-sample/round-1/review-ux", "id": "r1"})
            self.assertEqual((runs[1]["status"], runs[1]["passed"], runs[1]["failed"], runs[1]["failures"]),
                             ("passed", 4, 0, []))
            self.assertIn("security PASS · ux SKIPPED · ui SKIPPED · code PASS", runs[1]["output"])
            self.assertEqual(len(runs), 2)
            self.review(3, self.ALL_PASS)
            before = len(fake.requests)
            self.assertIn("1 review rounds posted", self.sync("sample/001-sample")[1])
            self.assertIn("0 review rounds posted", self.sync("sample/001-sample")[1])
            self.assertEqual(len([r for r in fake.requests[before:] if r[0] == "POST"]), 1)

    def test_a_spec_with_no_tasks_posts_no_runs(self):
        (self.folder / "spec.md").write_text("# Spec: none\n")
        self.job["stage"] = "review"
        self.review(1, self.ALL_PASS)
        with FakeTracer() as fake:
            self.configure(fake)
            self.assertIn("0 tasks, 0 review rounds posted", self.sync("sample/001-sample")[1])
            self.assertFalse([r for r in fake.requests if r[0] == "POST" and "test-results" in r[1]])

    def test_feature_resolution(self):
        with FakeTracer() as fake:
            self.configure(fake)
            # Made earlier under the job id, with our ticket: reused.
            fake.features["001-sample"] = {"id": "001-sample", "ticket": "PLAN-1234", "tasks": [], "agents": []}
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertIn("as Tracer feature 001-sample:", out)
            self.assertNotIn("plan-1234", fake.features)
            self.assertEqual(len(fake.features["001-sample"]["tasks"]), 3)
        with FakeTracer() as fake:
            self.configure(fake)
            fake.features["plan-1234"] = {"id": "plan-1234", "ticket": "PLAN-9999", "tasks": [], "agents": []}
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual(code, 5)
            self.assertIn("Tracer feature plan-1234 for job 001-sample belongs to PLAN-9999", err)
        with FakeTracer() as fake:
            self.configure(fake)
            fake.features["plan-1234"] = {"id": "plan-1234", "ticket": "PLAN-99\x1b\u202e99", "tasks": [], "agents": []}
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual(code, 5)
            self.assertIn("belongs to PLAN-9999", err)
            self.assertIsNone(factory.NOTE_INVISIBLE.search(err))
        with FakeTracer() as fake:
            self.configure(fake)
            fake.features["plan-1234"] = {"id": "plan-1234", "ticket": "", "tasks": [], "agents": []}
            self.assertEqual(self.sync("sample/001-sample")[0], 0)
            self.assertEqual(fake.features["plan-1234"]["ticket"], "PLAN-1234")
        with FakeTracer() as fake:
            self.configure(fake)
            del self.board["jobs"][0]["jira"]
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual(fake.features["factory-1"]["ticket"], "FACTORY-1")
            self.assertIn("FACTORY-1 as Tracer feature factory-1", out)

    def test_title_falls_back_to_spec_heading_then_id(self):
        with FakeTracer() as fake:
            self.configure(fake)
            del self.job["title"]
            self.sync("sample/001-sample")
            self.assertEqual(fake.features["plan-1234"]["title"], "Sample feature")

    def test_scope_lock_is_not_an_error(self):
        with FakeTracer() as fake:
            fake.scope_locked = True
            self.configure(fake)
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertIn("; tasks unassigned until scope is approved in Tracer", out)
            self.assertEqual(len(fake.features["plan-1234"]["tasks"]), 3)
            self.assertEqual({t["agent"] for t in fake.features["plan-1234"]["tasks"]}, {None})

    def test_tracer_build_keeps_its_tasks(self):
        self.job["stage"] = "build"
        suffix = "; tasks left with the Tracer build agent"
        with FakeTracer() as fake:
            self.configure(fake)
            fake.features["plan-1234"] = {
                "id": "plan-1234", "ticket": "PLAN-1234", "stage": "build",
                "agents": [{"id": "tracer-build-1", "stage": "build", "kind": "session"}],
                "tasks": [{"id": f"t{n}", "agent": "tracer-build-1", "test_runs": []} for n in (1, 2, 3)]}
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertIn(suffix, out)
            self.assertFalse([r for r in fake.requests if r[0] == "PATCH"])
            self.assertEqual({t["agent"] for t in fake.features["plan-1234"]["tasks"]}, {"tracer-build-1"})
            fake.features["plan-1234"]["stage"] = "verify"
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertNotIn(suffix, out)
            self.assertEqual({t["agent"] for t in fake.features["plan-1234"]["tasks"]}, {"factory-builder"})

    def test_tracer_build_keeps_its_statuses(self):
        self.job["stage"] = "build"
        with FakeTracer() as fake:
            self.configure(fake)
            fake.features["plan-1234"] = {
                "id": "plan-1234", "ticket": "PLAN-1234", "stage": "build",
                "agents": [{"id": "tracer-build-1", "stage": "build", "kind": "session"}],
                "tasks": [{"id": f"t{n}", "status": s, "agent": "tracer-build-1", "test_runs": []}
                          for n, s in ((1, "done"), (2, "verifying"))]}

            def statuses():
                return {t["id"]: t["status"] for t in fake.features["plan-1234"]["tasks"]}

            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual(statuses(), {"t1": "done", "t2": "verifying", "t3": "not_started"})
            next(t for t in fake.features["plan-1234"]["tasks"] if t["id"] == "t3")["status"] = "in_progress"
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual(statuses(), {"t1": "done", "t2": "verifying", "t3": "in_progress"})
            fake.features["plan-1234"]["stage"] = "verify"
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual(statuses(), {"t1": "done", "t2": "in_progress", "t3": "not_started"})

    def test_rerun_assigns_nothing_new_and_keeps_done_tasks(self):
        self.job["stage"] = "build"
        with FakeTracer() as fake:
            self.configure(fake)
            self.sync("sample/001-sample")
            tasks = fake.features["plan-1234"]["tasks"]
            self.assertEqual([t["status"] for t in sorted(tasks, key=lambda t: t["id"])],
                             ["in_progress", "not_started", "not_started"])
            tasks[0]["status"] = "done"
            before = len(fake.requests)
            self.sync("sample/001-sample")
            self.assertFalse([r for r in fake.requests[before:] if r[0] == "PATCH"])
            statuses = {t["id"]: t["status"] for t in fake.features["plan-1234"]["tasks"]}
            self.assertEqual(statuses, {"t1": "done", "t2": "in_progress", "t3": "not_started"})


class CommandTest(TracerCase):
    def setUp(self):
        super().setUp()
        (self.folder.parent / "002-idea").mkdir()
        self.board["jobs"] += [
            {"id": "002-idea", "project": "sample", "stage": "idea"},
            {"id": "003-merged", "project": "sample", "stage": "merged", "jira": "PLAN-5"},
            {"id": "004-nospec", "project": "sample", "stage": "build"},
            {"id": "005-other", "project": "other", "stage": "build"}]
        (self.folder.parent / "003-merged").mkdir()
        (self.folder.parent / "003-merged" / "spec.md").write_text("## Tasks\n\n1. Only\n")

    def test_sync_targets(self):
        with FakeTracer() as fake:
            self.configure(fake)
            code, out, err = self.sync()
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual([line.split(":")[0] for line in out.splitlines()], ["sample/001-sample"])
            self.assertEqual(sorted(fake.features), ["plan-1234"])
            code, out, _ = self.sync("sample")
            self.assertEqual([line.split(":")[0] for line in out.splitlines()], ["sample/001-sample"])
            code, out, err = self.sync("sample/003-merged")
            self.assertEqual((code, err), (0, ""), err)
            self.assertIn("sample/003-merged: PLAN-5 as Tracer feature plan-5: stage merged, 1 tasks", out)
            self.assertEqual(fake.features["plan-5"]["tasks"][0]["status"], "done")
            code, out, _ = self.sync("sample/002-idea")
            self.assertEqual((code, out.count("skipped")), (0, 1))
            code, out, _ = self.sync("sample/004-nospec")
            self.assertEqual((code, out), (0, "sample/004-nospec: skipped: no spec.md\n"))
            self.assertEqual(self.sync("sample/009-missing")[0], 4)
            self.assertEqual(self.sync("other")[0], 2)
            self.assertEqual(self.sync("other/005-other")[0], 2)
            self.assertEqual(self.sync("bad slug/x")[0], 2)
            self.assertEqual(invoke(factory, ["--home", str(self.home), "tracer", "nope"])[0], 2)
            self.assertEqual(invoke(factory, ["--home", str(self.home), "tracer"])[0], 2)

    def test_sync_exits_5_on_the_first_tracer_failure(self):
        with FakeTracer(token="another-token") as fake:
            self.configure(fake)
            code, out, err = self.sync()
            self.assertEqual((code, out), (5, ""))
            self.assertEqual(len(fake.requests), 1)

    def test_token_never_printed(self):
        with FakeTracer(token="another-token") as fake:
            self.configure(fake)
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual(code, 5)
            self.assertIn("returned 401", err)
            self.assertIn("check token_file", err)
            code2, out2, err2 = self.board_patch({"stage": "build"})
            self.assertEqual(code2, 0)
            for text in (out, err, out2, err2):
                self.assertNotIn(TOKEN, text)
        # Even a server that echoes the token in its detail does not get it printed.
        with FakeTracer() as fake:
            self.configure(fake)
            fake.answer = lambda *args: (500, {"detail": f"boom {TOKEN} boom"})
            code, out, err = self.sync("sample/001-sample")
            self.assertEqual(code, 5)
            self.assertIn("returned 500", err)
            self.assertNotIn(TOKEN, out + err)

    def test_redirects_are_not_followed(self):
        with FakeTracer() as fake:
            self.configure(fake)
            fake.answer = lambda *args: (302, {"detail": "elsewhere"})
            self.assertEqual(self.sync("sample/001-sample")[0], 5)
            self.assertEqual(len(fake.requests), 1)

    def test_environment_proxies_are_ignored(self):
        with FakeTracer() as fake:
            self.configure(fake)
            with patch.dict(os.environ, {"http_proxy": "http://127.0.0.1:1", "HTTP_PROXY": "http://127.0.0.1:1",
                                         "no_proxy": "", "NO_PROXY": ""}):
                code, out, err = self.sync("sample/001-sample")
            self.assertEqual((code, err), (0, ""), err)
            self.assertTrue(fake.requests)

    def test_http_client_errors_exit_5(self):
        with patch.object(factory.urllib.request.OpenerDirector, "open",
                          side_effect=factory.http.client.RemoteDisconnected("closed")):
            with self.assertRaises(SystemExit) as raised, contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_call("http://127.0.0.1:1", TOKEN, "GET", "api/features")
        self.assertEqual(raised.exception.code, 5)
        self.assertIn("can't reach Tracer", err.getvalue())

    def test_non_object_feature_body_exits_5(self):
        with FakeTracer() as fake:
            self.configure(fake)
            fake.answer = lambda *args: (200, ["not", "a", "feature"])
            code, _, err = self.sync("sample/001-sample")
            self.assertEqual(code, 5)
            self.assertIn("not a feature", err)

    def test_empty_bulk_sync_prints_one_line(self):
        with FakeTracer() as fake:
            self.configure(fake)
            self.job["stage"] = "idea"
            code, out, err = self.sync()
            self.assertEqual((code, len(out.splitlines())), (0, 1))
            self.assertIn("nothing to mirror", out)

    def test_client_error_shapes(self):
        base = "http://127.0.0.1:1"
        with self.assertRaises(SystemExit) as raised, contextlib.redirect_stderr(io.StringIO()) as err:
            factory.tracer_call(base, TOKEN, "GET", "api/features")
        self.assertEqual(raised.exception.code, 5)
        self.assertIn("can't reach Tracer at http://127.0.0.1:1", err.getvalue())
        self.assertNotIn(TOKEN, err.getvalue())


class BuildPassTest(TracerCase):
    def setUp(self):
        super().setUp()
        self.job["stage"] = "idea"
        self.agent = {"id": "tracer-build-1", "stage": "build", "kind": "session"}

    @property
    def stored(self):
        return self.board["jobs"][0]

    def feature(self, **change):
        feature = {"id": "plan-1234", "ticket": "PLAN-1234", "stage": "build", "tasks": [],
                   "agents": [self.agent, {"id": "factory-builder", "stage": "build", "kind": "session"}]}
        return dict(feature, **change)

    def run_pass(self, fake):
        self.configure(fake)
        with contextlib.redirect_stderr(io.StringIO()) as err:
            factory.tracer_build_pass(self.home)
        return err.getvalue()

    def test_build_pass_marks_and_clears(self):
        with FakeTracer() as fake:
            fake.features["plan-1234"] = self.feature()
            self.assertEqual(self.run_pass(fake), "")
            self.assertEqual(self.stored["tracer"], {"building": True, "tasks": []})
            self.assertEqual(self.board_writes, 1)
            self.run_pass(fake)
            self.assertEqual(self.board_writes, 1)
            self.stored["tracer"] = {"follow": True, "building": True}
            fake.features["plan-1234"]["agents"] = [{"id": "factory-builder", "stage": "build", "kind": "session"}]
            self.run_pass(fake)
            self.assertEqual(self.stored["tracer"], {"follow": True})
            self.assertEqual(self.board_writes, 2)
            self.stored["tracer"] = {"building": True}
            self.assertTrue(factory.tracer_build_pass(self.home))
            self.assertNotIn("tracer", self.stored)

    def test_build_pass_without_tracer_json_keeps_mark_while_on(self):
        self.stored["tracer"] = {"follow": True, "building": True}
        with FakeTracer() as fake:
            self.configure(fake)
            (self.home / "tracer.json").unlink()
            self.assertIsNone(factory.tracer_build_pass(self.home))
            self.assertEqual(self.stored["tracer"], {"follow": True, "building": True})
            self.assertEqual(self.board_writes, 0)
            self.assertEqual(fake.requests, [])

    def test_build_pass_not_building(self):
        with FakeTracer() as fake:
            for name, feature in (("gather", self.feature(stage="gather")),
                                  ("closed", self.feature(closed=True)),
                                  ("run agent", self.feature(agents=[dict(self.agent, kind="run")])),
                                  ("404", None)):
                with self.subTest(name):
                    fake.features.clear()
                    if feature:
                        fake.features["plan-1234"] = feature
                    self.assertEqual(self.run_pass(fake), "")
                    self.assertNotIn("tracer", self.stored)
                    self.assertEqual(self.board_writes, 0)

    TASKS = [{"id": "t1", "title": "Add export button", "status": "done"},
             {"id": "t2", "title": "Write CSV encoder", "status": "in_progress"},
             {"id": "t3", "title": "Document export", "status": "not_started"}]

    def test_build_pass_saves_tasks(self):
        with FakeTracer() as fake:
            fake.features["plan-1234"] = self.feature(tasks=[dict(t, kind="code") for t in self.TASKS])
            self.assertEqual(self.run_pass(fake), "")
            self.assertEqual(self.stored["tracer"], {"building": True, "tasks": self.TASKS})
            self.assertEqual(self.board_writes, 1)
            self.run_pass(fake)
            self.assertEqual(self.board_writes, 1)

    def test_build_pass_refreshes_and_drops_tasks(self):
        with FakeTracer() as fake:
            fake.features["plan-1234"] = self.feature(tasks=[dict(t) for t in self.TASKS])
            self.run_pass(fake)
            feature = fake.features["plan-1234"]
            feature["agents"] = []
            feature["tasks"][1]["status"] = "done"
            self.assertEqual(self.run_pass(fake), "")
            done = [self.TASKS[0], dict(self.TASKS[1], status="done"), self.TASKS[2]]
            self.assertEqual(self.stored["tracer"], {"tasks": done})
            self.assertEqual(self.board_writes, 2)
            self.run_pass(fake)
            self.assertEqual(self.board_writes, 2)
            fake.features.clear()
            self.assertEqual(self.run_pass(fake), "")
            self.assertNotIn("tracer", self.stored)
            self.assertEqual(self.board_writes, 3)

    def test_task_summary(self):
        summary = factory.tracer_task_summary
        self.assertEqual(summary({}), [])
        self.assertEqual(summary({"tasks": "t1"}), [])
        self.assertEqual(summary({"tasks": [
            "t1", {"id": "t2", "status": "Done!"}, {"id": 3, "status": "done"},
            {"id": "bad id", "status": "done"}, {"id": "t4", "status": "done"},
            {"id": "t5", "title": "   ", "status": "done"}, {"id": "t6", "title": "x" * 100, "status": "done"},
            {"id": "t7", "title": "  Ship it  ", "status": "in_progress"}]}),
            [{"id": "t4", "title": "t4", "status": "done"}, {"id": "t5", "title": "t5", "status": "done"},
             {"id": "t6", "title": "x" * 80, "status": "done"},
             {"id": "t7", "title": "Ship it", "status": "in_progress"}])
        many = summary({"tasks": [{"id": f"t{n}", "title": "T", "status": "done"} for n in range(35)]})
        self.assertEqual([t["id"] for t in many], [f"t{n}" for n in range(30)])

    def test_task_summary_strips_bidi(self):
        self.assertEqual(factory.tracer_task_summary({"tasks": [
            {"id": "t2", "title": "Write CSV \u202eencoder\u2069", "status": "in_progress"},
            {"id": "t9", "title": "\u200f\u200e", "status": "done"},
            {"id": "t10", "title": "\u2066" * 10 + "x" * 80, "status": "done"}]}),
            [{"id": "t2", "title": "Write CSV encoder", "status": "in_progress"},
             {"id": "t9", "title": "t9", "status": "done"},
             {"id": "t10", "title": "x" * 80, "status": "done"}])

    def test_build_pass_tracer_down_keeps_board(self):
        with FakeTracer() as fake:
            self.configure(fake)
        self.stored["tracer"] = {"building": True}
        with contextlib.redirect_stderr(io.StringIO()) as err:
            factory.tracer_build_pass(self.home)
        lines = err.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        self.assertIn("warning: Tracer build check failed", lines[0])
        self.assertEqual(self.stored["tracer"], {"building": True})
        self.assertEqual(self.board_writes, 0)

    def test_build_pass_ignores_a_board_feature_that_is_not_the_jobs(self):
        with FakeTracer() as fake:
            fake.features["factory-7"] = self.feature(id="factory-7")
            self.stored["tracer"] = {"feature": "factory-7", "tasks": []}
            self.assertEqual(self.run_pass(fake), "")
            self.assertEqual(fake.requests, [])
            self.assertEqual(self.board_writes, 0)
            self.assertEqual(self.stored["tracer"], {"feature": "factory-7", "tasks": []})
            fake.features["plan-1234"] = self.feature()
            self.stored["tracer"] = {"feature": "plan-1234"}
            self.run_pass(fake)
            self.assertIs(self.stored["tracer"]["building"], True)

    def test_build_pass_refuses_a_feature_with_another_ticket(self):
        with FakeTracer() as fake:
            fake.features["plan-1234"] = self.feature(ticket="PLAN-9")
            self.stored["tracer"] = {"building": True}
            lines = self.run_pass(fake).splitlines()
            self.assertEqual(len(lines), 1)
            self.assertIn("Tracer build check failed: Tracer feature plan-1234 for job 001-sample belongs to PLAN-9.",
                          lines[0])
            self.assertEqual(self.stored["tracer"], {"building": True})
            self.assertEqual(self.board_writes, 0)
            fake.answer = lambda *args: (200, ["not", "a", "feature"])
            lines = self.run_pass(fake).splitlines()
            self.assertEqual(len(lines), 1)
            self.assertIn("not a feature", lines[0])

    def test_build_pass_cleans_tracer_ticket_text(self):
        for ticket, label in ((None, "belongs to no ticket."), ("PLAN-\x1b9\u202e", "belongs to PLAN-9.")):
            with FakeTracer() as fake:
                fake.features["plan-1234"] = self.feature(ticket=ticket)
                lines = self.run_pass(fake).splitlines()
                self.assertEqual(len(lines), 1)
                self.assertIn(label, lines[0])
                self.assertIsNone(factory.NOTE_INVISIBLE.search(lines[0]))

    def test_build_pass_quotes_feature_id(self):
        patch.object(factory, "tracer_ids", return_value=("PLAN-1234", "plan 1234/x")).start()
        with FakeTracer() as fake:
            fake.features["plan%201234%2Fx"] = self.feature()
            self.assertEqual(self.run_pass(fake), "")
            self.assertEqual(fake.requests[0][1], "/api/features/plan%201234%2Fx")
            self.assertIs(self.stored["tracer"]["building"], True)

    def test_build_pass_clears_out_of_scope_mark(self):
        for name, change, tracer, left in (
                ("pr-open", {"stage": "pr-open"}, {"building": True, "tasks": self.TASKS}, {"tasks": self.TASKS}),
                ("other project", {"project": "other"}, {"building": True}, None)):
            with self.subTest(name), FakeTracer() as fake:
                self.board = {"jobs": [dict(self.job, tracer=tracer, **change)]}
                self.board_writes = 0
                self.assertEqual(self.run_pass(fake), "")
                self.assertEqual(self.stored.get("tracer"), left)
                self.assertEqual(self.board_writes, 1)
                self.assertEqual(fake.requests, [])


class OffTest(TracerCase):
    def setUp(self):
        super().setUp()
        self.pipeline = {"schema": 1, "tracer": {"enabled": False}}

    def test_unset_setting_is_off(self):
        for pipeline in (None, {"schema": 1}):
            with self.subTest(pipeline=pipeline):
                self.pipeline = pipeline
                self.assertIs(factory.tracer_enabled(self.home), False)
        self.pipeline = {"schema": 1, "tracer": {"enabled": True}}
        self.assertIs(factory.tracer_enabled(self.home), True)

    def test_off_board_patch_makes_no_tracer_call(self):
        with FakeTracer() as fake:
            self.configure(fake)
            code, out, err = self.board_patch({"stage": "build", "tracer": {"follow": True}})
            self.assertEqual((code, err), (0, ""), err)
            self.assertEqual(self.board["jobs"][0]["tracer"], {"follow": True})
            self.assertEqual(fake.requests, [])

    def test_off_follow_pass_and_sync(self):
        self.job.update(stage="build", tracer={"follow": True})
        with FakeTracer() as fake:
            self.configure(fake)
            with contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_follow_pass(self.home)
            self.assertEqual((out.getvalue(), err.getvalue()), ("", ""))
            for target in ((), ("sample",), ("sample/001-sample",)):
                code, out, err = self.sync(*target)
                self.assertEqual((code, out, err), (0, "nothing to mirror: Tracer is off in Settings, Pipeline\n", ""))
            self.assertEqual(self.board_writes, 0)
            self.assertEqual(fake.requests, [])

    def test_off_build_pass_clears_mark(self):
        self.job.update(stage="idea", tracer={"follow": True, "building": True})
        with FakeTracer() as fake:
            fake.features["plan-1234"] = {"id": "plan-1234", "ticket": "PLAN-1234", "stage": "build", "tasks": [],
                                          "agents": [{"id": "tracer-build-1", "stage": "build", "kind": "session"}]}
            self.configure(fake)
            with contextlib.redirect_stderr(io.StringIO()) as err:
                factory.tracer_build_pass(self.home)
            self.assertEqual(err.getvalue(), "")
            self.assertEqual(self.board["jobs"][0]["tracer"], {"follow": True})
            self.assertEqual(self.board_writes, 1)
            self.assertEqual(fake.requests, [])

    def test_off_build_pass_without_tracer_json_clears_mark(self):
        self.job.update(stage="idea", tracer={"follow": True, "building": True})
        with FakeTracer() as fake:
            self.configure(fake)
            (self.home / "tracer.json").unlink()
            self.assertTrue(factory.tracer_build_pass(self.home))
            self.assertEqual(self.board["jobs"][0]["tracer"], {"follow": True})
            self.assertEqual(self.board_writes, 1)
            self.assertEqual(fake.requests, [])


if __name__ == "__main__":
    unittest.main()
