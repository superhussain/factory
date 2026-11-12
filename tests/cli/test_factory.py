import ast
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import ssl
import stat
import subprocess
import tempfile
import types
import unittest
import urllib.error
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import MagicMock, patch


from support import ROOT, IsolatedTestCase, invoke

SKILL_ROOT = Path(__file__).resolve().parents[2] / "skills" / "factory"
spec = importlib.util.spec_from_file_location("factory", SKILL_ROOT / "runtime" / "factory.py")
factory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(factory)


class ProjectSyncTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)
        self.document, self.state, self.remote = factory.project_files(self.home, "sample")
        self.base = b"---\nrepo: example/sample\nbase: main\ntitle: Sample\n---\nBody\n"
        self.data = self.base
        self.calls = []
        self.during_get = self.during_put = None
        self.error = None
        self.no_etag = False
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "api", side_effect=self.api).start()
        self.output = io.StringIO()
        redirect = contextlib.redirect_stdout(self.output)
        redirect.__enter__()
        self.addCleanup(redirect.__exit__, None, None, None)

    def tag(self):
        return '"' + factory.sha(self.data) + '"' if self.data is not None else None

    def api(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        self.calls.append((method, path, body, headers))
        if self.error:
            raise SystemExit(self.error)
        if path == "/projects.json":
            return 200, {}, '[{"slug":"sample","repo":"example/sample","title":"Sample"}]'
        if method == "GET":
            data, tag = self.data, self.tag()
            if self.during_get:
                self.during_get()
            status = 404 if data is None else 200
            if status not in ok:
                raise SystemExit(5)
            return status, {} if self.no_etag else {"ETag": tag}, data if raw else (data or b"").decode()
        self.assertEqual(headers.get("Content-Type"), "text/markdown; charset=utf-8")
        if self.during_put:
            self.during_put()
        if (headers.get("If-None-Match") == "*" and self.data is not None
                or "If-Match" in headers and (self.data is None or headers["If-Match"] != self.tag())):
            return 412, {}, ""
        self.data = body.encode() if isinstance(body, str) else body
        return 200, {} if self.no_etag else {"ETag": self.tag()}, "ok"

    def refused(self, call, code=6):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            call()
        self.assertEqual(raised.exception.code, code)

    def pull(self):
        return factory.pull_project(self.home, "sample")

    def push(self):
        return factory.push_project(self.home, "sample")

    def saved(self):
        return json.loads(self.state.read_text())

    def test_missing_equal_and_clean_refresh(self):
        self.assertFalse(self.pull())
        self.assertEqual(self.document.read_bytes(), self.base)
        self.assertEqual(self.saved(), {"version": 1, "etag": self.tag(), "sha256": factory.sha(self.base)})
        self.state.unlink()
        self.assertFalse(self.pull())
        self.data += b"Remote"
        self.assertFalse(self.pull())
        self.assertEqual(self.document.read_bytes(), self.data)
        self.assertEqual(self.saved()["sha256"], factory.sha(self.data))

    def test_dirty_pull_where_and_repeated_init_keep_draft_and_paths(self):
        self.pull()
        old = self.state.read_bytes()
        draft = self.base + b"Draft"
        self.document.write_bytes(draft)
        (self.home / "config.md").write_text("---\napi_url: http://localhost:8200\n---\n")
        (self.home / "paths.md").write_text(f"---\nsample: {self.home / 'checkout'}\n---\n")
        paths = (self.home / "paths.md").read_bytes()
        self.assertTrue(self.pull())
        self.refused(lambda: factory.where("sample"))
        self.refused(self.init)
        self.assertEqual(self.document.read_bytes(), draft)
        self.assertEqual(self.state.read_bytes(), old)
        self.assertEqual((self.home / "paths.md").read_bytes(), paths)
        self.assertFalse(any(c[0] == "PUT" for c in self.calls))

    def init(self):
        if not (self.home / "config.md").exists():
            (self.home / "config.md").write_text("---\napi_url: https://example.invalid\n---\n")
        with patch.object(factory, "install_agents"), patch.object(factory, "main_checkout", return_value=self.home / "sample"), patch.object(factory, "remotes", return_value=["example/sample"]), patch.object(factory, "read_conf", return_value={"FACTORY_ACCESS_CLIENT_ID":"test", "FACTORY_ACCESS_CLIENT_SECRET":"test"}):
            factory.init(str(self.home / "sample"))

    def test_unversioned_divergence_and_marker_resolution(self):
        draft = self.base + b"Legacy"
        self.document.write_bytes(draft)
        self.refused(self.pull)
        self.assertEqual(self.document.read_bytes(), draft)
        self.assertEqual(self.remote.read_bytes(), self.base)
        self.assertEqual(self.saved()["etag"], self.tag())
        self.refused(self.push)
        self.data += b"New remote"
        self.refused(self.pull)
        self.assertEqual(self.document.read_bytes(), draft)
        self.assertEqual(self.remote.read_bytes(), self.data)
        self.remote.unlink()
        compared = self.tag()
        self.push()
        self.assertEqual(self.calls[-1][3]["If-Match"], compared)
        self.assertEqual(self.data, draft)

    def test_both_changed_stale_push_never_gets_or_retries(self):
        self.pull()
        baseline = self.state.read_bytes()
        draft = self.base + b"Local"
        self.document.write_bytes(draft)
        self.data += b"Elsewhere"
        self.calls.clear()
        self.refused(self.push)
        self.assertEqual([c[0] for c in self.calls], ["PUT"])
        self.assertEqual(self.document.read_bytes(), draft)
        self.assertEqual(self.state.read_bytes(), baseline)
        self.refused(self.pull)
        self.assertEqual(self.remote.read_bytes(), self.data)

    def test_no_base_and_corrupt_state_fail_closed(self):
        self.document.write_bytes(self.base)
        self.refused(self.push)
        for value in ['{}','bad','{"version":true,"etag":"x","sha256":"x"}', json.dumps({"version":1,"etag":'W/"a"',"sha256":"0"*64})]:
            self.state.write_text(value)
            self.refused(self.push)
            self.refused(self.pull)
        self.assertEqual(self.calls, [])
        self.assertEqual(self.document.read_bytes(), self.base)

    def test_local_edit_during_get_and_put(self):
        self.pull()
        before = self.state.read_bytes()
        self.data += b"Remote"
        draft = self.base + b"Typing"
        self.during_get = lambda: self.document.write_bytes(draft)
        self.refused(self.pull)
        self.assertEqual(self.document.read_bytes(), draft)
        self.assertEqual(self.state.read_bytes(), before)
        self.during_get = None
        self.data = self.base
        self.during_put = lambda: self.document.write_bytes(draft + b"More")
        self.push()
        self.assertEqual(self.data, draft)
        self.assertEqual(self.document.read_bytes(), draft + b"More")
        self.assertEqual(self.saved()["sha256"], factory.sha(draft))

    def test_error_paths_preserve_draft_and_state(self):
        self.pull()
        old = self.state.read_bytes()
        draft = self.base + b"Draft"
        self.document.write_bytes(draft)
        for data, error, no_etag in [(None,None,False),(b"\xff",None,False),(self.base,5,False),(self.base,None,True)]:
            self.data, self.error, self.no_etag = data, error, no_etag
            self.refused(self.pull, 5)
            self.assertEqual(self.document.read_bytes(), draft)
            self.assertEqual(self.state.read_bytes(), old)
        self.data, self.error, self.no_etag = self.base, None, True
        self.refused(self.push, 5)
        self.assertEqual(self.document.read_bytes(), draft)
        self.assertEqual(self.state.read_bytes(), old)

    def test_path_stripping_records_uploaded_snapshot_only(self):
        self.pull()
        legacy = self.base.replace(b"base: main", f"path: {self.home / 'device-only'}\nbase: main".encode())
        self.document.write_bytes(legacy)
        self.push()
        self.assertEqual(self.data, self.base)
        self.assertEqual(self.document.read_bytes(), self.base)
        self.document.write_bytes(legacy)
        self.during_put = lambda: self.document.write_bytes(legacy + b"Typing")
        self.push()
        self.assertEqual(self.document.read_bytes(), legacy + b"Typing")
        self.assertEqual(self.saved()["sha256"], factory.sha(self.base))

    def test_project_cli_and_successful_where_fields(self):
        factory.pull("sample")
        self.assertIn("pulled project sample", self.output.getvalue())
        self.document.write_bytes(self.base + b"Edit")
        factory.push("sample")
        self.assertEqual(self.data, self.base + b"Edit")
        (self.home / "config.md").write_text("---\napi_url: http://localhost:8200\n---\n")
        (self.home / "paths.md").write_text(f"---\nsample: {self.home / 'checkout'}\n---\n")
        self.output.seek(0)
        self.output.truncate()
        factory.where("sample")
        got = json.loads(self.output.getvalue())
        self.assertEqual(set(got), {"factory","runtime","project_file","repo_path","repo","base","slug","jobs","worktrees","dashboard"})
        self.assertEqual(got["repo"], "example/sample")
        self.assertEqual(got["dashboard"], "http://localhost:8200/")

    def test_registration_create_only_and_race(self):
        self.data = None
        self.init()
        puts = [c for c in self.calls if c[0] == "PUT"]
        self.assertEqual(puts[0][3]["If-None-Match"], "*")
        self.assertNotIn(b"path:", self.data)
        self.assertEqual(self.saved()["etag"], self.tag())
        self.assertEqual(self.document.read_bytes(), self.data)
        original = self.document.read_bytes()
        old_paths = (self.home / "paths.md").read_bytes()
        self.data = None
        self.during_put = lambda: setattr(self, "data", self.base)
        self.refused(self.init)
        self.assertEqual(self.document.read_bytes(), original)
        self.assertEqual((self.home / "paths.md").read_bytes(), old_paths)

    def test_link_nonregular_unreadable_and_junction_paths(self):
        self.document.symlink_to(self.home / "outside")
        self.refused(self.pull)
        self.document.unlink()
        self.document.mkdir()
        self.refused(self.pull)
        self.document.rmdir()
        with patch.object(factory, "is_link", side_effect=lambda p: p == self.state):
            self.refused(self.pull)
        with patch.object(factory.os, "access", return_value=False):
            self.refused(self.pull)
        self.document.parent.rmdir()
        self.document.parent.symlink_to(self.home, target_is_directory=True)
        self.refused(self.pull)
        self.assertEqual(self.calls, [])


class FactoryTest(IsolatedTestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.dict(os.environ, {"FACTORY_SESSION": "sender", "PASEO_AGENT_ID": ""}).start()
        (self.home / "config.md").write_text("---\ndevice: test\n---\n")
        self.target = "sample/001-test"
        self.folder = self.home / "jobs" / self.target
        self.folder.mkdir(parents=True)
        (self.folder / "handoff.md").write_text("Resume build, round 1. Checks not yet run.\n")
        self.wt = self.home / "worktrees" / self.target
        self.wt.mkdir(parents=True)
        self.remote = self.home / "origin.git"
        self.git(self.home, "init", "--bare", str(self.remote))
        self.git(self.wt, "init")
        self.git(self.wt, "config", "user.name", "Test")
        self.git(self.wt, "config", "user.email", "test@example.com")
        self.git(self.wt, "checkout", "-b", "factory/001-test")
        self.git(self.wt, "commit", "--allow-empty", "-m", "initial")
        self.git(self.wt, "remote", "add", "origin", str(self.remote))
        self.git(self.wt, "push", "origin", "factory/001-test")
        self.head = self.git(self.wt, "rev-parse", "HEAD")
        self.job = {"id": "001-test", "project": "sample", "stage": "build", "round": 1,
                    "rounds": [{"code": "CHANGES"}], "claim": factory.new_claim("test", "sender")}
        self.store = {"/api/board": factory.dump_board({"jobs": [self.job]}).encode()}
        self.fail_upload = False
        self.change_on_upload = False
        self.race = False
        patch.object(factory, "api", side_effect=self.api).start()
        self.output = io.StringIO()
        self.redirect = contextlib.redirect_stdout(self.output)
        self.redirect.__enter__()
        self.addCleanup(self.redirect.__exit__, None, None, None)

    def git(self, cwd, *args):
        return subprocess.run(["git", "-C", str(cwd), *args], check=True,
                              capture_output=True, text=True).stdout.strip()

    def saved(self):
        return json.loads(self.store["/api/board"])["jobs"][0]

    def save(self, job):
        self.store["/api/board"] = factory.dump_board({"jobs": [job]}).encode()

    def api(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if method == "GET" and path.endswith("/?etags=1"):
            prefix = path[:-len("?etags=1")]
            rows = [{"name": k[len(prefix):], "etag": factory.sha(v)}
                    for k, v in self.store.items() if k.startswith(prefix)]
            return 200, {}, json.dumps(rows)
        data = self.store.get(path)
        tag = factory.sha(data) if data is not None else None
        if method == "PUT":
            if path != "/api/board" and self.fail_upload:
                raise SystemExit(5)
            if path == "/api/board" and self.race:
                self.race = False
                changed = self.saved()
                changed["claim"] = factory.new_claim("other", "receiver")
                self.save(changed)
                return 412, {}, ""
            if headers and (("If-Match" in headers and headers["If-Match"] != tag)
                            or headers.get("If-None-Match") == "*" and data is not None):
                return 412, {}, ""
            data = body.encode() if isinstance(body, str) else body
            self.store[path] = data
            if path != "/api/board" and self.change_on_upload:
                changed = self.saved()
                changed["stage"] = "review"
                self.save(changed)
            return 200, {"ETag": factory.sha(data)}, ""
        if data is None:
            if 404 in ok:
                return 404, {}, b"" if raw else ""
            raise AssertionError(f"unexpected missing API file {path}")
        return 200, {"ETag": tag}, data if raw else data.decode()

    def rejected(self, code=6):
        before = self.store["/api/board"]
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.handoff(self.target)
        self.assertEqual(raised.exception.code, code)
        self.assertEqual(self.store["/api/board"], before)

    def test_handoff_uploads_then_releases_without_resetting_stage_or_round(self):
        factory.handoff(self.target)
        job = self.saved()
        self.assertNotIn("claim", job)
        self.assertEqual((job["stage"], job["round"], job["rounds"]),
                         ("build", 1, [{"code": "CHANGES"}]))
        self.assertEqual(job["handoff"]["head"], self.head)
        self.assertEqual(job["handoff"]["state"], "ready")
        self.assertIn("/api/jobs/sample/001-test/handoff.md", self.store)

    def test_all_active_stages_and_old_acceptance_metadata(self):
        initial = self.store["/api/board"]
        for stage in ("spec", "build", "review", "approve"):
            self.store["/api/board"] = initial
            changed = self.saved()
            changed["stage"] = stage
            changed["handoff"] = {"state": "accepted", "to": {"session": "old"}, "accepted": "old"}
            self.save(changed)
            factory.handoff(self.target)
            self.assertEqual(self.saved()["stage"], stage)
            self.assertNotIn("to", self.saved()["handoff"])
            self.assertNotIn("accepted", self.saved()["handoff"])

    def test_hold_sets_started_once(self):
        factory.hold(self.target)
        self.assertEqual(self.saved()["started"], self.saved()["updated"])
        job = self.saved()
        job["started"] = "2026-10-30T00:00:00Z"
        self.save(job)
        factory.hold(self.target)
        self.assertEqual(self.saved()["started"], "2026-10-30T00:00:00Z")

    def test_hold_prints_elapsed_since_started(self):
        job = self.saved()
        job["started"] = (datetime.now(timezone.utc) - timedelta(seconds=340)).strftime("%Y-%m-%dT%H:%M:%SZ")
        self.save(job)
        factory.hold(self.target)
        lines = self.output.getvalue().splitlines()
        held = next(i for i, line in enumerate(lines) if line.startswith("held "))
        match = re.fullmatch(r"elapsed (\d+)s", lines[held + 1])
        self.assertIsNotNone(match)
        self.assertTrue(340 <= int(match.group(1)) <= 400)

    def test_receiver_accepts_with_own_claim_and_paseo_link(self):
        factory.handoff(self.target)
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}), patch.object(
                factory, "paseo_session", return_value={"server": "srv_new", "agent": "agent_new"}):
            factory.hold(self.target)
        job = self.saved()
        self.assertEqual(job["claim"]["session"], "receiver")
        self.assertEqual(job["handoff"]["state"], "accepted")
        self.assertEqual(job["handoff"]["from"]["session"], "sender")
        self.assertEqual(job["paseo"]["agent"], "agent_new")
        self.assertTrue((self.home / "paseo-claims" / "001-test" / "agent_new").is_file())
        with patch.dict(os.environ, {"FACTORY_SESSION": "third"}), self.assertRaises(SystemExit):
            factory.hold(self.target)
        self.assertEqual(self.saved()["claim"]["session"], "receiver")

    def test_non_owner_cannot_handoff_even_if_claim_is_stale(self):
        changed = self.saved()
        changed["claim"] = factory.new_claim("other", "someone", "2000-01-01T00:00:00Z")
        changed["claim"]["seen"] = "2000-01-01T00:00:00Z"
        self.save(changed)
        self.rejected()

    def test_released_hold_refuses_stale_foreign_claim(self):
        changed = self.saved()
        changed["claim"]["seen"] = "2000-01-01T00:00:00Z"
        self.save(changed)
        before = self.store["/api/board"]
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}), self.assertRaises(SystemExit):
            factory.hold(self.target, "released")
        self.assertEqual(self.store["/api/board"], before)

    def test_released_hold_accepts_unclaimed_job_and_refreshes_own_claim(self):
        factory.handoff(self.target)
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
            factory.hold(self.target, "released")
            factory.hold(self.target, "released")
        self.assertEqual(self.saved()["claim"]["session"], "receiver")

    def test_released_hold_rejects_inactive_stages(self):
        factory.handoff(self.target)
        initial = self.store["/api/board"]
        for stage in factory.RELEASE:
            self.store["/api/board"] = initial
            changed = self.saved()
            changed["stage"] = stage
            self.save(changed)
            before = self.store["/api/board"]
            with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}), \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                factory.hold(self.target, "released")
            self.assertEqual(raised.exception.code, 2)
            self.assertEqual(self.store["/api/board"], before)

    def test_handoff_preserves_human_note_through_acceptance(self):
        changed = self.saved()
        changed["note"] = "escalated: pending human sign-off"
        self.save(changed)
        factory.handoff(self.target)
        self.assertEqual(self.saved()["note"], "escalated: pending human sign-off")
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
            factory.hold(self.target, "released")
        self.assertEqual(self.saved()["note"], "escalated: pending human sign-off")
        self.assertEqual(self.saved()["handoff"]["state"], "accepted")

    def test_released_hold_rejects_stage_drift_before_claiming(self):
        factory.handoff(self.target)
        changed = self.saved()
        changed["stage"] = "review"
        self.save(changed)
        before = self.store["/api/board"]
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}), \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.hold(self.target, "released")
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual(self.store["/api/board"], before)
        self.assertNotIn("claim", json.loads(before)["jobs"][0])

    def test_released_hold_rejects_round_drift_before_claiming(self):
        factory.handoff(self.target)
        changed = self.saved()
        changed["round"] = 2
        self.save(changed)
        before = self.store["/api/board"]
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}), \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.hold(self.target, "released")
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual(self.store["/api/board"], before)

    def test_released_hold_after_progress_ignores_accepted_snapshot(self):
        factory.handoff(self.target)
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
            factory.hold(self.target, "released")
        changed = self.saved()
        changed["stage"], changed["round"] = "review", 2
        self.save(changed)
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
            factory.hold(self.target, "released")
        self.assertEqual(self.saved()["claim"]["session"], "receiver")
        self.assertEqual((self.saved()["stage"], self.saved()["round"]), ("review", 2))

    def add_review(self, text):
        (self.folder / "round-1").mkdir(exist_ok=True)
        (self.folder / "round-1" / "review-code.md").write_text(text)

    def released_hold_failure(self):
        """Run a receiver's released hold; return (SystemExit code, stderr text)."""
        err = io.StringIO()
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.hold(self.target, "released")
        return raised.exception.code, err.getvalue()

    def released_hold_exit(self):
        """Run a receiver's released hold; return its SystemExit code."""
        return self.released_hold_failure()[0]

    def test_released_hold_fails_on_local_only_review_verdict(self):
        factory.handoff(self.target)
        self.add_review("VERDICT: PASS\n")
        self.assertEqual(self.released_hold_exit(), 6)
        # The claim is already taken and is not rolled back.
        self.assertEqual(self.saved()["claim"]["session"], "receiver")
        self.assertEqual(self.saved()["handoff"]["state"], "accepted")

    def test_released_hold_fails_on_local_edit_of_synced_review(self):
        self.add_review("VERDICT: CHANGES\n")
        factory.handoff(self.target)
        self.add_review("VERDICT: PASS\n")
        self.assertEqual(self.released_hold_exit(), 6)
        self.assertEqual(self.saved()["claim"]["session"], "receiver")
        # The local edit is preserved, never overwritten or uploaded.
        self.assertEqual((self.folder / "round-1" / "review-code.md").read_text(), "VERDICT: PASS\n")

    def test_released_hold_fails_when_r2_loses_a_synced_review(self):
        self.add_review("VERDICT: CHANGES\n")
        factory.handoff(self.target)
        del self.store["/api/jobs/sample/001-test/round-1/review-code.md"]
        self.assertEqual(self.released_hold_exit(), 6)
        self.assertEqual(self.saved()["claim"]["session"], "receiver")
        self.assertTrue((self.folder / "round-1" / "review-code.md").exists())

    def test_released_hold_fails_on_symlinked_review(self):
        factory.handoff(self.target)
        outside = self.home / "outside.md"
        outside.write_text("VERDICT: PASS\n")
        try:
            self.add_review("unused")
            (self.folder / "round-1" / "review-code.md").unlink()
            (self.folder / "round-1" / "review-code.md").symlink_to(outside)
        except OSError:
            self.skipTest("symlinks unavailable")
        code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction inside the job folder", err)
        self.assertTrue((self.folder / "round-1" / "review-code.md").is_symlink())
        self.assertEqual(outside.read_text(), "VERDICT: PASS\n")

    def test_released_hold_fails_on_symlinked_round_directory(self):
        factory.handoff(self.target)
        outside = self.home / "outside-round"
        outside.mkdir()
        (outside / "review-code.md").write_text("VERDICT: PASS\n")
        try:
            (self.folder / "round-1").symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unavailable")
        code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction inside the job folder", err)
        # The link, its target, and the unpublished verdict are left for a human.
        self.assertTrue((self.folder / "round-1").is_symlink())
        self.assertEqual((outside / "review-code.md").read_text(), "VERDICT: PASS\n")

    def test_released_hold_fails_when_newer_remote_replaces_a_symlink(self):
        self.add_review("VERDICT: CHANGES\n")
        factory.handoff(self.target)
        path = self.folder / "round-1" / "review-code.md"
        try:
            path.unlink()
            path.symlink_to(self.home / "no-such-target.md")
        except OSError:
            self.skipTest("symlinks unavailable")
        self.store["/api/jobs/sample/001-test/round-1/review-code.md"] = b"VERDICT: PASS\n"
        code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction inside the job folder", err)
        # Pull must refuse before downloading, so the link is never replaced by the file.
        self.assertTrue(path.is_symlink())

    def test_released_hold_fails_on_symlinked_job_folder(self):
        factory.handoff(self.target)
        real = self.home / "real-job-folder"
        try:
            self.folder.rename(real)
            self.folder.symlink_to(real, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unavailable")
        code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction", err)
        self.assertIn("must be real directories", err)
        self.assertTrue(self.folder.is_symlink())

    def test_released_hold_fails_on_symlinked_project_folder(self):
        factory.handoff(self.target)
        real = self.home / "real-jobs-sample"
        try:
            (self.home / "jobs" / "sample").rename(real)
            (self.home / "jobs" / "sample").symlink_to(real, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unavailable")
        code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction", err)
        self.assertIn("must be real directories", err)
        self.assertTrue((self.home / "jobs" / "sample").is_symlink())

    def test_released_hold_fails_on_symlinked_sync_manifest(self):
        factory.handoff(self.target)
        outside = self.home / "outside-manifest.json"
        outside.write_text((self.folder / ".sync.json").read_text())
        try:
            (self.folder / ".sync.json").unlink()
            (self.folder / ".sync.json").symlink_to(outside)
        except OSError:
            self.skipTest("symlinks unavailable")
        code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction inside the job folder", err)
        self.assertTrue((self.folder / ".sync.json").is_symlink())

    def test_released_hold_fails_on_symlinked_conflict_copy(self):
        self.add_review("VERDICT: CHANGES\n")
        factory.handoff(self.target)
        link = self.folder / "round-1" / "review-code.md.remote"
        try:
            link.symlink_to(self.home / "no-such.remote")
        except OSError:
            self.skipTest("symlinks unavailable")
        code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction inside the job folder", err)
        self.assertTrue(link.is_symlink())

    def needs_posix_permissions(self):
        if os.name != "posix" or os.geteuid() == 0:
            self.skipTest("real permission bits need a non-root POSIX user")

    def test_released_hold_fails_on_unlistable_round_directory(self):
        # A traversal-only (mode 0111) round directory enumerates as empty, so an
        # unpublished local-only PASS must not slip past the guard as "no artifacts".
        self.needs_posix_permissions()
        factory.handoff(self.target)
        self.add_review("VERDICT: PASS\n")
        round_dir = self.folder / "round-1"
        artifacts = {k: v for k, v in self.store.items() if k.startswith("/api/jobs/")}
        try:
            round_dir.chmod(0o111)
            code, err = self.released_hold_failure()
        finally:
            round_dir.chmod(0o755)
        self.assertEqual(code, 6)
        self.assertIn("cannot inspect job paths", err)
        # The hidden verdict is directly readable and nothing was uploaded, so R2
        # still lacks it; the guard refused before any artifact I/O.
        self.assertEqual((round_dir / "review-code.md").read_text(), "VERDICT: PASS\n")
        self.assertEqual({k: v for k, v in self.store.items() if k.startswith("/api/jobs/")}, artifacts)

    def test_released_hold_fails_on_unreadable_job_folder_hiding_a_link(self):
        # A write-and-execute-only (mode 0333) job folder enumerates as empty, so a
        # symlinked round directory inside it could route pull's writes outside the
        # job before the strict check could ever see it.
        self.needs_posix_permissions()
        self.add_review("VERDICT: CHANGES\n")
        factory.handoff(self.target)
        outside = self.home / "outside-round"
        outside.mkdir()
        (outside / "review-code.md").write_text("VERDICT: PASS\n")
        link = self.folder / "round-1"
        try:
            shutil.rmtree(self.folder / "round-1")
            link.symlink_to(outside, target_is_directory=True)
            self.store["/api/jobs/sample/001-test/round-1/review-code.md"] = b"VERDICT: NEWER\n"
            self.folder.chmod(0o333)
            code, err = self.released_hold_failure()
        finally:
            self.folder.chmod(0o755)
        self.assertEqual(code, 6)
        self.assertIn("cannot inspect job paths", err)
        # The link and its target are untouched; nothing was written outside the job.
        self.assertTrue(link.is_symlink())
        self.assertEqual((outside / "review-code.md").read_text(), "VERDICT: PASS\n")

    def test_scan_errors_fail_closed_but_a_missing_fresh_root_passes(self):
        real_walk = os.walk
        factory.handoff(self.target)

        def walk_denied(path, onerror=None, followlinks=False):
            if onerror is not None:
                onerror(PermissionError(13, "Permission denied"))
            return iter([])

        with patch.object(factory.os, "walk", side_effect=walk_denied):
            code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("cannot inspect job paths", err)

        # A genuinely missing job folder is a fresh receiver's empty start: the
        # FileNotFoundError from the scan is not a refusal. Once pull creates the
        # root, the mock steps aside so the later strict verification walks the
        # real tree instead of pretending an existing root is still missing.
        def walk_missing(path, onerror=None, followlinks=False):
            if Path(path).exists():
                return real_walk(path, onerror=onerror, followlinks=followlinks)
            if onerror is not None:
                onerror(FileNotFoundError(2, "No such file or directory"))
            return iter([])

        shutil.rmtree(self.folder)
        with patch.object(factory.os, "walk", side_effect=walk_missing):
            with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
                factory.hold(self.target, "released")
        self.assertEqual(self.saved()["claim"]["session"], "receiver")
        self.assertTrue((self.folder / "handoff.md").exists())

    def test_released_hold_fails_when_a_scan_error_hits_an_existing_root(self):
        # A FileNotFoundError reported while the job folder still exists is an
        # incomplete walk of an existing tree, not a fresh receiver's missing root,
        # so it must fail closed before any artifact I/O.
        factory.handoff(self.target)

        def walk_vanished(path, onerror=None, followlinks=False):
            if onerror is not None:
                onerror(FileNotFoundError(2, "No such file or directory"))
            return iter([])

        with patch.object(factory.os, "walk", side_effect=walk_vanished):
            code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("cannot inspect job paths", err)

    def reparse(self, path, is_dir):
        """Patch os.lstat so one path reports Windows reparse-point attributes, the
        shape a directory junction or a file reparse point has, as os.path.islink
        reports False for it. Only st_mode and st_file_attributes are faked."""
        real, target = os.lstat, str(path)
        mode = stat.S_IFDIR if is_dir else stat.S_IFREG

        def lstat(p, *args, **kwargs):
            if str(p) == target:
                return types.SimpleNamespace(
                    st_mode=mode,
                    st_file_attributes=stat.FILE_ATTRIBUTE_REPARSE_POINT)
            return real(p, *args, **kwargs)

        return patch.object(factory.os, "lstat", side_effect=lstat)

    def test_released_hold_fails_on_mocked_junction_round_directory(self):
        factory.handoff(self.target)
        outside = self.home / "outside-round"
        outside.mkdir()
        (outside / "review-code.md").write_text("VERDICT: PASS\n")
        (self.folder / "round-1").mkdir()
        with self.reparse(self.folder / "round-1", is_dir=True):
            code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction inside the job folder", err)
        # The junction's target is left untouched.
        self.assertEqual((outside / "review-code.md").read_text(), "VERDICT: PASS\n")

    def test_released_hold_fails_on_mocked_junction_job_folder(self):
        factory.handoff(self.target)
        with self.reparse(self.folder, is_dir=True):
            code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction", err)
        self.assertIn("must be real directories", err)

    def test_released_hold_fails_on_mocked_reparse_artifact(self):
        self.add_review("VERDICT: CHANGES\n")
        factory.handoff(self.target)
        with self.reparse(self.folder / "round-1" / "review-code.md", is_dir=False):
            code, err = self.released_hold_failure()
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction inside the job folder", err)

    def test_released_hold_fails_on_real_windows_junction(self):
        if os.name != "nt":
            self.skipTest("directory junctions need Windows; the mocked reparse tests "
                          "cover the guard on other platforms")
        factory.handoff(self.target)
        outside = self.home / "outside-round"
        outside.mkdir()
        (outside / "review-code.md").write_text("VERDICT: PASS\n")
        subprocess.run(["cmd", "/c", "mklink", "/J",
                        str(self.folder / "round-1"), str(outside)],
                       check=True, capture_output=True)
        code, err = self.released_hold_failure()
        # The refusal is the path guard, not a held claim or a drifted snapshot.
        self.assertEqual(code, 6)
        self.assertIn("is a symlink or junction inside the job folder", err)
        self.assertIn("round-1", err)
        st = os.lstat(self.folder / "round-1")
        self.assertTrue(st.st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT)
        self.assertEqual((outside / "review-code.md").read_text(), "VERDICT: PASS\n")

    def test_symlinked_factory_home_is_trusted(self):
        alias = Path(self.temp.name + "-alias")
        try:
            alias.symlink_to(self.home, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unavailable")
        self.addCleanup(alias.unlink)
        with patch.object(factory, "factory_home", return_value=alias):
            factory.handoff(self.target)
            with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
                factory.hold(self.target, "released")
        self.assertEqual(self.saved()["claim"]["session"], "receiver")

    def test_released_hold_clean_resume_downloads_and_passes_strict_check(self):
        self.add_review("VERDICT: CHANGES\n")
        factory.handoff(self.target)
        # A fresh receiver device starts with no local copies and no manifest.
        shutil.rmtree(self.folder)
        self.folder.mkdir(parents=True)
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
            factory.hold(self.target, "released")
        self.assertEqual(self.saved()["claim"]["session"], "receiver")
        self.assertEqual((self.folder / "round-1" / "review-code.md").read_text(), "VERDICT: CHANGES\n")
        self.assertTrue((self.folder / ".sync.json").exists())

    def test_released_hold_artifact_listing_failure_after_claim(self):
        factory.handoff(self.target)
        listings = []

        def flaky(home, method, path, *args, **kwargs):
            if method == "GET" and path.endswith("/?etags=1"):
                listings.append(path)
                if len(listings) > 1:
                    raise SystemExit(5)
            return self.api(home, method, path, *args, **kwargs)

        with patch.object(factory, "api", side_effect=flaky), \
                patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}), \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.hold(self.target, "released")
        self.assertEqual(raised.exception.code, 5)
        self.assertEqual(self.saved()["claim"]["session"], "receiver")

    def test_old_sender_cannot_handoff_twice(self):
        factory.handoff(self.target)
        self.rejected()

    def test_missing_remote_branch(self):
        self.git(self.wt, "push", "origin", "--delete", "factory/001-test")
        self.rejected()

    def test_network_failure_keeps_claim(self):
        with patch.object(factory, "origin_git", side_effect=SystemExit(5)):
            self.rejected(5)

    def test_remote_deleted_artifact_keeps_claim(self):
        (self.folder / "build.md").write_text("build")
        factory.push(self.target)
        del self.store["/api/jobs/sample/001-test/build.md"]
        self.rejected()

    def test_unrelated_board_race_retries(self):
        original_api = self.api
        raced = []

        def api(*args, **kwargs):
            if args[1:3] == ("PUT", "/api/board") and not raced:
                raced.append(True)
                data = json.loads(self.store["/api/board"])
                data["jobs"].append({"id": "002-other", "stage": "spec"})
                self.store["/api/board"] = factory.dump_board(data).encode()
                return 412, {}, ""
            return original_api(*args, **kwargs)

        with patch.object(factory, "api", side_effect=api), patch.object(factory.time, "sleep"):
            factory.handoff(self.target)
        self.assertNotIn("claim", self.saved())
        self.assertEqual(json.loads(self.store["/api/board"])["jobs"][1]["id"], "002-other")

    def test_no_session_cannot_handoff(self):
        with patch.dict(os.environ, {"FACTORY_SESSION": ""}):
            self.rejected()

    def test_project_mismatch(self):
        self.target = "wrong/001-test"
        self.rejected(4)

    def test_hold_rejects_project_mismatch(self):
        before = self.store["/api/board"]
        with self.assertRaises(SystemExit):
            factory.hold("wrong/001-test")
        self.assertEqual(self.store["/api/board"], before)

    def test_human_and_terminal_gates_are_not_released(self):
        for stage in ("needs-human", "pr-open", "merged", "closed"):
            changed = self.saved()
            changed["stage"] = stage
            self.save(changed)
            self.rejected(2)

    def test_missing_checkpoint(self):
        (self.folder / "handoff.md").unlink()
        self.rejected(2)

    def test_blank_checkpoint(self):
        (self.folder / "handoff.md").write_text("\n")
        self.rejected(2)

    def test_symlink_checkpoint(self):
        checkpoint = self.folder / "handoff.md"
        checkpoint.unlink()
        outside = self.home / "outside.md"
        outside.write_text("not a checkpoint")
        try:
            checkpoint.symlink_to(outside)
        except OSError:
            self.skipTest("symlinks unavailable")
        self.rejected(6)

    def test_dirty_worktree(self):
        (self.wt / "unfinished.txt").write_text("keep this")
        self.rejected()

    def test_wrong_branch(self):
        self.git(self.wt, "checkout", "-b", "other")
        self.rejected()

    def test_unpublished_commit(self):
        self.git(self.wt, "commit", "--allow-empty", "-m", "not pushed")
        self.rejected()

    def test_failed_upload_keeps_claim(self):
        self.fail_upload = True
        self.rejected(5)

    def test_conflicting_checkpoint_keeps_claim(self):
        self.store["/api/jobs/sample/001-test/handoff.md"] = b"another device's checkpoint"
        self.rejected()

    def test_remote_newer_artifact_keeps_claim(self):
        (self.folder / "build.md").write_text("old build")
        factory.push(self.target)
        self.store["/api/jobs/sample/001-test/build.md"] = b"newer build"
        self.rejected()

    def test_state_change_during_upload_does_not_release_claim(self):
        self.change_on_upload = True
        with self.assertRaises(SystemExit):
            factory.handoff(self.target)
        self.assertEqual(self.saved()["stage"], "review")
        self.assertEqual(self.saved()["claim"]["session"], "sender")

    def test_cas_retry_cannot_release_new_owner(self):
        self.race = True
        with patch.object(factory.time, "sleep"), self.assertRaises(SystemExit):
            factory.handoff(self.target)
        self.assertEqual(self.saved()["claim"]["session"], "receiver")

    def test_board_cannot_forge_handoff(self):
        with patch.object(factory.sys, "stdin", io.StringIO('{"handoff":{"state":"ready"}}')):
            with self.assertRaises(SystemExit):
                factory.board(self.home, "001-test")
        self.assertNotIn("handoff", self.saved())


    def test_stuck_skips_a_stale_job_waiting_on_human(self):
        (self.home / "paths.md").write_text(f"---\nsample: {self.home / 'sample'}\n---\n")
        patch.object(factory, "paseo_server", return_value="").start()
        stale = dict(self.job, claim=dict(self.job["claim"], seen="2020-01-01T00:00:00Z"))

        self.save(dict(stale, needs="send iPhone screenshots"))
        before = self.store["/api/board"]
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.stuck()
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.store["/api/board"], before)

        for job in (stale, dict(stale, needs="   ")):
            with self.subTest(needs=job.get("needs")):
                self.save(job)
                self.output.seek(0)
                self.output.truncate()
                factory.stuck()
                self.assertEqual([j["id"] for j in json.loads(self.output.getvalue())], ["001-test"])


    def test_stuck_drops_a_stale_stub(self):
        (self.home / "paths.md").write_text(f"---\nsample: {self.home / 'sample'}\n---\n")
        patch.object(factory, "paseo_server", return_value="").start()
        claim = dict(factory.new_claim("other", "s0"), seen="2020-01-01T00:00:00Z")
        self.store["/api/board"] = factory.dump_board({"jobs": [
            {"id": "002-stub", "project": "sample", "claim": claim},
            {"id": "003-fresh", "project": "sample", "claim": factory.new_claim("other", "s1")},
            {"id": "004-unclaimed", "project": "sample", "updated": "2020-01-02T00:00:00Z"},
            {"project": "sample", "updated": "2020-01-03T00:00:00Z"},
            {"id": "005-bad\nfake", "project": "sample", "updated": "2020-01-04T00:00:00Z"}]}).encode()
        with contextlib.redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit) as raised:
            factory.stuck()
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual([j["id"] for j in json.loads(self.store["/api/board"])["jobs"]], ["003-fresh"])
        self.assertEqual(err.getvalue(), (
            "dropped stub '002-stub': no stage, last seen 2020-01-01T00:00:00Z\n"
            "dropped stub '004-unclaimed': no stage, last seen 2020-01-02T00:00:00Z\n"
            "dropped stub None: no stage, last seen 2020-01-03T00:00:00Z\n"
            "dropped stub '005-bad\\nfake': no stage, last seen 2020-01-04T00:00:00Z\n"))

