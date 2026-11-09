"""Actual HTTP v1 drills through the installed CLI copy: concurrent claims, conditional
registration, job write grammar, weak ETags, idea-image idempotence and two-device
job/project exit-6 reconciliation. Installed into temporary HOME, checkout imports
unavailable, synthetic Git checkouts, owned loopback target only."""
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from unittest.mock import patch

from test_project_api import install_cli, test_url, OWNED

ROOT = Path(__file__).resolve().parents[2]


def png_bytes():
    """A real 1x1 RGBA PNG built from stdlib parts."""
    import zlib
    import struct
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    header = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(b"\x00\x00\x00\x00\xff"))
            + chunk(b"IEND", b""))


PNG = png_bytes()
PNG_NAME = hashlib.sha256(PNG).hexdigest() + ".png"


def strong(tag):
    if not isinstance(tag, str) or not re.fullmatch(r'(W/)?"[\x21\x23-\x7e\x80-\xff]+"', tag):
        raise ValueError("missing or malformed ETag")
    return tag[2:] if tag.startswith("W/") else tag


class LocalTargetTest(unittest.TestCase):
    def test_guard_before_network(self):
        with patch.object(urllib.request, "build_opener") as network:
            for url in ("https://factory.example.test", "http://localhost:8220", "http://127.0.0.1:8220/",
                        "http://127.0.0.1:8210", "http://user@127.0.0.1:8220"):
                with patch.dict(os.environ, {"FACTORY_TEST_URL": url}), self.assertRaises(ValueError):
                    test_url()
            network.assert_not_called()


