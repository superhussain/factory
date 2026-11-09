"""Tests for tools/rewrite-pr-links.py with a fake runtime and gh; no test reaches gh, the network or a live board."""
import importlib.util
import io
import json
import tempfile
import types
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent.parent

spec = importlib.util.spec_from_file_location("rewrite_pr_links", ROOT / "tools" / "rewrite-pr-links.py")
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)

REPO = "octo/factory"
ARCHIVE = "octo/factory-private"
OLD = "https://github.com/octo/factory/pull/5"
NEW = "https://github.com/octo/factory-private/pull/5"
BOARD = {
    "updated": "2026-01-01T00:00:00Z",
    "jobs": [
        {"id": "101-old-job", "stage": "merged", "updated": "2026-01-01T00:00:00Z", "pr": OLD},
        {"id": "201-new-job", "stage": "merged", "updated": "2026-01-01T00:00:00Z", "pr": OLD},
    ],
}
HEADS = [{"number": 5, "headRefName": "factory/101-old-job"}]


def fake_runtime(board):
    """An in-memory board behind api and a cas that counts writes."""
    rt = types.SimpleNamespace(text=json.dumps(board), writes=0, now=lambda: "2026-02-02T00:00:00Z",
                               load_board=lambda text: json.loads(text),
                               dump_board=lambda data: json.dumps(data))

    def api(home, method, path, ok=(200,), raw=False):
        assert (method, path) == ("GET", "/api/board")
        return 200, {}, rt.text.encode("utf-8")

    def cas(home, path, change):
        assert path == "/api/board"
        new, result = change(rt.text)
        if new is not None:
            rt.text = new
            rt.writes += 1
        return result

    rt.api, rt.cas = api, cas
    return rt


def gh(returncode=0, listed=HEADS):
    return types.SimpleNamespace(returncode=returncode, stdout=json.dumps(listed), stderr="gh: synthetic failure")


class RewritePrLinksTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.home = tmp.name
        self.rt = fake_runtime(BOARD)
        self.gh = gh()
        for patch in (mock.patch.object(tool, "load_runtime", return_value=self.rt),
                      mock.patch.object(tool, "run", side_effect=lambda argv: self.gh),
                      mock.patch.object(tool.subprocess, "run", side_effect=AssertionError("real subprocess"))):
            patch.start()
            self.addCleanup(patch.stop)

    def main(self, *extra):
        out = io.StringIO()
        with redirect_stdout(out), redirect_stderr(io.StringIO()):
            code = tool.main(["--runtime", "/synthetic/factory.py", "--home", self.home,
                              "--repo", REPO, "--archive", ARCHIVE, *extra])
        return code, out.getvalue()

    def test_plan_matches_head_branch(self):
        self.assertEqual(tool.plan(BOARD, REPO, ARCHIVE, {5: "factory/101-old-job"}),
                         [{"id": "101-old-job", "old": OLD, "new": NEW}])

    def test_dry_run_writes_nothing(self):
        code, out = self.main()
        self.assertEqual(code, 0)
        self.assertEqual(self.rt.writes, 0)
        self.assertEqual(json.loads(self.rt.text), BOARD)
        self.assertEqual(out.splitlines(), [
            f"[keep] 201-new-job: {OLD}",
            f"[plan] 101-old-job: {OLD} -> {NEW}",
            "1 link(s) to rewrite. Dry run: nothing changed. Re-run with --apply to act.",
        ])

    def test_apply_rewrites_only_planned(self):
        code, out = self.main("--apply")
        self.assertEqual(code, 0)
        self.assertEqual(self.rt.writes, 1)
        self.assertTrue(out.endswith("Rewrote 1 link(s) on the board.\n"))
        expected = json.loads(json.dumps(BOARD))
        expected["updated"] = "2026-02-02T00:00:00Z"
        expected["jobs"][0]["pr"] = NEW
        self.assertEqual(json.loads(self.rt.text), expected)

    def test_apply_is_idempotent(self):
        self.main("--apply")
        code, out = self.main("--apply")
        self.assertEqual(code, 0)
        self.assertEqual(self.rt.writes, 1)
        self.assertEqual(out.splitlines(), [
            f"[keep] 201-new-job: {OLD}",
            "No board PR links point at archived pull requests.",
        ])

    def test_gh_failure_exits_1(self):
        self.gh = gh(returncode=1)
        with self.assertRaises(SystemExit) as raised:
            self.main("--apply")
        self.assertEqual(raised.exception.code, 1)
        self.assertEqual(self.rt.writes, 0)

    def test_full_page_exits_1(self):
        self.gh = gh(listed=[{"number": n, "headRefName": f"factory/{n}"} for n in range(1, 1001)])
        err = io.StringIO()
        with self.assertRaises(SystemExit) as raised, redirect_stderr(err):
            tool.main(["--runtime", "/synthetic/factory.py", "--home", self.home,
                       "--repo", REPO, "--archive", ARCHIVE, "--apply"])
        self.assertEqual(raised.exception.code, 1)
        self.assertIn("archive lists 1000 or more pull requests; nothing changed", err.getvalue())
        self.assertEqual(self.rt.writes, 0)


if __name__ == "__main__":
    unittest.main()