class PaseoClaimHomeTest(IsolatedTestCase):
    setUp = FactoryTest.setUp
    api = FactoryTest.api
    git = FactoryTest.git
    saved = FactoryTest.saved

    @contextlib.contextmanager
    def resolved_home(self):
        resolved = self.home / "resolved"
        with patch.object(factory, "factory_home", return_value=resolved), patch.object(
                factory, "paseo_session", return_value={"server": "srv_new", "agent": "agent_new"}):
            yield resolved

    def test_board_writes_marker_under_resolved_home(self):
        with self.resolved_home() as resolved, patch.object(factory.sys, "stdin", io.StringIO('{"note": "checking"}')):
            factory.board(self.home, "001-test")
        self.assertTrue((resolved / "paseo-claims" / "001-test" / "agent_new").is_file())
        self.assertFalse((self.home / "paseo-claims").exists())

    def test_claim_id_writes_marker_under_resolved_home(self):
        with self.resolved_home() as resolved:
            job_id = factory.claim_id(self.home, "sample-export", "sample")
        self.assertTrue((resolved / "paseo-claims" / job_id / "agent_new").is_file())
        self.assertFalse((self.home / "paseo-claims").exists())

    def test_board_resolves_home_before_the_write(self):
        before = self.store["/api/board"]
        with patch.object(factory, "factory_home", side_effect=SystemExit(2)), patch.object(
                factory, "paseo_session", return_value={"server": "srv_new", "agent": "agent_new"}), \
                patch.object(factory.sys, "stdin", io.StringIO('{"note": "checking"}')):
            with self.assertRaises(SystemExit):
                factory.board(self.home, "001-test")
        self.assertEqual(self.store["/api/board"], before)

    def test_claim_id_resolves_home_before_the_write(self):
        before = self.store["/api/board"]
        with patch.object(factory, "factory_home", side_effect=SystemExit(2)), patch.object(
                factory, "paseo_session", return_value={"server": "srv_new", "agent": "agent_new"}), \
                patch.object(factory.sys, "stdin", io.StringIO('{"note": "checking"}')):
            with self.assertRaises(SystemExit):
                factory.claim_id(self.home, "sample-export", "sample")
        self.assertEqual(self.store["/api/board"], before)

    def test_claim_id_resolves_home_before_the_folder_and_session(self):
        fresh = self.home / "fresh"
        with patch.dict(os.environ, {"FACTORY_SESSION": ""}), patch.object(
                factory, "factory_home", side_effect=SystemExit(2)), patch.object(
                factory, "paseo_session", return_value={"server": "srv_new", "agent": "agent_new"}):
            with self.assertRaises(SystemExit):
                factory.claim_id(fresh, "sample-export", "sample")
            self.assertEqual(os.environ["FACTORY_SESSION"], "")
        self.assertFalse(fresh.exists())

    def test_board_without_paseo_skips_the_home(self):
        with patch.object(factory, "factory_home", side_effect=SystemExit(2)), patch.object(
                factory, "paseo_session", return_value=None), \
                patch.object(factory.sys, "stdin", io.StringIO('{"note": "checking"}')):
            factory.board(self.home, "001-test")
        self.assertEqual(self.saved()["note"], "checking")


