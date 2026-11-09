"""Actual local R2 document conditions; never follows redirects."""
import concurrent.futures
import json
import os
from pathlib import Path
import re
import threading
import unittest
import urllib.error
import urllib.request
from unittest.mock import patch

FIXTURES = Path(__file__).parent.parent / "fixtures"
PROJECTS = ("documents-sample", "documents-second")
OWNED = ("http://127.0.0.1:8220", "http://127.0.0.1:8221")


def test_url():
    url = os.environ.get("FACTORY_TEST_URL")
    if url is not None and url not in OWNED:
        raise ValueError("FACTORY_TEST_URL must be an owned loopback origin: " + " or ".join(OWNED))
    return url


def strong(tag):
    if not isinstance(tag, str) or not re.fullmatch(r'(W/)?"[\x21\x23-\x7e\x80-\xff]+"', tag):
        raise ValueError("missing or malformed ETag")
    return tag[2:] if tag.startswith("W/") else tag


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class LocalTargetTest(unittest.TestCase):
    def test_guard_before_network(self):
        with patch.object(urllib.request, "build_opener") as network:
            for url in ("https://factory.example.test", "http://localhost:8220", "http://127.0.0.1:8220/", "http://127.0.0.1:8210", "http://user@127.0.0.1:8220", "http://127.0.0.1:8220?x=1", "http://127.0.0.1:8220#x", "http://127.0.0.1:8220/api"):
                with patch.dict(os.environ, {"FACTORY_TEST_URL": url}), self.assertRaises(ValueError):
                    test_url()
            network.assert_not_called()


class ProjectDocumentsApiTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.url = test_url()
        if cls.url is None:
            raise unittest.SkipTest("set FACTORY_TEST_URL for actual local R2 tests")

    def request(self, path, method="GET", data=None, headers=None):
        req = urllib.request.Request(self.url + path, data, headers or {}, method=method)
        try:
            with urllib.request.build_opener(NoRedirect).open(req, timeout=30) as r:
                return r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def put(self, path, body, condition):
        return self.request(path, "PUT", body, {"Content-Type": "text/markdown; charset=utf-8", **condition})

    def reset(self, path, body):
        status, headers, _ = self.request(path)
        self.assertIn(status, (200, 404))
        condition = {"If-Match": strong(headers.get("ETag"))} if status == 200 else {"If-None-Match": "*"}
        result = self.put(path, body, condition)
        self.assertEqual(result[0], 200)
        self.assertEqual(self.request(path)[2], body)
        return strong(result[1].get("ETag"))

    def setUp(self):
        self.config = (FIXTURES / "project-documents.md").read_bytes()
        self.notes = (FIXTURES / "project-document-notes.md").read_bytes()
        for slug in PROJECTS:
            self.reset("/api/projects/" + slug + ".md", self.config)
            self.reset(self.path(slug, "notes.md"), self.notes)

    @staticmethod
    def path(slug="documents-sample", name="notes.md"):
        return "/api/project-documents/" + slug + "/" + name

    def race(self, writes):
        gate = threading.Barrier(len(writes))
        def write(args):
            gate.wait(timeout=10)
            return self.put(*args)
        with concurrent.futures.ThreadPoolExecutor(len(writes)) as pool:
            return list(pool.map(write, writes))

    def assert_winner(self, path, bodies, results):
        self.assertEqual(sorted(r[0] for r in results), [200, 412])
        winner = next(i for i, r in enumerate(results) if r[0] == 200)
        status, headers, body = self.request(path)
        self.assertEqual(status, 200)
        self.assertEqual(body, bodies[winner])
        self.assertEqual(strong(headers["ETag"]), strong(results[winner][1]["ETag"]))
        print("R2 winner", path, body, strong(headers["ETag"]))

    def test_create_race(self):
        path = self.path(name="decisions.md")
        self.assertEqual(self.request(path)[0], 404, "complete runs require fresh job-local persistence")
        bodies = (b"Created A\n", b"Created B\n")
        results = self.race([(path, body, {"If-None-Match": "*"}) for body in bodies])
        self.assert_winner(path, bodies, results)

    def test_update_race(self):
        path = self.path()
        tag = strong(self.request(path)[1]["ETag"])
        bodies = (b"Updated A\n", b"Updated B\n")
        results = self.race([(path, body, {"If-Match": tag}) for body in bodies])
        self.assert_winner(path, bodies, results)

    def test_independent_names_and_projects(self):
        paths = (self.path(), self.path(name="runbook.md"), self.path("documents-second"))
        tags = [self.reset(path, ("Distinct base " + str(i)).encode()) for i, path in enumerate(paths)]
        bodies = [b"Notes saved", b"Runbook saved", b"Second project saved"]
        results = self.race([(path, body, {"If-Match": tag}) for path, body, tag in zip(paths, bodies, tags)])
        self.assertEqual([r[0] for r in results], [200, 200, 200])
        for path, body, result in zip(paths, bodies, results):
            got = self.request(path)
            self.assertEqual(got[2], body)
            self.assertEqual(strong(got[1]["ETag"]), strong(result[1]["ETag"]))
            print("R2 independent", path, got[2], strong(got[1]["ETag"]))
        for slug in PROJECTS:
            self.assertEqual(self.request("/api/projects/" + slug + ".md")[2], self.config)

    def test_validation_listing_and_missing_registration(self):
        path = self.path()
        original = self.request(path)
        tag = strong(original[1]["ETag"])
        for headers, body, expected in [({}, b"valid", 428), ({"If-Match": "unquoted"}, b"valid", 400), ({"If-Match": tag, "Content-Type": "text/plain"}, b"valid", 415), ({"If-Match": tag, "Content-Type": "text/markdown"}, b"\xff", 400), ({"If-Match": tag, "Content-Type": "text/markdown"}, b"\0", 400), ({"If-Match": tag, "Content-Type": "text/markdown"}, b"x" * ((1 << 20) + 1), 413), ({"If-Match": '"stale"', "Content-Type": "text/markdown"}, b"valid", 412), ({"If-None-Match": "*", "Content-Type": "text/markdown"}, b"valid", 412)]:
            with self.subTest(expected=expected, headers=headers):
                rejected = self.request(path, "PUT", body, headers)
                self.assertEqual(rejected[0], expected)
                if headers.get("If-Match") == "unquoted":
                    self.assertEqual(rejected[2], b"invalid document write condition")
                current = self.request(path)
                self.assertEqual(current[2], original[2])
                self.assertEqual(strong(current[1]["ETag"]), tag)
        transport = self.request(path, "PUT", b"valid", {"If-Match": "", "Content-Type": "text/markdown"})
        self.assertIn(transport[0], (400, 428))
        self.assertEqual(transport[2], b"invalid document write condition" if transport[0] == 400 else b'PUT needs If-Match: "<etag>" or If-None-Match: *')
        current = self.request(path)
        self.assertEqual(current[2], original[2])
        self.assertEqual(strong(current[1]["ETag"]), tag)
        print("HTTP empty-header transport rejection", transport[0], transport[2])
        for method in ("GET", "PUT"):
            self.assertEqual(self.request(self.path("documents-unregistered"), method, b"text" if method == "PUT" else None)[0], 404)
        self.assertEqual(self.request("/api/project-documents/documents-unregistered")[0], 404)
        for name in ("config.md", "Notes.md", "notes.md/extra", "%6eotes.md"):
            self.assertEqual(self.request(self.path(name=name))[0], 404)
        listing = json.loads(self.request("/api/project-documents/documents-sample")[2])
        self.assertEqual([item["name"] for item in listing], ["notes.md", "decisions.md", "runbook.md"])
        for item in listing:
            self.assertEqual(set(item), {"name", "exists"})
            self.assertIsInstance(item["exists"], bool)
        exact = b"# Notes\r\nUnicode \xf0\x9f\x90\x88 <script>literal</script>\r\n"
        self.assertEqual(self.put(path, exact, {"If-Match": tag})[0], 200)
        self.assertEqual(self.request(path)[2], exact)


if __name__ == "__main__":
    unittest.main()
