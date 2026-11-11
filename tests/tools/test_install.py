import contextlib
import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "cli"))
from support import ROOT, SKILL, IsolatedTestCase, snapshot


class InstallTest(IsolatedTestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("install", ROOT / "tools/install.py")
        self.installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.installer)
        self.root = Path(os.environ["HOME"]).resolve()
        self.destination = self.root / "skills"

    def install(self, replace=False, source=SKILL):
        with contextlib.redirect_stdout(io.StringIO()) as out:
            result = self.installer.install(source, self.destination, replace)
        return result, out.getvalue().splitlines()

    def refused(self, call):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            call()
        self.assertEqual(error.exception.code, 2)

    def test_exact_copy_idempotence_and_unrelated_skills(self):
        self.destination.mkdir()
        other = self.destination / "other"
        other.mkdir()
        (other / "SKILL.md").write_text("other skill")
        target, lines = self.install()
        self.assertEqual(lines, [str(target)])
        installed = self.installer.tree(target)[0]
        self.assertEqual(set(installed), set(self.installer.REQUIRED) | {"bundle.json"})
        del installed["bundle.json"]
        self.assertEqual(installed, self.installer.tree(SKILL, source=True)[0])
        before = snapshot(self.destination)
        self.assertEqual(self.install()[0], target)
        self.assertEqual(snapshot(self.destination), before)
        self.assertEqual((other / "SKILL.md").read_text(), "other skill")
        self.assertFalse((self.destination / ".factory-install.lock").exists())

    def test_git_checkout_stamps_commit_and_tree(self):
        target, _ = self.install()
        stamp = (target / "bundle.json").read_text()
        head = subprocess.run(["git", "-C", str(SKILL), "rev-parse", "HEAD"], capture_output=True, text=True)
        files = self.installer.tree(SKILL, source=True)[0]
        tree = hashlib.sha256(b"".join(rel.encode() + b"\0" + hashlib.sha256(files[rel]).digest()
                                       for rel in sorted(files))).hexdigest()
        self.assertRegex(head.stdout.strip(), r"^[0-9a-f]{40}$")
        self.assertEqual(stamp, json.dumps({"commit": head.stdout.strip(), "tree": tree}, sort_keys=True) + "\n")
        spec = importlib.util.spec_from_file_location("installed", target / "runtime/factory.py")
        runtime = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runtime)
        self.assertEqual(runtime.bundle_stamp(), json.loads(stamp))

    def test_refusal_and_replacement_retain_unique_backups(self):
        target, _ = self.install()
        (target / "SKILL.md").write_text("locally edited")
        old = snapshot(target)
        self.refused(self.install)
        self.assertEqual(snapshot(target), old)
        target, lines = self.install(replace=True)
        self.assertEqual(len(lines), 2)
        first = Path(lines[1])
        self.assertEqual(snapshot(first), old)
        (target / "SKILL.md").write_text("second edit")
        _, next_lines = self.install(replace=True)
        self.assertNotEqual(lines[1], next_lines[1])
        self.assertEqual(snapshot(first), old)

    def test_exclusive_lock_is_never_broken(self):
        self.destination.mkdir()
        lock = self.destination / ".factory-install.lock"
        lock.write_text("owned by another installer")
        before = snapshot(self.destination)
        self.refused(self.install)
        self.assertEqual(snapshot(self.destination), before)

    def test_switch_failure_restores_old_target_without_removing_other_backups(self):
        target, _ = self.install()
        (target / "SKILL.md").write_text("old contents")
        old = snapshot(target)
        rename = os.rename
        def fail_switch(source, destination):
            if Path(source).name.startswith(".factory-stage-"):
                raise OSError("switch interrupted")
            return rename(source, destination)
        with patch.object(self.installer.os, "rename", side_effect=fail_switch):
            self.refused(lambda: self.install(replace=True))
        self.assertEqual(snapshot(target), old)
        self.assertFalse(list(self.destination.glob(".factory-stage-*")))
        self.assertFalse((self.destination / ".factory-install.lock").exists())

    def test_competing_target_is_not_overwritten_and_backup_survives(self):
        target, _ = self.install()
        (target / "SKILL.md").write_text("old contents")
        rename = os.rename
        def competitor(source, destination):
            result = rename(source, destination)
            if Path(destination).name.startswith("factory.backup-"):
                target.mkdir()
                (target / "other").write_text("competing target")
            return result
        with patch.object(self.installer.os, "rename", side_effect=competitor):
            self.refused(lambda: self.install(replace=True))
        self.assertEqual((target / "other").read_text(), "competing target")
        backups = list(self.destination.glob("factory.backup-*"))
        self.assertEqual(len(backups), 1)
        self.assertEqual((backups[0] / "SKILL.md").read_text(), "old contents")

    def test_detected_destination_edit_and_staging_failure_leave_old_install(self):
        target, _ = self.install()
        (target / "SKILL.md").write_text("old")
        tree = self.installer.tree
        def changed(root, source=False):
            if root == target and not source:
                before = tree(root, source)
                (target / "SKILL.md").write_text("new external edit")
                return before
            return tree(root, source)
        with patch.object(self.installer, "tree", side_effect=changed):
            self.refused(lambda: self.install(replace=True))
        self.assertEqual((target / "SKILL.md").read_text(), "new external edit")
        self.assertFalse(list(self.destination.glob("factory.backup-*")))
        with patch.object(self.installer.tempfile, "mkdtemp", side_effect=OSError("no space")):
            self.refused(lambda: self.install(replace=True))
        self.assertEqual((target / "SKILL.md").read_text(), "new external edit")

    def test_unsafe_paths_inventory_and_source_cache_exclusion(self):
        self.refused(lambda: self.installer.install(SKILL, Path("relative")))
        self.refused(lambda: self.installer.install(SKILL, SKILL / "inside"))
        clone = self.root / "source"
        shutil.copytree(SKILL, clone)
        cached = clone / "__pycache__"
        cached.mkdir()
        (cached / "factory.pyc").write_bytes(b"not shipped")
        (clone / ".development").write_text("not shipped")
        self.install(source=clone)
        self.assertFalse((self.destination / "factory/__pycache__").exists())
        self.assertFalse((self.destination / "factory/bundle.json").exists())
        missing = clone / "agents/factory-ux.md"
        missing.write_text("unexpected role")
        self.refused(lambda: self.install(source=clone))
        missing.unlink()
        (clone / "SKILL.md").unlink()
        self.refused(lambda: self.install(source=clone))
        if os.name != "nt":
            (clone / "SKILL.md").symlink_to(SKILL / "SKILL.md")
            self.refused(lambda: self.install(source=clone))
            alias = self.root / "alias"
            alias.symlink_to(self.destination, target_is_directory=True)
            self.refused(lambda: self.installer.install(SKILL, alias))

    def test_file_targets_and_parent_traversal_are_refused_without_writes(self):
        self.destination.mkdir()
        (self.destination / "factory").write_text("not a skill directory")
        before = snapshot(self.destination)
        self.refused(self.install)
        self.assertEqual(snapshot(self.destination), before)
        self.refused(lambda: self.installer.install(SKILL, self.root / "part/../skills"))

    def test_installed_only_offline_setup_adoption_roles_and_http_conflict(self):
        distribution = self.root / "distribution"
        (distribution / "tools").mkdir(parents=True)
        shutil.copyfile(ROOT / "tools/install.py", distribution / "tools/install.py")
        shutil.copytree(SKILL, distribution / "skills/factory")
        unrelated = self.root / "unrelated"
        unrelated.mkdir()
        subprocess.run([sys.executable, "-I", "-B", str(distribution / "tools/install.py"), "--destination", str(self.destination)],
                       cwd=unrelated, check=True, capture_output=True)
        shutil.rmtree(distribution)
        cli = self.destination / "factory/runtime/factory.py"
        def command(*args, input=None, expected=0):
            result = subprocess.run([sys.executable, "-I", "-B", str(cli)] + list(args), cwd=unrelated,
                                    input=input, capture_output=True, text=True)
            self.assertEqual(result.returncode, expected, result.stderr)
            self.assertNotIn("synthetic-secret", result.stdout + result.stderr)
            return result
        home = self.root / "retained-state"
        result = command("--home", str(home), "init", "--agents", "both")
        self.assertIn("Set api_url", result.stdout)
        self.assertEqual(len(list((self.root / ".claude/agents").glob("*.md"))), 9)
        command("adopt-home", str(home))
        command("doctor", expected=2)
        store = {"/projects.json": b'[{"slug":"sample","repo":"example/sample"}]',
                 "/api/projects/sample.md": b"---\nrepo: example/sample\nbase: main\n---\nProject\n",
                 "/api/board": b'{"jobs":[]}'}
        requests = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def respond(self, status, data):
                self.send_response(status)
                self.send_header("ETag", '"' + hashlib.sha256(data).hexdigest() + '"')
                self.end_headers()
                self.wfile.write(data)
            def do_GET(self):
                requests.append(("GET", self.path))
                if self.path.endswith("/?etags=1"):
                    prefix = self.path[:-len("?etags=1")]
                    data = json.dumps([{"name": key[len(prefix):], "etag": '"' + hashlib.sha256(value).hexdigest() + '"'}
                                       for key, value in store.items() if key.startswith(prefix)]).encode()
                    return self.respond(200, data)
                self.respond(200 if self.path in store else 404, store.get(self.path, b""))
            def do_PUT(self):
                requests.append(("PUT", self.path))
                data = self.rfile.read(int(self.headers["Content-Length"]))
                old = store.get(self.path)
                tag = '"' + hashlib.sha256(old).hexdigest() + '"' if old is not None else None
                if (self.headers.get("If-None-Match") == "*" and old is not None or
                        self.headers.get("If-Match") is not None and self.headers["If-Match"] != tag):
                    return self.respond(412, b"changed")
                store[self.path] = data
                self.respond(200, data)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            (home / "config.md").write_text("---\napi_url: http://127.0.0.1:" + str(server.server_port) + "\ndevice: test\n---\n")
            (home / ".env").write_text("FACTORY_ACCESS_CLIENT_ID=synthetic-id\nFACTORY_ACCESS_CLIENT_SECRET=synthetic-secret\n")
            report = json.loads(command("doctor").stdout)
            self.assertTrue(all(c["status"] == "pass" for c in report["checks"]))
            self.assertEqual(requests, [("GET", "/projects.json"), ("GET", "/api/board"), ("PUT", "/api/hosts/test/status")])
            self.assertIsNone(json.loads(store.pop("/api/hosts/test/status"))["bundle"])
            (home / "paths.md").write_text("---\nsample: " + str(unrelated) + "\n---\n")
            self.assertEqual(json.loads(command("where", "sample").stdout)["repo"], "example/sample")
            claim = command("claim", str(home), "test", "sample")
            job_id = claim.stdout.strip()
            self.assertEqual(job_id, "001-test")
            self.assertIn("new session", claim.stderr)
            session = json.loads(store["/api/board"])["jobs"][0]["claim"]["session"]
            command("board", str(home), job_id, "--session", session, input='{"stage":"build"}')
            artifact = home / "jobs/sample" / job_id / "build.md"
            artifact.write_text("initial")
            command("push", "sample/" + job_id)
            artifact.write_text("local edit")
            store["/api/jobs/sample/" + job_id + "/build.md"] = b"remote edit"
            command("sync", "sample", expected=6)
            self.assertEqual(artifact.read_text(), "local edit")
            self.assertEqual(artifact.with_name("build.md.remote").read_bytes(), b"remote edit")
            command("push", "sample/" + job_id, expected=6)
            command("init", "--agents", "both")
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
        self.assertFalse(distribution.exists())