class HoldWriteSkipTest(IsolatedTestCase):
    setUp = FactoryTest.setUp
    api = FactoryTest.api
    git = FactoryTest.git
    saved = FactoryTest.saved
    save = FactoryTest.save

    def ago(self, minutes):
        stamp = datetime.now(timezone.utc) - timedelta(minutes=minutes)
        return stamp.strftime("%Y-%m-%dT%H:%M:%SZ")

    def seed(self, minutes, **extra):
        job = self.saved()
        job["started"] = "2026-11-11T00:00:00Z"
        job["claim"]["seen"] = self.ago(minutes)
        job.update(extra)
        self.save(job)

    def board_puts(self, take=None):
        calls = []

        def record(home, method, path, *args, **kwargs):
            calls.append((method, path))
            return self.api(home, method, path, *args, **kwargs)
        with patch.object(factory, "api", side_effect=record):
            factory.hold(self.target, take)
        return calls.count(("PUT", "/api/board"))

    def test_fresh_own_claim_skips_the_board_write(self):
        self.seed(3)
        before = self.store["/api/board"]
        self.assertEqual(self.board_puts(), 0)
        self.assertEqual(self.store["/api/board"], before)
        self.assertIn("held sample/001-test as test session sender", self.output.getvalue())
        self.assertIn("elapsed ", self.output.getvalue())

    def test_claim_seen_over_ten_minutes_ago_writes(self):
        self.seed(11)
        old = self.saved()["claim"]["seen"]
        self.assertEqual(self.board_puts(), 1)
        self.assertGreater(self.saved()["claim"]["seen"], old)

    def test_other_session_claim_writes(self):
        self.seed(200)
        job = self.saved()
        job["claim"] = factory.new_claim("other", "someone")
        job["claim"]["seen"] = self.ago(200)
        self.save(job)
        self.assertEqual(self.board_puts(), 1)
        self.assertEqual(self.saved()["claim"]["session"], "sender")

    def test_take_writes(self):
        self.seed(3)
        self.assertEqual(self.board_puts("take"), 1)

    def test_released_writes(self):
        self.seed(3)
        factory.handoff(self.target)
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
            factory.hold(self.target, "released")
            self.assertEqual(self.board_puts("released"), 1)

    def test_handoff_acceptance_writes(self):
        self.seed(3)
        factory.handoff(self.target)
        with patch.dict(os.environ, {"FACTORY_SESSION": "receiver"}):
            job = self.saved()
            job["claim"] = factory.new_claim("test", "receiver")
            self.save(job)
            self.assertEqual(self.board_puts(), 1)
        self.assertEqual(self.saved()["handoff"]["state"], "accepted")

    def test_paseo_link_change_writes(self):
        self.seed(3, paseo={"server": "srv_old", "agent": "agent_old"})
        new = {"server": "srv_new", "agent": "agent_new"}
        with patch.object(factory, "paseo_session", return_value=new):
            self.assertEqual(self.board_puts(), 1)
        self.assertEqual(self.saved()["paseo"], new)
        self.seed(3, paseo={"server": "srv_old", "agent": "agent_old"})
        with patch.object(factory, "paseo_session", return_value=None):
            self.assertEqual(self.board_puts(), 1)
        self.assertNotIn("paseo", self.saved())


