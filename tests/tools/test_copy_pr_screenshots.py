"""Tests for tools/copy-pr-screenshots.py with a fake urlopen and a synthetic factory home."""
import http.client
import importlib.util
import io
import json
import tempfile
import unittest
import urllib.error
import urllib.parse
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent.parent

spec = importlib.util.spec_from_file_location("copy_pr_screenshots", ROOT / "tools" / "copy-pr-screenshots.py")
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)

API = "https://api.cloudflare.com/client/v4/accounts/synthetic-account/r2/buckets/synthetic-bucket/objects"
SRC_081 = "legacy/factory-081-factory-board-collapse/3f9a1c0e2b7d-after.png"
DST_081 = "factory/factory-081-factory-board-collapse/3f9a1c0e2b7d-after.png"
SRC_163 = "legacy/factory-163-pr-screenshots-origin/a1b2c3d4e5f6-before.png"
SRC_999 = "legacy/factory-999-gone/0badc0ffee00-after.png"
SRC_200 = "legacy/factory-200-both/112233445566-after.png"


class Response:
    def __init__(self, body, content_type):
        self.body = body
        self.headers = {"Content-Type": content_type}

    def read(self):
        return self.body

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FakeNet:
    """Answers R2 API and public origin requests by method and URL, and records each request."""

    def __init__(self, objects=None, pages=None, public=None, errors=None):
        self.objects = dict(objects or {})
        self.pages = pages or []
        self.public = public or {}
        self.errors = errors or {}
        self.requests = []
        self.timeouts = []

    def urlopen(self, request, timeout=None):
        self.requests.append(request)
        self.timeouts.append(timeout)
        method, url = request.get_method(), request.full_url
        error = self.errors.get((method, url))
        if isinstance(error, int):
            raise urllib.error.HTTPError(url, error, "error", {}, None)
        if isinstance(error, Exception):
            raise error
        if isinstance(error, bytes):
            return Response(error, "text/html")
        if url.startswith(API + "?"):
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
            index = int(query.get("cursor", ["0"])[0])
            page = self.pages[index]
            info = {"is_truncated": index + 1 < len(self.pages), "cursor": str(index + 1)}
            return Response(json.dumps({"result": [{"key": k} for k in page], "result_info": info}).encode(), "application/json")
        if url.startswith(API + "/"):
            key = urllib.parse.unquote(url[len(API) + 1 :])
            if method == "PUT":
                self.objects[key] = (request.data, request.get_header("Content-type"))
                return Response(b"{}", "application/json")
            if key not in self.objects:
                raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
            return Response(*self.objects[key])
        key = urllib.parse.unquote(url[len("https://images.example.test/") :])
        return Response(self.public.get(key, self.objects[key][0]), "image/png")


class CopyTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.home = Path(tmp.name)
        (self.home / ".env").write_text("CLOUDFLARE_API_TOKEN=synthetic-token\nCLOUDFLARE_ACCOUNT_ID=synthetic-account\nPR_SCREENSHOTS_BUCKET=synthetic-bucket\nPR_SCREENSHOTS_ORIGIN=https://images.example.test\n")
        for job in ("factory/081-factory-board-collapse", "legacy/163-pr-screenshots-origin", "factory/200-both", "legacy/200-both"):
            (self.home / "jobs" / job).mkdir(parents=True)
            (self.home / "jobs" / job / "spec.md").write_text("spec")
        (self.home / "jobs" / "other" / "999-gone").mkdir(parents=True)

    def run_tool(self, net, *argv):
        with mock.patch("urllib.request.urlopen", net.urlopen), redirect_stdout(io.StringIO()) as out, redirect_stderr(io.StringIO()) as err:
            code = tool.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def write_plan(self, *rows):
        path = self.home / "plan.tsv"
        path.write_text("".join(f"{line}\n" for line in [tool.HEADER, *rows]))
        return str(path)

    def apply(self, net, *rows):
        return self.run_tool(net, "apply", "--home", str(self.home), "--source-project", "legacy", "--plan", self.write_plan(*rows))

    def test_plan_maps_projects(self):
        net = FakeNet(pages=[[SRC_081, SRC_163, "legacy/factory-loose.png"], [SRC_999, SRC_200]])
        out_path = self.home / "plan.tsv"
        code, out, err = self.run_tool(net, "plan", "--home", str(self.home), "--source-project", "legacy", "--out", str(out_path))
        self.assertEqual(code, 0, err)
        self.assertEqual(
            out_path.read_text().splitlines(),
            [
                "source\tdestination\treason",
                f"{SRC_081}\t{DST_081}\tjob folder",
                f"{SRC_163}\t{SRC_163}\tin place",
                f"{SRC_999}\tother/factory-999-gone/0badc0ffee00-after.png\tunknown",
                f"{SRC_200}\tother/factory-200-both/112233445566-after.png\tambiguous: factory,legacy",
            ],
        )
        self.assertEqual(out, "planned 4 rows: 1 job folder, 1 in place, 1 unknown, 1 ambiguous\n")
        self.assertEqual(err, "skipped: legacy/factory-loose.png\n")
        self.assertEqual(len(net.requests), 2)
        first = urllib.parse.parse_qs(urllib.parse.urlsplit(net.requests[0].full_url).query)
        self.assertEqual(first, {"prefix": ["legacy/factory-"], "per_page": ["1000"]})
        self.assertIn("cursor=1", net.requests[1].full_url)

    def test_plan_is_read_only(self):
        net = FakeNet(pages=[[SRC_081], [SRC_163]])
        code, _, _ = self.run_tool(net, "plan", "--home", str(self.home), "--source-project", "legacy", "--out", str(self.home / "plan.tsv"))
        self.assertEqual(code, 0)
        self.assertEqual([r.get_method() for r in net.requests], ["GET", "GET"])
        self.assertTrue(all(r.full_url.startswith(API + "?") for r in net.requests))
        self.assertTrue(all(r.get_header("Authorization") == "Bearer synthetic-token" for r in net.requests))

    def test_apply_copies_and_verifies(self):
        net = FakeNet(objects={SRC_081: (b"png-081", "image/png"), SRC_163: (b"png-163", "image/png")})
        code, out, _ = self.apply(net, f"{SRC_081}\t{DST_081}\tjob folder", f"{SRC_163}\t{SRC_163}\tin place")
        self.assertEqual(code, 0)
        self.assertEqual(
            out,
            f"copied\t{SRC_081}\thttps://images.example.test/{DST_081}\nin place\t{SRC_163}\thttps://images.example.test/{SRC_163}\n",
        )
        self.assertEqual(net.objects[DST_081], (b"png-081", "image/png"))
        self.assertEqual(
            [(r.get_method(), r.full_url) for r in net.requests],
            [
                ("GET", f"{API}/{SRC_081}"),
                ("GET", f"{API}/{DST_081}"),
                ("PUT", f"{API}/{DST_081}"),
                ("GET", f"https://images.example.test/{DST_081}"),
            ],
        )
        public = net.requests[-1]
        self.assertEqual(public.get_header("User-agent"), "factory-copy-pr-screenshots")
        self.assertIsNone(public.get_header("Authorization"))

    def test_apply_never_overwrites(self):
        other = "legacy/factory-081-factory-board-collapse/aaaaaaaaaaaa-before.png"
        other_dst = "factory/factory-081-factory-board-collapse/aaaaaaaaaaaa-before.png"
        objects = {
            SRC_081: (b"png-081", "image/png"),
            DST_081: (b"png-081", "image/png"),
            other: (b"new", "image/png"),
            other_dst: (b"old", "image/png"),
        }
        net = FakeNet(objects=objects)
        code, out, _ = self.apply(net, f"{SRC_081}\t{DST_081}\tjob folder", f"{other}\t{other_dst}\tjob folder")
        self.assertEqual(code, 1)
        self.assertEqual(
            out,
            f"exists-same\t{SRC_081}\thttps://images.example.test/{DST_081}\nexists-different\t{other}\thttps://images.example.test/{other_dst}\n",
        )
        self.assertNotIn("PUT", [r.get_method() for r in net.requests])
        self.assertEqual(net.objects[other_dst], (b"old", "image/png"))

    def test_apply_incomplete_read_is_one_row(self):
        net = FakeNet(
            objects={SRC_163: (b"png-163", "image/png")},
            errors={("GET", f"{API}/{SRC_081}"): http.client.IncompleteRead(b"", 10)},
        )
        code, out, err = self.apply(net, f"{SRC_081}\t{DST_081}\tjob folder", f"{SRC_163}\t{SRC_163}\tin place")
        self.assertEqual(code, 1)
        self.assertEqual(
            out,
            f"failed\t{SRC_081}\thttps://images.example.test/{DST_081}\tsource read: network error: IncompleteRead(0 bytes read, 10 more expected)\n"
            f"in place\t{SRC_163}\thttps://images.example.test/{SRC_163}\n",
        )
        self.assertNotIn("synthetic-token", out + err)
        self.assertNotIn("PUT", [r.get_method() for r in net.requests])

    def test_apply_mismatch(self):
        net = FakeNet(objects={SRC_081: (b"png-081", "image/png")}, public={DST_081: b"challenge page"})
        code, out, _ = self.apply(net, f"{SRC_081}\t{DST_081}\tjob folder")
        self.assertEqual(code, 1)
        self.assertEqual(out, f"mismatch\t{SRC_081}\thttps://images.example.test/{DST_081}\n")

    def test_apply_rejects_bad_plan(self):
        bad_rows = [
            f"other/factory-081-x/a.png\tfactory/factory-081-x/a.png\tjob folder",
            f"{SRC_081}\tfactory/factory-082-other/3f9a1c0e2b7d-after.png\tjob folder",
            f"{SRC_081}\tFactory/factory-081-factory-board-collapse/3f9a1c0e2b7d-after.png\tjob folder",
            f"{SRC_081}\tfActory/factory-081-factory-board-collapse/3f9a1c0e2b7d-after.png\tjob folder",
            f"{SRC_081}\t../factory-081-factory-board-collapse/3f9a1c0e2b7d-after.png\tjob folder",
            f"{SRC_081}",
            "legacy/factory-081-factory-board-collapse/../x.png\tfactory/factory-081-factory-board-collapse/../x.png\tjob folder",
            "legacy/factory-081-factory-board-collapse/./x.png\tfactory/factory-081-factory-board-collapse/./x.png\tjob folder",
        ]
        for bad in bad_rows:
            with self.subTest(bad=bad):
                net = FakeNet(objects={SRC_081: (b"png-081", "image/png")})
                code, out, err = self.apply(net, f"{SRC_163}\t{SRC_163}\tin place", bad)
                self.assertEqual(code, 1)
                self.assertEqual(err, "bad plan row 2\n")
                self.assertEqual(out, "")
                self.assertEqual(net.requests, [])

    def test_http_error_hides_token(self):
        net = FakeNet(objects={SRC_163: (b"png-163", "image/png")}, errors={("GET", f"{API}/{SRC_081}"): 403})
        code, out, err = self.apply(net, f"{SRC_081}\t{DST_081}\tjob folder", f"{SRC_163}\t{SRC_163}\tin place")
        self.assertEqual(code, 1)
        self.assertEqual(
            out,
            f"failed\t{SRC_081}\thttps://images.example.test/{DST_081}\tsource read: HTTP 403\nin place\t{SRC_163}\thttps://images.example.test/{SRC_163}\n",
        )
        self.assertNotIn("synthetic-token", out + err)
        self.assertNotIn("PUT", [r.get_method() for r in net.requests])

    def test_requests_use_long_timeout(self):
        net = FakeNet(objects={SRC_081: (b"png-081", "image/png")})
        code, _, _ = self.apply(net, f"{SRC_081}\t{DST_081}\tjob folder")
        self.assertEqual(code, 0)
        self.assertEqual(net.timeouts, [300, 300, 300, 300])

    def test_failed_row_says_why(self):
        other = "legacy/factory-081-factory-board-collapse/aaaaaaaaaaaa-before.png"
        other_dst = "factory/factory-081-factory-board-collapse/aaaaaaaaaaaa-before.png"
        net = FakeNet(
            objects={other: (b"new", "image/png")},
            errors={("GET", f"{API}/{SRC_081}"): TimeoutError("timed out"), ("PUT", f"{API}/{other_dst}"): 403},
        )
        code, out, err = self.apply(net, f"{SRC_081}\t{DST_081}\tjob folder", f"{other}\t{other_dst}\tjob folder")
        self.assertEqual(code, 1)
        self.assertEqual(
            out,
            f"failed\t{SRC_081}\thttps://images.example.test/{DST_081}\tsource read: timed out after 300 s\n"
            f"failed\t{other}\thttps://images.example.test/{other_dst}\tupload: HTTP 403\n",
        )
        self.assertNotIn("synthetic-token", out + err)

    def test_plan_listing_failure_is_one_line(self):
        listing = f"{API}?prefix=legacy%2Ffactory-&per_page=1000"
        cases = [
            (403, f"listing failed: HTTP 403{tool.READ_HINT}\n"),
            (http.client.IncompleteRead(b"", 10), "listing failed: network error: IncompleteRead(0 bytes read, 10 more expected)\n"),
            (b'{"result": [], "result_info": "x"}', "listing failed: bad response\n"),
            (urllib.error.URLError(TimeoutError("timed out")), "listing failed: timed out after 300 s\n"),
            (b"<html>", "listing failed: bad response\n"),
        ]
        for error, line in cases:
            with self.subTest(line=line):
                out_path = self.home / "plan.tsv"
                net = FakeNet(errors={("GET", listing): error})
                code, out, err = self.run_tool(net, "plan", "--home", str(self.home), "--source-project", "legacy", "--out", str(out_path))
                self.assertEqual(err, line)
                self.assertNotIn("synthetic-token", out + err)
                self.assertEqual(code, 1)
                self.assertEqual(out, "")
                self.assertFalse(out_path.exists())

    def test_plan_skips_dot_segments(self):
        dotdot = "legacy/factory-081-factory-board-collapse/../x.png"
        dot = "legacy/factory-081-factory-board-collapse/./x.png"
        out_path = self.home / "plan.tsv"
        net = FakeNet(pages=[[SRC_081, dotdot, dot]])
        code, out, err = self.run_tool(net, "plan", "--home", str(self.home), "--source-project", "legacy", "--out", str(out_path))
        self.assertEqual(code, 0, err)
        self.assertEqual(err, f"skipped: {dotdot}\nskipped: {dot}\n")
        self.assertEqual(out_path.read_text().splitlines(), ["source\tdestination\treason", f"{SRC_081}\t{DST_081}\tjob folder"])

    def test_missing_publisher_keys(self):
        (self.home / ".env").write_text("CLOUDFLARE_API_TOKEN=synthetic-token\nCLOUDFLARE_ACCOUNT_ID=synthetic-account\n")
        net = FakeNet(pages=[[SRC_081]])
        code, out, err = self.run_tool(net, "plan", "--home", str(self.home), "--source-project", "legacy", "--out", str(self.home / "plan.tsv"))
        self.assertEqual(code, 1)
        self.assertEqual(err, f"PR_SCREENSHOTS_BUCKET / PR_SCREENSHOTS_ORIGIN missing from {self.home / '.env'}\n")
        self.assertEqual(net.requests, [])

    def test_source_project_is_required(self):
        for command in (["plan", "--out", "plan.tsv"], ["apply", "--plan", "plan.tsv"]):
            with self.subTest(command=command[0]), redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit) as exit:
                tool.main([command[0], "--home", str(self.home), *command[1:]])
            self.assertEqual(exit.exception.code, 2)
            self.assertIn("--source-project", err.getvalue())


if __name__ == "__main__":
    unittest.main()
