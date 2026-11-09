"""Local Worker project API via the installed CLI copy; never follows redirects.

Installs the merged device bundle into a temporary directory once per run and loads
the installed runtime by its absolute path, with checkout imports unavailable."""
import concurrent.futures
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "tests" / "fixtures" / "project-settings.md"
PROJECT = "/api/projects/settings-sample.md"
OWNED = ("http://127.0.0.1:8220", "http://127.0.0.1:8221")


def test_url():
    url = os.environ.get("FACTORY_TEST_URL")
    if url is not None and url not in OWNED:
        raise ValueError("FACTORY_TEST_URL must be an owned loopback origin: " + " or ".join(OWNED))
    return url


def install_cli(test_case):
    """Install the device bundle once and load the installed runtime copy."""
    temp = tempfile.TemporaryDirectory()
    test_case.addClassCleanup(temp.cleanup)
    # macOS returns /var/... temporary paths through a /private/var symlink; the
    # installer's link guard correctly refuses symlinked parents, so resolve first.
    destination = Path(temp.name).resolve() / "installed"
    result = subprocess.run(
        ["python3", "-B", str(ROOT / "tools" / "install.py"), "--destination", str(destination)],
        capture_output=True, text=True, env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})
    if result.returncode != 0:
        raise AssertionError("install.py failed: " + result.stderr)
    installed = destination / "factory" / "runtime" / "factory.py"
    if not installed.is_file() or not str(installed).startswith(str(destination)):
        raise AssertionError("the tested CLI must be the installed copy, not the checkout")
    spec = importlib.util.spec_from_file_location("factory_installed", installed)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class LocalTargetTest(unittest.TestCase):
    def test_refuses_other_targets_before_network(self):
        with patch.object(urllib.request, "build_opener") as network:
            for url in ("https://factory.example.test", "http://localhost:8220", "http://127.0.0.1:8220/",
                        "http://127.0.0.1:8210", "http://user@127.0.0.1:8220", "http://127.0.0.1:8220?x=1",
                        "http://127.0.0.1:8220#x", "http://127.0.0.1:8220/api"):
                with patch.dict(os.environ, {"FACTORY_TEST_URL": url}), self.assertRaises(ValueError):
                    test_url()
            network.assert_not_called()


class ProjectApiTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.url = test_url()
        if cls.url is None:
            raise unittest.SkipTest("set FACTORY_TEST_URL to run local R2 integration")
        cls.factory = install_cli(cls)

    def request(self, method="GET", data=None, headers=None):
        req = urllib.request.Request(self.url + PROJECT, data, headers or {}, method=method)
        try:
            with urllib.request.build_opener(self.factory.NoRedirect).open(req, timeout=30) as r:
                return r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def put(self, data, tag):
        return self.request("PUT", data, {"Content-Type": "text/markdown; charset=utf-8", "If-Match": tag})

    def setUp(self):
        self.base = FIXTURE.read_bytes()
        status, headers, _ = self.request()
        self.assertIn(status, (200, 404))
        condition = {"If-Match": self.factory.strong(headers.get("ETag"))} if status == 200 else {"If-None-Match": "*"}
        status, headers, _ = self.request("PUT", self.base, {"Content-Type": "text/markdown", **condition})
        self.assertEqual(status, 200)
        self.tag = self.factory.strong(headers["ETag"])
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)

    def home(self, name):
        home = Path(self.temp.name) / name
        home.mkdir()
        (home / "config.md").write_text(f"---\napi_url: {self.url}\n---\n")
        (home / ".env").write_text("")
        (home / "paths.md").write_text(f"---\nsettings-sample: {home}\n---\n")
        self.factory.pull_project(home, "settings-sample")
        return home

    def push(self, home):
        try:
            self.factory.push_project(home, "settings-sample")
            return 0
        except SystemExit as e:
            return e.code

    def test_real_atomic_race_and_readback(self):
        gate = threading.Barrier(2)
        def write(text):
            gate.wait(timeout=10)
            return self.put(self.base + text, self.tag)
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            results = list(pool.map(write, (b"A", b"B")))
        self.assertEqual(sorted(r[0] for r in results), [200, 412])
        winner = next(i for i, r in enumerate(results) if r[0] == 200)
        status, headers, body = self.request()
        self.assertEqual(status, 200)
        self.assertEqual(body, self.base + (b"A", b"B")[winner])
        self.assertEqual(self.factory.strong(headers["ETag"]), self.factory.strong(results[winner][1]["ETag"]))

    def test_validation_and_baseless_stale_writes_leave_bytes(self):
        cases = [
            ({}, self.base, 428),
            ({"If-Match": '"a", "b"'}, self.base, 400),
            ({"If-Match": self.tag, "Content-Type": "application/json"}, self.base, 415),
            ({"If-Match": self.tag, "Content-Type": "text/markdown"}, self.base.replace(b"base: main", b"path: /device\nbase: main"), 400),
            ({"If-Match": self.tag, "Content-Type": "text/markdown"}, b"\xff", 400),
            ({"If-Match": self.tag, "Content-Type": "text/markdown"}, self.base + b"x" * (1 << 20), 413),
            ({"If-Match": '"stale"', "Content-Type": "text/markdown"}, self.base, 412),
            ({"If-None-Match": "*", "Content-Type": "text/markdown"}, self.base, 412),
        ]
        for headers, data, status in cases:
            with self.subTest(status=status):
                self.assertEqual(self.request("PUT", data, headers)[0], status)
                self.assertEqual(self.request()[2], self.base)
                self.assertEqual(self.factory.strong(self.request()[1]["ETag"]), self.tag)

    def test_two_device_race_conflict_merge_and_where(self):
        factory = self.factory
        homes = [self.home("A"), self.home("B")]
        drafts = [self.base + b"Device A", self.base + b"Device B"]
        for home, data in zip(homes, drafts):
            factory.project_files(home, "settings-sample")[0].write_bytes(data)
        gate = threading.Barrier(2)
        real_api = factory.api
        calls = []
        def api(home, method, *args, **kwargs):
            calls.append(method)
            if method == "PUT":
                gate.wait(timeout=10)
            return real_api(home, method, *args, **kwargs)
        with patch.object(factory, "api", side_effect=api), concurrent.futures.ThreadPoolExecutor(2) as pool:
            results = list(pool.map(self.push, homes))
        self.assertEqual(sorted(results), [0, 6])
        self.assertEqual(calls, ["PUT", "PUT"])
        winner, loser = results.index(0), results.index(6)
        self.assertEqual(self.request()[2], drafts[winner])
        doc, state, remote = factory.project_files(homes[loser], "settings-sample")
        self.assertEqual(doc.read_bytes(), drafts[loser])
        with self.assertRaises(SystemExit) as raised:
            factory.pull_project(homes[loser], "settings-sample")
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual(remote.read_bytes(), drafts[winner])
        self.assertEqual(doc.read_bytes(), drafts[loser])
        doc.write_bytes(self.base + b"Merged explicitly")
        remote.unlink()
        self.assertEqual(self.push(homes[loser]), 0)
        out = io.StringIO()
        with patch.object(factory, "factory_home", return_value=homes[loser]), contextlib.redirect_stdout(out):
            factory.where("settings-sample")
        got = json.loads(out.getvalue())
        self.assertEqual(Path(got["project_file"]).read_bytes(), self.request()[2])
        self.assertEqual(json.loads(state.read_text())["etag"], factory.strong(self.request()[1]["ETag"]))

    def test_browser_like_and_device_conflicts_both_orders(self):
        factory = self.factory
        home = self.home("device")
        doc = factory.project_files(home, "settings-sample")[0]
        doc.write_bytes(self.base + b"Device")
        self.assertEqual(self.put(self.base + b"Browser", self.tag)[0], 200)
        self.assertEqual(self.push(home), 6)
        self.assertEqual(doc.read_bytes(), self.base + b"Device")
        doc.write_bytes(self.base)
        factory.pull_project(home, "settings-sample")
        browser_tag = factory.strong(self.request()[1]["ETag"])
        doc.write_bytes(self.base + b"Device second")
        self.assertEqual(self.push(home), 0)
        self.assertEqual(self.put(self.base + b"Browser second", browser_tag)[0], 412)
        self.assertEqual(self.request()[2], self.base + b"Device second")


if __name__ == "__main__":
    unittest.main()