class IdeaTest(IsolatedTestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.board = {"jobs": [
            {"id": "001-missing", "stage": "idea", "idea": "missing"},
            {"id": "002-null", "stage": "idea", "project": None, "idea": "null"},
            {"id": "003-empty", "stage": "idea", "project": "", "idea": "empty"},
        ]}
        self.unassigned = json.loads(json.dumps(self.board["jobs"]))
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(Path, "home", return_value=self.home).start()
        patch.object(factory, "holder", return_value=("device", "session", 120)).start()
        patch.object(factory, "read_paths", return_value={"": str(self.home), "sample": str(self.home)}).start()
        patch.object(factory.time, "sleep").start()
        self.api = patch.object(factory, "api", side_effect=self.request).start()
        self.output = io.StringIO()
        self.redirect = contextlib.redirect_stdout(self.output)
        self.redirect.__enter__()
        self.addCleanup(self.redirect.__exit__, None, None, None)
        self.conflict = False

    def request(self, home, method, path, body=None, ok=(200,), headers=None):
        self.assertEqual(path, "/api/board")
        if method == "GET":
            return 200, {"ETag": '"fixture"'}, factory.dump_board(self.board)
        if self.conflict:
            self.conflict = False
            self.board["jobs"][-1]["claim"] = factory.new_claim("other", "other")
            return 412, {}, ""
        self.assertEqual(headers, {"If-Match": '"fixture"'})
        self.board = json.loads(body)
        return 200, {}, ""

    def test_ideas_skip_unassigned_without_consuming_limit(self):
        self.board["jobs"].append({"id": "004-ready", "stage": "idea", "project": "sample", "idea": "ready"})
        factory.ideas("--limit", "1")
        picked = json.loads(self.output.getvalue())
        self.assertEqual(picked, [{"id": "004-ready", "project": "sample",
                                  "repo_path": str(self.home), "session": "session"}])
        self.assertEqual(self.board["jobs"][:3], self.unassigned)
        self.assertEqual(self.board["jobs"][-1]["claim"]["session"], "session")

    def test_ideas_only_unassigned_writes_nothing(self):
        with self.assertRaises(SystemExit) as raised:
            factory.ideas("--limit", "1")
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.output.getvalue(), "")
        self.assertEqual(self.board["jobs"], self.unassigned)
        self.assertFalse(any(c.args[1] == "PUT" for c in self.api.call_args_list))
        self.assertEqual(list(self.home.iterdir()), [])

    def test_ideas_claim_conflict_does_not_overwrite_other_holder(self):
        self.board["jobs"].append({"id": "004-ready", "stage": "idea", "project": "sample"})
        self.conflict = True
        with self.assertRaises(SystemExit) as raised:
            factory.ideas()
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.output.getvalue(), "")
        self.assertEqual(self.board["jobs"][-1]["claim"]["session"], "other")
        self.assertEqual(sum(c.args[1] == "PUT" for c in self.api.call_args_list), 1)

    def stamp(self, **delta):
        return (factory.datetime.now(factory.timezone.utc) + factory.timedelta(**delta)).strftime(
            "%Y-%m-%dT%H:%M:%SZ")

    def test_ideas_rejects_bad_flags_with_usage(self):
        for argv in (("--max-inflight", "0"), ("--max-inflight", "abc"), ("--max-inflight", "-1"),
                     ("--max-inflight",), ("--max-inflight", "1", "--max-inflight", "1"),
                     ("--project",), ("--project", ""), ("--project", "a", "--project", "b"),
                     ("--unknown", "1")):
            with self.subTest(args=argv):
                with contextlib.redirect_stderr(io.StringIO()) as err:
                    with self.assertRaises(SystemExit) as raised:
                        factory.ideas(*argv)
                self.assertEqual(raised.exception.code, 2)
                self.assertIn(
                    "usage: ideas [--limit N] [--max-inflight N] [--project <slug>] [--session <tag>]",
                    err.getvalue())

    def test_ideas_extra_or_dangling_args_exit_with_ideas_usage_not_module_help(self):
        for argv in (("ideas", "--project", "factory", "--max-inflight", "2",
                      "--limit", "1", "--limit", "1"),
                     ("ideas", "--limit")):
            with self.subTest(args=argv):
                with contextlib.redirect_stderr(io.StringIO()) as err:
                    with self.assertRaises(SystemExit) as raised:
                        factory.main(list(argv))
                self.assertEqual(raised.exception.code, 2)
                self.assertIn(
                    "usage: ideas [--limit N] [--max-inflight N] [--project <slug>] [--session <tag>]",
                    err.getvalue())
                self.assertNotIn("usage:\n", err.getvalue())

    def test_ideas_max_inflight_counts_fresh_moving_job_and_puts_nothing(self):
        self.board["jobs"] += [{"id": "004-spec", "stage": "spec", "project": "sample",
                                "updated": self.stamp()},
                               {"id": "005-ready", "stage": "idea", "project": "sample"}]
        with self.assertRaises(SystemExit) as raised:
            factory.ideas("--max-inflight", "1")
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.output.getvalue(), "")
        self.assertFalse(any(c.args[1] == "PUT" for c in self.api.call_args_list))

    def test_ideas_max_inflight_treats_null_timestamp_as_stale(self):
        self.board["jobs"] += [{"id": "004-spec", "stage": "spec", "project": "sample",
                                "updated": None},
                               {"id": "005-ready", "stage": "idea", "project": "sample"}]
        factory.ideas("--max-inflight", "1")
        self.assertEqual([p["id"] for p in json.loads(self.output.getvalue())], ["005-ready"])

    def test_ideas_max_inflight_counts_fresh_claim_from_any_holder(self):
        self.board["jobs"] += [{"id": "004-claimed", "stage": "idea", "project": "sample",
                                "claim": factory.new_claim("other", "other")},
                               {"id": "005-ready", "stage": "idea", "project": "sample"}]
        with self.assertRaises(SystemExit) as raised:
            factory.ideas("--max-inflight", "1")
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.output.getvalue(), "")
        self.assertFalse(any(c.args[1] == "PUT" for c in self.api.call_args_list))

    def test_ideas_max_inflight_ignores_stale_load(self):
        self.board["jobs"] += [{"id": "004-spec", "stage": "spec", "project": "sample",
                                "updated": self.stamp(hours=-3)},
                               {"id": "005-ready", "stage": "idea", "project": "sample"},
                               {"id": "006-claimed", "stage": "idea", "project": "sample",
                                "claim": dict(factory.new_claim("other", "other"),
                                              seen=self.stamp(hours=-3))}]
        factory.ideas("--max-inflight", "1")
        picked = json.loads(self.output.getvalue())
        self.assertEqual([p["id"] for p in picked], ["005-ready"])
        self.assertEqual(picked[0]["session"], "session")

    def test_ideas_max_inflight_skips_escalated_pr_open(self):
        self.board["jobs"] += [{"id": "004-pr", "stage": "pr-open", "project": "sample",
                                "escalated": True, "updated": self.stamp()},
                               {"id": "005-ready", "stage": "idea", "project": "sample"}]
        factory.ideas("--max-inflight", "1")
        self.assertEqual([p["id"] for p in json.loads(self.output.getvalue())], ["005-ready"])

    def test_ideas_max_inflight_and_limit_compose_in_either_order(self):
        for argv in (("--limit", "1", "--max-inflight", "2"), ("--max-inflight", "2", "--limit", "1")):
            with self.subTest(args=argv):
                self.board = {"jobs": self.unassigned + [
                    {"id": "004-a", "stage": "idea", "project": "sample"},
                    {"id": "005-b", "stage": "idea", "project": "sample"}]}
                self.output.seek(0)
                self.output.truncate()
                factory.ideas(*argv)
                self.assertEqual([p["id"] for p in json.loads(self.output.getvalue())], ["004-a"])
                self.assertEqual(self.board["jobs"][3]["claim"]["session"], "session")

    def test_ideas_without_max_inflight_claims_despite_fresh_inflight(self):
        self.board["jobs"] += [{"id": "004-spec", "stage": "spec", "project": "sample",
                                "updated": self.stamp()},
                               {"id": "005-ready", "stage": "idea", "project": "sample"}]
        factory.ideas()
        self.assertEqual([p["id"] for p in json.loads(self.output.getvalue())], ["005-ready"])

    def test_ideas_max_inflight_conflict_recounts_and_puts_once(self):
        self.board["jobs"].append({"id": "004-ready", "stage": "idea", "project": "sample"})
        self.conflict = True
        with self.assertRaises(SystemExit) as raised:
            factory.ideas("--max-inflight", "1")
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.output.getvalue(), "")
        self.assertEqual(self.board["jobs"][-1]["claim"]["session"], "other")
        self.assertEqual(sum(c.args[1] == "PUT" for c in self.api.call_args_list), 1)

    def test_ideas_project_claims_only_that_project(self):
        self.board["jobs"] += [{"id": "004-foreign", "stage": "idea", "project": "other"},
                               {"id": "005-ready", "stage": "idea", "project": "sample"}]
        with patch.object(factory, "read_paths",
                          return_value={"sample": str(self.home), "other": str(self.home)}):
            factory.ideas("--project", "sample")
        picked = json.loads(self.output.getvalue())
        self.assertEqual([p["id"] for p in picked], ["005-ready"])
        self.assertIsNone(self.board["jobs"][3].get("claim"))

    def test_ideas_project_cap_ignores_foreign_inflight(self):
        self.board["jobs"] += [{"id": "004-spec", "stage": "spec", "project": "other",
                                "updated": self.stamp()},
                               {"id": "005-ready", "stage": "idea", "project": "sample"}]
        factory.ideas("--project", "sample", "--max-inflight", "1")
        self.assertEqual([p["id"] for p in json.loads(self.output.getvalue())], ["005-ready"])

    def test_ideas_project_unknown_slug_fails_before_cas(self):
        with contextlib.redirect_stderr(io.StringIO()) as err:
            with self.assertRaises(SystemExit) as raised:
                factory.ideas("--project", "nosuch")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("unknown or unavailable project: nosuch", err.getvalue())
        self.assertFalse(any(c.args[1] == "PUT" for c in self.api.call_args_list))

    def test_ideas_project_without_checkout_fails_before_cas(self):
        with patch.object(factory, "read_paths",
                          return_value={"sample": str(self.home / "missing")}):
            with contextlib.redirect_stderr(io.StringIO()) as err:
                with self.assertRaises(SystemExit) as raised:
                    factory.ideas("--project", "sample")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("unknown or unavailable project: sample", err.getvalue())
        self.assertFalse(any(c.args[1] == "PUT" for c in self.api.call_args_list))

    def test_ideas_without_project_counts_all_projects_toward_cap(self):
        self.board["jobs"] += [{"id": "004-spec", "stage": "spec", "project": "other",
                                "updated": self.stamp()},
                               {"id": "005-ready", "stage": "idea", "project": "sample"}]
        with self.assertRaises(SystemExit) as raised:
            factory.ideas("--max-inflight", "1")
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.output.getvalue(), "")
        self.assertFalse(any(c.args[1] == "PUT" for c in self.api.call_args_list))

    def test_sync_skips_discarded_unassigned_but_not_executable_records(self):
        self.board["jobs"] += [dict(j, stage="closed") for j in self.unassigned]
        self.board["jobs"].append({"id": "007-build", "stage": "build", "project": "sample"})
        with patch.object(factory, "pull_job", return_value=("ok", [])) as pull:
            factory.sync()
            pull.assert_called_once_with(self.home, "sample", "007-build")
        self.assertEqual(list(self.home.iterdir()), [])
        self.board = {"jobs": [{"id": "008-malformed", "stage": "build"}]}
        with patch.object(factory, "pull_job", side_effect=RuntimeError("bad executable job")):
            with self.assertRaisesRegex(RuntimeError, "bad executable job"):
                factory.sync()