class HttpV1Test(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.url = test_url()
        if cls.url is None:
            raise unittest.SkipTest("set FACTORY_TEST_URL for actual local HTTP drills")
        cls.factory = install_cli(cls)

    def request(self, path, method="GET", data=None, headers=None):
        req = urllib.request.Request(self.url + path, data, headers or {}, method=method)
        try:
            with urllib.request.build_opener(self.factory.NoRedirect).open(req, timeout=30) as r:
                return r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)

    def home(self, name):
        home = Path(self.temp.name) / name
        home.mkdir()
        (home / "config.md").write_text(f"---\napi_url: {self.url}\n---\n")
        (home / ".env").write_text("")
        return home

    def git_repo(self, name):
        repo = Path(self.temp.name) / name
        repo.mkdir()
        env = {**os.environ, "GIT_CONFIG_NOSYSTEM": "1",
               "GIT_AUTHOR_NAME": "Test", "GIT_AUTHOR_EMAIL": "test@example.invalid",
               "GIT_COMMITTER_NAME": "Test", "GIT_COMMITTER_EMAIL": "test@example.invalid"}
        subprocess.run(["git", "init"], cwd=repo, check=True, capture_output=True, env=env)
        subprocess.run(["git", "symbolic-ref", "HEAD", "refs/heads/main"], cwd=repo, check=True, capture_output=True, env=env)
        (repo / "README.md").write_text("# synthetic checkout\n")
        subprocess.run(["git", "add", "README.md"], cwd=repo, check=True, capture_output=True, env=env)
        subprocess.run(["git", "commit", "-m", "init"], cwd=repo, check=True, capture_output=True, env=env)
        return repo

    def git_state(self, repo):
        status = subprocess.run(["git", "status", "--porcelain"], cwd=repo, check=True,
                                capture_output=True, text=True).stdout
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo, check=True,
                              capture_output=True, text=True).stdout.strip()
        return status, head

    def test_concurrent_board_claims(self):
        factory = self.factory
        homes = [self.home("claim-a"), self.home("claim-b")]
        gate = threading.Barrier(2)
        gated = [0]
        real_api = factory.api
        def api(home, method, *args, **kwargs):
            result = real_api(home, method, *args, **kwargs)
            if method == "GET" and args and args[0] == "/api/board" and gated[0] < 2:
                gated[0] += 1
                gate.wait(timeout=10)
            return result
        with patch.object(factory, "api", side_effect=api), concurrent.futures.ThreadPoolExecutor(2) as pool:
            ids = list(pool.map(lambda home: factory.claim_id(home, "synthetic-claim", "sample"), homes))
        self.assertEqual(len(set(ids)), 2, "concurrent claims get distinct ids")
        status, _, body = self.request("/api/board")
        self.assertEqual(status, 200)
        jobs = json.loads(body)["jobs"]
        claimed = [j for j in jobs if j.get("id") in ids]
        self.assertEqual(len(claimed), 2)
        for job in claimed:
            self.assertEqual(job["project"], "sample")
            self.assertNotIn("stage", job, "a claim stub carries no stage")
            self.assertTrue(job.get("claim", {}).get("device"))
        for home, job_id in zip(homes, ids):
            self.assertTrue((home / "jobs" / "sample" / job_id).is_dir())

    def test_project_conditional_registration_race(self):
        path = "/api/projects/http-sample.md"
        self.assertEqual(self.request(path)[0], 404, "complete runs require fresh job-local persistence")
        doc = b"---\nrepo: example/http-sample\nbase: main\ntitle: HTTP sample\n---\n"
        gate = threading.Barrier(2)
        def create(marker):
            gate.wait(timeout=10)
            return self.request(path, "PUT", doc + marker,
                                {"Content-Type": "text/markdown", "If-None-Match": "*"})
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            results = list(pool.map(create, (b"A\n", b"B\n")))
        self.assertEqual(sorted(r[0] for r in results), [200, 412])
        winner = next(i for i, r in enumerate(results) if r[0] == 200)
        status, headers, body = self.request(path)
        self.assertEqual(status, 200)
        self.assertEqual(body, doc + (b"A\n", b"B\n")[winner])
        self.assertEqual(strong(headers["ETag"]), strong(results[winner][1]["ETag"]))

    def test_job_optional_and_conditional_writes_and_weak_etags(self):
        path = "/api/jobs/sample/001-http/build.md"
        status, headers, _ = self.request(path, "PUT", b"blind v1\n", {"Content-Type": "text/markdown"})
        self.assertEqual(status, 200, "legacy blind job write remains allowed")
        tag = strong(headers["ETag"])
        status, _, body = self.request(path)
        self.assertEqual((status, body), (200, b"blind v1\n"))
        status, headers, _ = self.request(path, "PUT", b"weak v2\n",
                                          {"Content-Type": "text/markdown", "If-Match": "W/" + tag})
        self.assertEqual(status, 200, "edge-weakened W/ ETag is accepted")
        weak_tag = strong(headers["ETag"])
        self.assertNotEqual(weak_tag, tag)
        status, _, body = self.request(path, "PUT", b"stale\n",
                                       {"Content-Type": "text/markdown", "If-Match": tag})
        self.assertEqual(status, 412, "stale condition is refused")
        self.assertEqual(self.request(path)[2], b"weak v2\n")
        status, _, _ = self.request(path, "PUT", b"bad\n",
                                    {"Content-Type": "text/markdown", "If-Match": "garbage"})
        self.assertEqual(status, 428, "malformed present condition keeps the 428 grammar")
        status, _, _ = self.request(path, "PUT", b"create-only\n",
                                    {"Content-Type": "text/markdown", "If-None-Match": "*"})
        self.assertEqual(status, 412, "create-only on an existing job file is refused")
        self.assertEqual(self.request(path)[2], b"weak v2\n")

    def test_idea_image_idempotence(self):
        path = "/api/idea-images/" + PNG_NAME
        status, _, _ = self.request(path, "PUT", PNG, {"Content-Type": "image/png"})
        self.assertEqual(status, 200, "first PUT creates the object")
        before = self.request(path)
        status, _, _ = self.request(path, "PUT", PNG, {"Content-Type": "image/png"})
        self.assertEqual(status, 200, "second PUT is a create-only no-op on success")
        after = self.request(path)
        self.assertEqual(after[2], before[2], "stored bytes unchanged across no-op PUT")
        self.assertEqual(after[2], PNG, "stored bytes are the bytes we PUT")
        self.assertEqual(after[1].get("ETag"), before[1].get("ETag"),
                         "stored ETag is stable across no-op PUT (read from GET, not PUT)")
        self.assertEqual(after[1].get("Content-Security-Policy"), "default-src 'none'; sandbox")
        status, _, _ = self.request("/api/idea-images/" + "b" * 64 + ".png", "PUT", PNG,
                                    {"Content-Type": "image/png"})
        self.assertEqual(status, 400, "digest must match the file name")

    def test_two_device_job_exit6_and_reconciliation(self):
        factory = self.factory
        repo_a, repo_b = self.git_repo("repo-a"), self.git_repo("repo-b")
        before = {repo: self.git_state(repo) for repo in (repo_a, repo_b)}
        homes = [self.home("job-a"), self.home("job-b")]
        for home, repo in zip(homes, (repo_a, repo_b)):
            (home / "paths.md").write_text(f"---\nsample: {repo}\n---\n")
        target = "sample/002-http"
        for home in homes:
            with patch.object(factory, "factory_home", return_value=home):
                factory.pull(target)  # no files yet on either device
        folder_a = homes[0] / "jobs" / "sample" / "002-http"
        folder_a.mkdir(parents=True, exist_ok=True)
        (folder_a / "build.md").write_bytes(b"device A build\n")
        with patch.object(factory, "factory_home", return_value=homes[0]):
            factory.push(target)
        status, _, remote = self.request("/api/jobs/sample/002-http/build.md")
        self.assertEqual((status, remote), (200, b"device A build\n"))
        winner_tag = strong(self.request("/api/jobs/sample/002-http/build.md")[1]["ETag"])
        with patch.object(factory, "factory_home", return_value=homes[1]):
            factory.pull(target)
        folder_b = homes[1] / "jobs" / "sample" / "002-http"
        self.assertEqual((folder_b / "build.md").read_bytes(), b"device A build\n")
        (folder_b / "build.md").write_bytes(b"device B build\n")
        (folder_a / "build.md").write_bytes(b"device A build v2\n")
        with patch.object(factory, "factory_home", return_value=homes[0]):
            factory.push(target)
        self.assertEqual(self.request("/api/jobs/sample/002-http/build.md")[2], b"device A build v2\n")
        with self.assertRaises(SystemExit) as raised:
            with patch.object(factory, "factory_home", return_value=homes[1]):
                factory.push(target)
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual((folder_b / "build.md").read_bytes(), b"device B build\n", "local edit kept")
        remote_copy = folder_b / "build.md.remote"
        self.assertEqual(remote_copy.read_bytes(), b"device A build v2\n", "R2 winner saved beside the file")
        self.assertEqual(self.request("/api/jobs/sample/002-http/build.md.remote")[0], 404,
                         ".remote is never uploaded")
        self.assertEqual(self.request("/api/jobs/sample/002-http/.sync.json")[0], 404,
                         "the sync manifest is never uploaded")
        # Explicit reconciliation: adopt the local merge, drop the marker, push exactly that.
        (folder_b / "build.md").write_bytes(b"device B merged\n")
        remote_copy.unlink()
        with patch.object(factory, "factory_home", return_value=homes[1]):
            factory.pull(target)
            factory.push(target)
        final = self.request("/api/jobs/sample/002-http/build.md")
        self.assertEqual(final[2], b"device B merged\n")
        self.assertNotEqual(strong(final[1]["ETag"]), winner_tag)
        for repo in (repo_a, repo_b):
            self.assertEqual(self.git_state(repo), before[repo], "claims/sync never touch Git checkouts")

    def test_two_device_project_exit6_and_reconciliation(self):
        factory = self.factory
        path = "/api/projects/http-devices.md"
        doc = b"---\nrepo: example/http-devices\nbase: main\ntitle: HTTP devices\n---\n"
        status, _, _ = self.request(path, "PUT", doc,
                                    {"Content-Type": "text/markdown", "If-None-Match": "*"})
        self.assertEqual(status, 200)
        homes = [self.home("proj-a"), self.home("proj-b")]
        for home in homes:
            (home / "paths.md").write_text(f"---\nhttp-devices: {home}\n---\n")
            factory.pull_project(home, "http-devices")
        docs = [factory.project_files(home, "http-devices")[0] for home in homes]
        docs[0].write_bytes(doc + b"A\n")
        docs[1].write_bytes(doc + b"B\n")
        with patch.object(factory, "factory_home", return_value=homes[0]):
            factory.push_project(homes[0], "http-devices")
        with self.assertRaises(SystemExit) as raised:
            factory.push_project(homes[1], "http-devices")
        self.assertEqual(raised.exception.code, 6)
        self.assertEqual(docs[1].read_bytes(), doc + b"B\n", "loser keeps its local edit")
        with self.assertRaises(SystemExit) as pulled:
            factory.pull_project(homes[1], "http-devices")
        self.assertEqual(pulled.exception.code, 6)
        remote = factory.project_files(homes[1], "http-devices")[2]
        self.assertEqual(remote.read_bytes(), doc + b"A\n", "winner bytes saved as .remote")
        self.assertEqual(self.request("/api/projects/http-devices.md")[2], doc + b"A\n")
        self.assertEqual(self.request("/api/projects/http-devices.md.remote")[0], 404,
                         ".remote is never uploaded")
        docs[1].write_bytes(doc + b"merged\n")
        remote.unlink()
        factory.push_project(homes[1], "http-devices")
        final = self.request("/api/projects/http-devices.md")
        self.assertEqual(final[2], doc + b"merged\n")
        self.assertTrue(strong(final[1]["ETag"]))


if __name__ == "__main__":
    unittest.main()