class SyncFinishedJobsTest(IsolatedTestCase):
    def setUp(self):
        IdeaTest.setUp(self)
        self.paths = []
        self.board["jobs"] += [
            {"id": "010-merged", "stage": "merged", "project": "sample"},
            {"id": "011-closed", "stage": "closed", "project": "sample"},
            {"id": "012-build", "stage": "build", "project": "sample"},
        ]

    def request(self, home, method, path, body=None, ok=(200,), headers=None):
        self.paths.append(path)
        if path == "/api/board":
            return 200, {"ETag": '"fixture"'}, factory.dump_board(self.board)
        self.assertEqual(method, "GET")
        return 200, {}, "[]"

    def listings(self):
        return [p for p in self.paths if p != "/api/board"]

    def check_sync(self, *args):
        factory.sync(*args)
        self.assertEqual(self.listings(), ["/api/jobs/sample/012-build/?etags=1"])
        out = self.output.getvalue()
        self.assertIn("sample/012-build: pulled 0 files", out)
        self.assertNotIn("010-merged", out)
        self.assertNotIn("011-closed", out)

    def test_bulk_sync_skips_merged_and_closed_jobs(self):
        self.check_sync()

    def test_project_sync_skips_merged_and_closed_jobs(self):
        self.check_sync("sample")

    def test_pull_still_lists_a_merged_job(self):
        factory.pull("sample/010-merged")
        self.assertEqual(self.listings(), ["/api/jobs/sample/010-merged/?etags=1"])


class MoveFenceTest(IsolatedTestCase):
    setUp = FactoryTest.setUp
    api = FactoryTest.api
    git = FactoryTest.git
    saved = FactoryTest.saved
    save = FactoryTest.save

    def moved(self, **extra):
        job = dict(self.job, move=dict({"from": "test", "session": "sender", "to": "studio-mac",
                                        "at": "2026-11-08T10:00:00Z"}, **extra))
        del job["claim"]
        self.save(job)
        return self.store["/api/board"]

    def test_board_and_hold_fence_a_moved_session(self):
        before = self.moved()
        message = ("001-test was moved to studio-mac from the dashboard at 2026-11-08T10:00:00Z. "
                   "Nothing was written. Stop this session.\n")
        for call in (lambda: factory.board(self.home, "001-test"), lambda: factory.hold(self.target),
                     lambda: factory.hold(self.target, "released")):
            err = io.StringIO()
            with patch.object(factory.sys, "stdin", io.StringIO('{"note": "x"}')), \
                    contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
                call()
            self.assertEqual(raised.exception.code, 6)
            self.assertTrue(err.getvalue().endswith(message), err.getvalue())
            self.assertEqual(self.store["/api/board"], before)

    def test_a_second_move_keeps_earlier_sessions_fenced(self):
        before = self.moved(session="d4e5f6", to="cloud-box", at="2026-11-08T11:00:00Z",
                            earlier=[{"from": "test", "session": "sender", "at": "2026-11-08T10:00:00Z",
                                      "stopped": "2026-11-08T10:30:00Z"}])
        for call in (lambda: factory.board(self.home, "001-test"), lambda: factory.hold(self.target)):
            err = io.StringIO()
            with patch.object(factory.sys, "stdin", io.StringIO('{"note": "x"}')), \
                    contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
                call()
            self.assertEqual(raised.exception.code, 6)
            self.assertIn("001-test was moved to cloud-box from the dashboard at 2026-11-08T11:00:00Z.",
                          err.getvalue())
            self.assertEqual(self.store["/api/board"], before)
        factory.hold(self.target, "take")
        self.assertEqual(self.saved()["claim"]["session"], "sender")

    def test_move_fence_spares_other_sessions_and_hold_take(self):
        self.moved()
        with patch.dict(os.environ, {"FACTORY_SESSION": "other"}), \
                patch.object(factory.sys, "stdin", io.StringIO('{"note": "x"}')):
            factory.board(self.home, "001-test")
        self.assertEqual(self.saved()["note"], "x")
        factory.hold(self.target, "take")
        self.assertEqual(self.saved()["claim"]["session"], "sender")

    def test_board_refuses_a_move_patch(self):
        before = self.store["/api/board"]
        err = io.StringIO()
        with patch.object(factory.sys, "stdin", io.StringIO('{"move": {"to": "studio-mac"}}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("only the dashboard and factory.py pickup set move", err.getvalue())
        self.assertEqual(self.store["/api/board"], before)

    def test_board_refuses_a_priority_patch(self):
        before = self.store["/api/board"]
        err = io.StringIO()
        with patch.object(factory.sys, "stdin", io.StringIO('{"priority": {"at": "2026-01-02T12:00:00Z"}}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("only the dashboard, factory.py pickup and factory.py control set priority. To relay a person's "
                      "chat, run factory.py control <job-id> prioritize --chat-file <file>", err.getvalue())
        self.assertEqual(self.store["/api/board"], before)

    def test_board_refuses_a_paused_patch(self):
        before = self.store["/api/board"]
        err = io.StringIO()
        with patch.object(factory.sys, "stdin", io.StringIO('{"paused": {"at": "2026-01-02T12:00:00Z"}}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("only the dashboard sets paused", err.getvalue())
        self.assertEqual(self.store["/api/board"], before)

    def test_board_refuses_a_babysit_patch(self):
        before = self.store["/api/board"]
        err = io.StringIO()
        with patch.object(factory.sys, "stdin", io.StringIO('{"babysit": {"at": "2026-01-02T12:00:00Z"}}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("only the dashboard sets babysit; a session clears it with null", err.getvalue())
        self.assertEqual(self.store["/api/board"], before)

    def test_board_clears_babysit_with_null(self):
        self.save(dict(self.job, babysit={"at": "2026-01-02T12:00:00Z"}))
        self.assertTrue(factory.babysitting(self.saved()))
        with patch.object(factory.sys, "stdin", io.StringIO('{"babysit": null}')):
            factory.board(self.home, "001-test")
        self.assertNotIn("babysit", self.saved())
        self.assertFalse(factory.babysitting(self.saved()))

    def test_board_refuses_a_stacked_patch(self):
        before = self.store["/api/board"]
        err = io.StringIO()
        with patch.object(factory.sys, "stdin", io.StringIO('{"stacked": true}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("only factory.py stack sets stacked", err.getvalue())
        self.assertEqual(self.store["/api/board"], before)

    def test_board_refuses_a_close_patch(self):
        before = self.store["/api/board"]
        err = io.StringIO()
        with patch.object(factory.sys, "stdin", io.StringIO('{"close": {"reason": "x", "at": "2026-01-02T12:00:00Z"}}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("only the dashboard and factory.py pickup set close", err.getvalue())
        self.assertEqual(self.store["/api/board"], before)

    def test_board_releases_a_closing_claim(self):
        message = ("001-test has a close queued from the dashboard. Nothing else was written. "
                   "Stop this session.\n")
        self.save(dict(self.job, close={"reason": "Duplicate", "at": "2026-01-02T12:00:00Z"}))
        before = self.store["/api/board"]
        err = io.StringIO()
        with patch.dict(os.environ, {"FACTORY_SESSION": "other"}), \
                patch.object(factory.sys, "stdin", io.StringIO('{"note": "x"}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual(self.store["/api/board"], before)
        err = io.StringIO()
        with patch.object(factory.sys, "stdin", io.StringIO('{"note": "x"}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 6)
        self.assertTrue(err.getvalue().endswith(message), err.getvalue())
        job = self.saved()
        self.assertNotIn("claim", job)
        self.assertNotIn("note", job)
        self.assertEqual(job["close"], {"reason": "Duplicate", "at": "2026-01-02T12:00:00Z"})
        released = self.store["/api/board"]
        err = io.StringIO()
        with patch.object(factory.sys, "stdin", io.StringIO('{"note": "x"}')), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            factory.board(self.home, "001-test")
        self.assertEqual(raised.exception.code, 6)
        self.assertTrue(err.getvalue().endswith(message), err.getvalue())
        self.assertEqual(self.store["/api/board"], released)

    def test_hold_refuses_a_closing_job(self):
        self.save(dict(self.job, close={"reason": "Duplicate", "at": "2026-01-02T12:00:00Z"}))
        before = self.store["/api/board"]
        for take in (None, "take"):
            err = io.StringIO()
            with contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
                factory.hold(self.target, take)
            self.assertEqual(raised.exception.code, 6)
            self.assertIn("001-test has a close queued from the dashboard. Nothing else was written. "
                          "Stop this session.", err.getvalue())
            self.assertEqual(self.store["/api/board"], before)

    def test_stuck_skips_a_stale_closing_job(self):
        (self.home / "paths.md").write_text(f"---\nsample: {self.home / 'sample'}\n---\n")
        patch.object(factory, "paseo_server", return_value="").start()
        stale = dict(self.job, claim=dict(self.job["claim"], seen="2020-01-01T00:00:00Z"))
        self.save(dict(stale, close={"reason": "Duplicate", "at": "2026-01-02T12:00:00Z"}))
        before = self.store["/api/board"]
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.stuck()
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.store["/api/board"], before)
        self.save(stale)
        factory.stuck()
        self.assertEqual([j["id"] for j in json.loads(self.output.getvalue())], ["001-test"])

    def test_stuck_leaves_an_unclaimed_move_to_its_target(self):
        (self.home / "paths.md").write_text(f"---\nsample: {self.home / 'sample'}\n---\n")
        patch.object(factory, "paseo_server", return_value="").start()
        self.job["updated"] = "2020-01-01T00:00:00Z"
        before = self.moved()
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.stuck()
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.store["/api/board"], before)
        self.moved(claimed="2026-11-08T10:05:00Z")
        factory.stuck()
        self.assertEqual([j["id"] for j in json.loads(self.output.getvalue())], ["001-test"])

    def test_stuck_skips_a_stale_paused_job(self):
        (self.home / "paths.md").write_text(f"---\nsample: {self.home / 'sample'}\n---\n")
        patch.object(factory, "paseo_server", return_value="").start()
        stale = dict(self.job, claim=dict(self.job["claim"], seen="2020-01-01T00:00:00Z"))
        self.save(dict(stale, paused={"at": "2026-01-02T12:00:00Z"}))
        before = self.store["/api/board"]
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.stuck()
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(self.store["/api/board"], before)
        self.save(stale)
        factory.stuck()
        self.assertEqual([j["id"] for j in json.loads(self.output.getvalue())], ["001-test"])


class TracerStartIdeaTest(IsolatedTestCase):
    setUp = IdeaTest.setUp
    request = IdeaTest.request

    def test_ideas_skip_tracer_approved_build(self):
        self.board["jobs"].append({"id": "004-sample", "stage": "build", "project": "sample",
                                   "note": "spec approved in Tracer", "updated": "2026-01-01T00:00:00Z"})
        with self.assertRaises(SystemExit) as raised:
            factory.ideas("--limit", "1")
        self.assertEqual(raised.exception.code, 3)
        self.assertNotIn("claim", self.board["jobs"][-1])


class BacklogReworkTest(IsolatedTestCase):
    LINE = "- [ ] sample: Add CSV export to the weekly report (rework-of 002-add-csv-export)\n"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.job = {"id": "002-add-csv-export", "project": "sample", "stage": "idea",
                    "idea": "Add CSV export to reports", "claim": factory.new_claim("device", "session"),
                    "backlog": {"line": "- [ ] sample: Add CSV export to reports", "decision": "rework",
                                "at": "2026-11-02T10:00:00Z",
                                "notes": "Limit it to the weekly report and say which columns."}}
        self.docs = {"/api/board": factory.dump_board({"updated": "", "jobs": [self.job]}),
                     "/api/backlog": "# factory backlog\n\n- [x] sample: Add CSV export to reports (002-add-csv-export)\n"}
        self.puts = []
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory.time, "sleep").start()
        patch.object(factory, "api_url", lambda home: "https://factory.example").start()
        patch.object(factory, "api", side_effect=self.request).start()

    def request(self, home, method, path, body=None, ok=(200,), headers=None):
        self.assertIn(path, self.docs)
        text = self.docs[path]
        tag = '"%s"' % hashlib.sha256((text or "").encode()).hexdigest()
        if method == "GET":
            return (200, {"ETag": tag}, text) if text is not None else (404, {}, "")
        self.assertEqual(headers, {"If-Match": tag} if text is not None else {"If-None-Match": "*"})
        self.puts.append(path)
        self.docs[path] = body
        return 200, {}, ""

    def run_rework(self, stdin, job_id="002-add-csv-export"):
        with patch.object(factory.sys, "stdin", io.StringIO(stdin)):
            return invoke(factory, ["backlog", "rework", job_id])

    def board_job(self):
        return json.loads(self.docs["/api/board"])["jobs"][0]

    def test_appends_line_and_closes_job(self):
        code, out, err = self.run_rework("Add CSV export to the weekly report\n")
        self.assertEqual((code, err), (0, ""))
        self.assertTrue(self.docs["/api/backlog"].endswith(
            "(002-add-csv-export)\n" + self.LINE))
        job = self.board_job()
        self.assertEqual((job["stage"], job["note"]), ("closed", "reworked into the backlog"))
        self.assertIn("done", job)
        self.assertNotIn("claim", job)
        self.assertFalse(factory.deny_notes_due(job), "a closed rework job is not a deny-notes job")
        self.assertEqual(out, "added " + self.LINE + "link: https://factory.example/#/backlog/355d066e943c\n")

    def test_copies_context_block(self):
        block = "  Recommend: approve\n\n  The operator exports the board by hand today.\n"
        done = "- [x] sample: Add CSV export to reports (002-add-csv-export)\n"
        self.docs["/api/backlog"] = "# factory backlog\n\n" + done + block + "- [ ] factory: Other item\n"
        code, out, _ = self.run_rework("Add CSV export to the weekly report")
        self.assertEqual(code, 0)
        self.assertEqual(self.docs["/api/backlog"],
                         "# factory backlog\n\n" + done + block + "- [ ] factory: Other item\n" + self.LINE + block)
        self.assertTrue(out.startswith("added ") and " with context\nlink: https://factory.example/#/backlog/" in out)

    def test_ignores_other_lines_context(self):
        self.docs["/api/backlog"] += "- [x] sample: Other (003-other)\n  Recommend: approve\n"
        code, out, _ = self.run_rework("Add CSV export to the weekly report")
        self.assertEqual(code, 0)
        self.assertTrue(self.docs["/api/backlog"].endswith(self.LINE))
        self.assertNotIn("with context", out)

    def test_missing_or_malformed_line_adds_line_alone(self):
        start = "# factory backlog\n\n- [x] sample: Add CSV export to reports (002-add-csv-export)\n  Recommend: approve\n"
        for backlog in ({"decision": "rework"}, dict(self.job["backlog"], line=None),
                        dict(self.job["backlog"], line="- [x] sample: Add CSV export to reports")):
            with self.subTest(backlog=backlog):
                self.docs["/api/board"] = factory.dump_board({"updated": "", "jobs": [dict(self.job, backlog=backlog)]})
                self.docs["/api/backlog"] = start
                code, out, err = self.run_rework("Add CSV export to the weekly report")
                self.assertEqual((code, err), (0, ""))
                self.assertEqual(self.docs["/api/backlog"], start + self.LINE)
                self.assertEqual(out, "added " + self.LINE + "link: https://factory.example/#/backlog/355d066e943c\n")

    def test_non_string_line_exits_2(self):
        for line in (7, ["x"]):
            with self.subTest(line=line):
                job = dict(self.job, backlog=dict(self.job["backlog"], line=line))
                self.docs["/api/board"] = factory.dump_board({"updated": "", "jobs": [job]})
                code, out, err = self.run_rework("Add CSV export to the weekly report")
                self.assertEqual((code, out), (2, ""))
                self.assertEqual(err, f"factory.py: 002-add-csv-export has a bad backlog line {line!r}\n")
                self.assertEqual(self.puts, [])

    def test_rerun_adds_nothing(self):
        self.run_rework("Add CSV export to the weekly report")
        backlog = self.docs["/api/backlog"]
        self.docs["/api/board"] = factory.dump_board({"updated": "", "jobs": [self.job]})
        self.puts = []
        code, out, _ = self.run_rework("Add CSV export to the weekly report")
        self.assertEqual(code, 0)
        self.assertEqual(self.puts, ["/api/board"])
        self.assertEqual(self.docs["/api/backlog"], backlog)
        self.assertEqual(self.board_job()["stage"], "closed")
        self.assertEqual(out, "kept the existing line for 002-add-csv-export\n")

    def test_checked_line_counts_as_present(self):
        self.docs["/api/backlog"] = "- [x] sample: X (rework-of 002-add-csv-export) (003-x)\n"
        code, out, _ = self.run_rework("Add CSV export to the weekly report")
        self.assertEqual(code, 0)
        self.assertEqual(self.puts, ["/api/board"])
        self.assertEqual(out, "kept the existing line for 002-add-csv-export\n")

    def test_missing_job_exits_4(self):
        code, _, _ = self.run_rework("Add CSV export", "009-unknown")
        self.assertEqual(code, 4)
        self.assertEqual(self.puts, [])

    def test_bad_input_exits_2(self):
        cases = [("", None), ("one\ntwo", None), ("Add CSV export (rework-of 001-x)", None),
                 ("Add CSV export", {"backlog": {"decision": "approved"}}), ("Add CSV export", {"stage": "closed"}),
                 ("Add CSV export", {"project": "sample\n- [ ] other: Injected"}),
                 ("Add CSV export", {"project": "sample: x"}),
                 ("Add CSV export", {"project": 7}), ("Add CSV export", {"project": ["sample"]})]
        for stdin, change in cases:
            with self.subTest(stdin=stdin, change=change):
                job = dict(self.job, **(change or {}))
                self.docs["/api/board"] = factory.dump_board({"updated": "", "jobs": [job]})
                code, _, _ = self.run_rework(stdin)
                self.assertEqual(code, 2)
                self.assertEqual(self.puts, [])

    def test_suffix_guard_is_ascii(self):
        self.assertTrue(factory.BACKLOG_SUFFIX.search("Add (rework-of 001-x)"))
        self.assertIsNone(factory.BACKLOG_SUFFIX.search("Add (rework-of 001-é)"))

    def test_close_skip_says_why(self):
        approved = dict(self.job, backlog=dict(self.job["backlog"], decision="approved"))
        no_stage = {k: v for k, v in self.job.items() if k != "stage"}
        cases = [
            ([approved], "002-add-csv-export stayed at idea, not closed: its backlog decision changed after the line was written"),
            ([], "002-add-csv-export is gone from the board, not closed: it was removed after the line was written"),
            ([dict(self.job, stage="spec")], "002-add-csv-export moved to spec, not closed: its stage changed after the line was written"),
            ([dict(self.job, stage="closed")], "002-add-csv-export was already closed: it was closed after the line was written"),
            ([dict(self.job, stage=None)], "002-add-csv-export has no valid stage, not closed: its board stage is None"),
            ([no_stage], "002-add-csv-export has no valid stage, not closed: its board stage is None"),
            ([dict(self.job, stage=7)], "002-add-csv-export has no valid stage, not closed: its board stage is 7"),
        ]
        for board_jobs, head in cases:
            with self.subTest(head=head, jobs=board_jobs):
                self.docs["/api/board"] = factory.dump_board({"updated": "", "jobs": [self.job]})
                self.docs["/api/backlog"] = "# factory backlog\n\n- [x] sample: Add CSV export to reports (002-add-csv-export)\n"
                self.puts = []

                def request(home, method, path, body=None, ok=(200,), headers=None):
                    result = self.request(home, method, path, body, ok, headers)
                    if method == "PUT" and path == "/api/backlog":
                        self.docs["/api/board"] = factory.dump_board({"updated": "", "jobs": board_jobs})
                    return result
                factory.api.side_effect = request
                code, out, _ = self.run_rework("Add CSV export to the weekly report")
                self.assertEqual(code, 0)
                self.assertEqual(out, head + ". Backlog line: " + self.LINE)
                self.assertEqual(self.puts, ["/api/backlog"])

    def test_unknown_sub_exits_2(self):
        code, _, err = invoke(factory, ["backlog", "list", "x"])
        self.assertEqual(code, 2)
        self.assertIn("backlog rework <job-id>", err)


class AgentInstallTest(IsolatedTestCase):
    def test_init_installs_without_credentials_and_leaves_unrelated_files(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(Path, "home", return_value=Path(temp)):
            home = Path(temp) / "factory"
            config = Path(temp) / ".config"
            agents = config / "opencode" / "agents"
            agents.mkdir(parents=True)
            unrelated = agents / "user-reviewer.md"
            unrelated.write_text("user-owned")
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(config)}), patch.object(
                    factory, "factory_home", return_value=home), patch.object(factory, "api") as api:
                factory.init(temp, agents="both")
                factory.init(temp, agents="both")
                api.assert_not_called()
            self.assertEqual(unrelated.read_text(), "user-owned")
            self.assertEqual(len(list(agents.glob("factory-*.md"))), 9)

    def test_install_both_runtimes_and_preserve_opencode_model(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(Path, "home", return_value=Path(temp)):
            xdg = Path(temp) / "xdg"
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(xdg)}):
                skill = SKILL_ROOT
                factory.install_agents(skill, ("claude", "opencode"))
                agents = xdg / "opencode" / "agents"
                self.assertEqual(len(list(agents.glob("factory-*.md"))), 9)
                for source in (skill / "agents").glob("factory-*.md"):
                    self.assertEqual(source.read_bytes(), (Path(temp) / ".claude/agents" / source.name).read_bytes())
                    meta, body = factory.frontmatter(agents / source.name)
                    self.assertEqual(set(meta), {"description", "mode"})
                    self.assertEqual(meta["mode"], "all")
                    self.assertNotIn("Installed into ~/.claude", body)
                builder = agents / "factory-builder.md"
                builder.write_text(builder.read_text().replace('mode: "all"', 'mode: "all"\nmodel: provider/model'))
                factory.install_agents(skill, ("claude", "opencode"))
                self.assertEqual(factory.frontmatter(builder)[0]["model"], "provider/model")

    NATIVE_HEADERS = (
        ("inline comment pin",
         '---\ndescription: "Local tuning of the builder"\n'
         "model: provider/model # pinned locally\nmode: all\n---\n", False),
        ("quoted model with #variant",
         '---\nmodel: "provider/model#fast"\nmode: all\n---\n', False),
        ("crlf header",
         '---\r\ndescription: "CRLF role"\r\nmodel: provider/model\r\nmode: all\r\n---\r\n', False),
        ("multiline and expanded fields",
         "---\ndescription: >-\n  First line of a local description\n  second line\n"
         "model: provider/model\nvariant: fast\nmode: all\n"
         "permission:\n  bash: allow\ntemperature: 0.2\n---\n", False),
        ("closing delimiter at eof",
         "---\nmodel: provider/model\nmode: all\n---", True),
    )

    def test_repeated_init_preserves_native_header_and_refreshes_body(self):
        """An existing OpenCode role keeps its entire frontmatter verbatim; init only refreshes the prompt body."""
        with tempfile.TemporaryDirectory() as temp, patch.object(Path, "home", return_value=Path(temp)):
            xdg = Path(temp) / "xdg"
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(xdg)}):
                skill = SKILL_ROOT
                agents = xdg / "opencode" / "agents"
                factory.install_agents(skill, ("claude", "opencode"))
                for name, header, adds_newline in self.NATIVE_HEADERS:
                    with self.subTest(name):
                        destination = agents / "factory-builder.md"
                        destination.write_bytes(header.encode("utf-8"))
                        factory.install_agents(skill, ("claude", "opencode"))
                        data = destination.read_bytes()
                        self.assertTrue(data.startswith(header.encode("utf-8")), data[:80])
                        self.assertNotIn(b"---You", data)
                        body = data[len(header) + 1:] if adds_newline else data[len(header):]
                        self.assertTrue(body.decode("utf-8").startswith("You build one software factory job"),
                                        body[:80])

    def test_init_refuses_missing_or_unsupported_header_delimiters(self):
        """A header missing the `---` delimiters, or closed by the unsupported `...`
        document end, is refused, never silently overwritten."""
        with tempfile.TemporaryDirectory() as temp, patch.object(Path, "home", return_value=Path(temp)):
            xdg = Path(temp) / "xdg"
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(xdg)}):
                skill = SKILL_ROOT
                agents = xdg / "opencode" / "agents"
                agents.mkdir(parents=True)
                # The third shape probes the `...` YAML document end: the native v2.2.0
                # loader does not load such a file, so init refuses it too.
                for content in ("plain text, no frontmatter\n",
                                "---\nmodel: provider/model\n",
                                "---\nmode: all\n...\nbody\n"):
                    broken = agents / "factory-builder.md"
                    broken.write_text(content)
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
                        factory.install_agents(skill, ("claude", "opencode"))
                    self.assertIn("factory-builder.md", stderr.getvalue())
                    self.assertEqual(broken.read_text(), content)

    def test_init_keeps_invalid_yaml_content_inside_valid_delimiters(self):
        """Init validates only header delimiters: syntactically or schema-invalid
        YAML inside valid `---` delimiters is opaque, kept byte for byte, and the
        factory-owned body is still refreshed. No rejection and no partial YAML
        parsing; the runtime's native loader owns content validation."""
        with tempfile.TemporaryDirectory() as temp, patch.object(Path, "home", return_value=Path(temp)):
            xdg = Path(temp) / "xdg"
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(xdg)}):
                skill = SKILL_ROOT
                agents = xdg / "opencode" / "agents"
                factory.install_agents(skill, ("claude", "opencode"))
                # Unterminated quoted scalar: valid delimiters, invalid YAML content.
                # Isolated OpenCode v2.2.0 omits this role before and after init.
                invalid = ('---\ndescription: "Local tuning"\nmodel: "provider/model\n'
                           "mode: all\n---\n")
                destination = agents / "factory-builder.md"
                destination.write_bytes(invalid.encode("utf-8"))
                factory.install_agents(skill, ("claude", "opencode"))
                data = destination.read_bytes()
                self.assertTrue(data.startswith(invalid.encode("utf-8")), data[:80])
                self.assertNotIn(b"---You", data)
                body = data[len(invalid):].decode("utf-8")
                self.assertTrue(body.startswith("You build one software factory job"),
                                body[:80])

    def test_empty_xdg_config_home_falls_back_to_default(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(Path, "home", return_value=Path(temp)):
            skill = SKILL_ROOT
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": ""}):
                factory.install_agents(skill, ("claude", "opencode"))
            agents = Path(temp) / ".config" / "opencode" / "agents"
            self.assertEqual(len(list(agents.glob("factory-*.md"))), 9)


class ResumeInstructionTest(IsolatedTestCase):
    """Structural check on the resume instructions, not proof of model compliance."""

    def test_resume_approve_requires_a_fresh_approver(self):
        text = (SKILL_ROOT / "references" / "execution.md").read_text(encoding="utf-8")
        row = next(line for line in text.splitlines() if line.startswith("| approve |"))
        self.assertIn("fresh approver", row)
        self.assertIn("current HEAD and round", row)
        self.assertNotIn("Honor an existing decision", row)

    def test_resume_released_documents_strict_artifact_verification(self):
        text = (SKILL_ROOT / "references" / "execution.md").read_text(encoding="utf-8")
        self.assertIn("may already be held", text)
        self.assertIn("needs-human", text)
        self.assertIn("Do not dispatch", text)


class IdeaImagesTest(IsolatedTestCase):
    """Focused idea-images import tests: materialization, hash checks, refusal cases,
    retry after a partial push, and a second device pulling identical bytes."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory.time, "sleep").start()
        (self.home / "config.md").write_text("---\ndevice: test\n---\n")
        self.png1 = b"\x89PNG\r\n\x1a\n" + os.urandom(64)
        self.png2 = b"\x89PNG\r\n\x1a\n" + os.urandom(64)
        self.name1 = factory.sha(self.png1) + ".png"
        self.name2 = factory.sha(self.png2) + ".png"
        self.job = {"id": "001-idea", "project": "sample", "stage": "idea",
                    "idea": "with images", "idea_images": [self.name1, self.name2],
                    "capture_id": "11111111-1111-4111-8111-111111111111"}
        self.store = {"/api/board": factory.dump_board({"jobs": [self.job]}).encode(),
                      f"/api/idea-images/{self.name1}": self.png1,
                      f"/api/idea-images/{self.name2}": self.png2}
        self.fail_one_put = 0
        self.puts = []
        patch.object(factory, "api", side_effect=self.request).start()
        self.output = io.StringIO()
        self.redirect = contextlib.redirect_stdout(self.output)
        self.redirect.__enter__()
        self.addCleanup(self.redirect.__exit__, None, None, None)

    def request(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if method == "GET" and path.endswith("/?etags=1"):
            prefix = path[:-len("?etags=1")]
            rows = [{"name": k[len(prefix):], "etag": factory.sha(v)}
                    for k, v in self.store.items()
                    if k.startswith(prefix) and factory.JOB_REL.fullmatch(k[len(prefix):])]
            return 200, {}, json.dumps(rows)
        if path not in self.store:
            if method == "PUT":
                data, tag = None, None
            elif 404 in ok:
                return 404, {}, b"" if raw else ""
            else:
                raise AssertionError(f"unexpected missing API file {path}")
        data = self.store.get(path)
        tag = factory.sha(data) if data is not None else None
        if method == "PUT":
            if self.fail_one_put:
                self.fail_one_put -= 1
                raise SystemExit(5)
            self.puts.append(path)
            if headers and "If-Match" in headers and headers["If-Match"] != tag:
                return 412, {}, ""
            self.store[path] = body.encode() if isinstance(body, str) else body
            return 200, {"ETag": factory.sha(self.store[path])}, ""
        return 200, {"ETag": tag}, data if raw else data.decode()

    def job_file(self, rel, home=None):
        return (home or self.home) / "jobs" / "sample" / "001-idea" / rel

    def test_import_materializes_hashed_images_and_markdown_then_pushes(self):
        factory.idea_images("sample/001-idea")
        for name, raw in ((self.name1, self.png1), (self.name2, self.png2)):
            rel = f"idea-image-{name}"
            self.assertEqual(self.job_file(rel).read_bytes(), raw)
            self.assertIn(f"/api/jobs/sample/001-idea/{rel}", self.store)
        md = self.job_file("idea-images.md").read_text(encoding="utf-8")
        self.assertIn(f"![pasted image 1](idea-image-{self.name1})", md)
        self.assertIn(f"![pasted image 2](idea-image-{self.name2})", md)
        self.assertNotIn("<", md)  # no HTML, only Markdown image links
        self.assertIn("/api/jobs/sample/001-idea/idea-images.md", self.store)

    def test_import_of_a_job_without_images_writes_nothing(self):
        del self.job["idea_images"]
        self.store["/api/board"] = factory.dump_board({"jobs": [self.job]}).encode()
        factory.idea_images("sample/001-idea")
        self.assertIn("nothing to import", self.output.getvalue())
        self.assertEqual(self.puts, [])
        self.assertEqual(list(self.job_file("").rglob("*")), [])

    def test_import_stops_on_missing_or_corrupt_image_without_writing(self):
        del self.store[f"/api/idea-images/{self.name2}"]
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 5)
        self.assertEqual(list(self.job_file("").rglob("*")), [])
        self.store[f"/api/idea-images/{self.name2}"] = self.png2[:-1]  # hash mismatch
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 5)
        self.assertEqual(list(self.job_file("").rglob("*")), [])

    def test_import_refuses_projectless_and_malformed_metadata(self):
        self.job["project"] = None
        self.store["/api/board"] = factory.dump_board({"jobs": [self.job]}).encode()
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 2)
        self.job["project"] = "sample"
        self.job["idea_images"] = ["../escape.png"]
        self.store["/api/board"] = factory.dump_board({"jobs": [self.job]}).encode()
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual(self.puts, [])
        self.assertEqual(list(self.job_file("").rglob("*")), [])

    def test_import_preserves_a_differing_local_file_and_unresolved_remote(self):
        self.job_file("").mkdir(parents=True)
        self.job_file("idea-images.md").write_text("a human's edit\n", encoding="utf-8")
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual(self.job_file("idea-images.md").read_text(encoding="utf-8"), "a human's edit\n")
        os.remove(self.job_file("idea-images.md"))
        self.job_file("idea-images.md.remote").write_text("unmerged\n", encoding="utf-8")
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 6)
        self.assertTrue(self.job_file("idea-images.md.remote").exists())
        self.assertEqual(self.puts, [])

    def test_import_retries_after_a_partial_push(self):
        self.fail_one_put = 1
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 5)
        factory.idea_images("sample/001-idea")
        self.assertEqual(sorted(p.rsplit("/", 1)[-1] for p in self.puts),
                         sorted([f"idea-image-{self.name1}", f"idea-image-{self.name2}", "idea-images.md"]))
        self.assertEqual(self.job_file(f"idea-image-{self.name1}").read_bytes(), self.png1)

    def test_import_stops_when_the_remote_copy_of_an_import_moves(self):
        factory.idea_images("sample/001-idea")
        local = self.job_file("idea-images.md").read_text(encoding="utf-8")
        self.store["/api/jobs/sample/001-idea/idea-images.md"] = b"replaced in R2\n"
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual(self.job_file("idea-images.md").read_text(encoding="utf-8"), local)
        del self.store["/api/jobs/sample/001-idea/idea-images.md"]
        self.store["/api/jobs/sample/001-idea/idea-images.md"] = factory.dump_board({"jobs": []}).encode()
        del self.store[f"/api/jobs/sample/001-idea/idea-image-{self.name1}"]
        with self.assertRaises(SystemExit) as raised:
            factory.idea_images("sample/001-idea")
        self.assertEqual(raised.exception.code, 6)
        self.assertTrue(self.job_file(f"idea-image-{self.name1}").exists())

    def test_another_device_pulls_identical_bytes_and_links(self):
        factory.idea_images("sample/001-idea")
        other = Path(tempfile.mkdtemp(dir=self.temp.name))
        (other / "config.md").write_text("---\ndevice: other\n---\n")
        patch.object(factory, "factory_home", return_value=other).start()
        factory.pull("sample/001-idea")
        for rel in (f"idea-image-{self.name1}", f"idea-image-{self.name2}", "idea-images.md"):
            self.assertEqual(factory.sha(self.job_file(rel, other).read_bytes()),
                             factory.sha(self.job_file(rel).read_bytes()), rel)
        md = self.job_file("idea-images.md", other).read_text(encoding="utf-8")
        self.assertIn(f"](idea-image-{self.name1})", md)


class ApiCertFallbackTest(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.home)
        self.addCleanup(patch.stopall)
        patch.object(factory, "api_url", return_value="https://factory.example.test").start()
        self.opener = MagicMock()
        self.build_opener = patch.object(factory.urllib.request, "build_opener", return_value=self.opener).start()
        self.context = patch.object(factory.ssl, "create_default_context").start()

    def cert_error(self):
        return urllib.error.URLError(ssl.SSLCertVerificationError("synthetic"))

    def test_cert_error_retries_with_fallback_bundle(self):
        response = MagicMock(status=200, headers={"ETag": '"v1"'})
        response.read.return_value = b'{"jobs": []}'
        response.__enter__.return_value = response
        self.opener.open.side_effect = [self.cert_error(), response]
        with patch.object(factory.os.path, "isfile", return_value=True), \
                patch.dict(os.environ, {"SSL_CERT_FILE": "", "SSL_CERT_DIR": ""}):
            status, _, text = factory.api(self.home, "GET", "/api/board")
        self.assertEqual((status, text), (200, '{"jobs": []}'))
        self.assertEqual(self.opener.open.call_count, 2)
        self.context.assert_called_once_with(cafile=factory.FALLBACK_CA_FILE)

    def test_cert_error_respects_ssl_cert_file(self):
        self.opener.open.side_effect = [self.cert_error()]
        with patch.object(factory.os.path, "isfile", return_value=True), \
                patch.dict(os.environ, {"SSL_CERT_FILE": "/synthetic/ca.pem", "SSL_CERT_DIR": ""}), \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
            factory.api(self.home, "GET", "/api/board")
        self.assertEqual(caught.exception.code, 5)
        self.assertEqual(self.opener.open.call_count, 1)
        self.context.assert_not_called()

    def test_cert_error_respects_ssl_cert_dir(self):
        self.opener.open.side_effect = [self.cert_error()]
        with patch.object(factory.os.path, "isfile", return_value=True), \
                patch.dict(os.environ, {"SSL_CERT_FILE": "", "SSL_CERT_DIR": "/synthetic/certs"}), \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
            factory.api(self.home, "GET", "/api/board")
        self.assertEqual(caught.exception.code, 5)
        self.assertEqual(self.opener.open.call_count, 1)
        self.context.assert_not_called()

    def test_cert_error_without_fallback_file_explains(self):
        self.opener.open.side_effect = [self.cert_error()]
        err = io.StringIO()
        with patch.object(factory.os.path, "isfile", return_value=False), \
                patch.dict(os.environ, {"SSL_CERT_FILE": "", "SSL_CERT_DIR": ""}), \
                contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as caught:
            factory.api(self.home, "GET", "/api/board")
        self.assertEqual(caught.exception.code, 5)
        self.assertEqual(self.opener.open.call_count, 1)
        self.assertIn("Set SSL_CERT_FILE", err.getvalue())

    def test_network_error_text_is_printable(self):
        self.opener.open.side_effect = [OSError("timed out\x1b[2J")]
        err = io.StringIO()
        with contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as caught:
            factory.api(self.home, "GET", "/api/board")
        self.assertEqual((caught.exception.code, err.getvalue()),
                         (5, "factory.py: can't reach https://factory.example.test/api/board: timed out [2J\n"))


class JobRelTest(unittest.TestCase):
    def test_walkthrough_html_names(self):
        for rel in ("spec.md", "round-1/review-code.md", "shot.webp", "walkthrough.html",
                    "build.walkthrough.html", "decision.walkthrough.html",
                    "round-1/review-ui.walkthrough.html", "rework-2.walkthrough.html"):
            self.assertTrue(factory.JOB_REL.fullmatch(rel), rel)
        for rel in ("round-1/walkthrough.html", "other.html", "walkthrough.html.remote", "a.svg",
                    "a.b.walkthrough.html", ".walkthrough.html", "build.walkthrough.htm",
                    "round-1/x/y.walkthrough.html"):
            self.assertIsNone(factory.JOB_REL.fullmatch(rel), rel)


class NoShadowedDefsTest(unittest.TestCase):
    def test_no_top_level_name_defined_twice(self):
        tree = ast.parse((ROOT / "skills/factory/runtime/factory.py").read_text(encoding="utf-8"))
        names = [node.name for node in tree.body
                 if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))]
        self.assertEqual(sorted({n for n in names if names.count(n) > 1}), [])


if __name__ == "__main__":
    unittest.main()
