import contextlib
import io
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from datetime import datetime
from pathlib import Path
from unittest.mock import call, patch

from support import IsolatedTestCase, invoke, load_runtime, snapshot


factory = load_runtime()

PR_URL = "https://github.com/example/factory/pull/7"
OWN_PR_URL = "https://github.com/example/factory/pull/8"
JOBS = [{"id": "2050-orphan-scan", "project": "factory", "stage": "spec"},
        {"id": "2031-stale-banner", "project": "factory", "stage": "merged"},
        {"id": "2044-old-idea", "project": "factory", "stage": "closed"}]
REAL_WHICH = shutil.which


class OrphanScanTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.home = self.root / "home"
        (self.home / "projects").mkdir(parents=True)
        (self.home / "projects" / "factory.md").write_text("---\nrepo: example/factory\n---\n", encoding="utf-8")
        self.worktrees = self.home / "worktrees" / "factory"
        for job_id in ("2031-stale-banner", "2050-orphan-scan"):
            (self.worktrees / job_id).mkdir(parents=True)
        self.remote = self.root / "remote.git"
        self.git("init", "-q", "--bare", str(self.remote))
        self.checkout = self.root / "checkout"
        self.git("clone", "-q", str(self.remote), str(self.checkout))
        self.git("-C", str(self.checkout), "commit", "-q", "--allow-empty", "-m", "base")
        for job_id in ("2031-stale-banner", "2050-orphan-scan"):
            self.git("-C", str(self.checkout), "push", "-q", "origin", f"HEAD:refs/heads/factory/{job_id}")
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "read_paths", return_value={
            "factory": str(self.checkout), "gone": str(self.root / "no-such-checkout")}).start()
        patch.object(factory, "read_board", return_value={"jobs": JOBS}).start()
        self.gh = patch.object(factory, "gh_cli", return_value=(0, json.dumps([
            {"number": 7, "headRefName": "factory/1999-gone", "url": PR_URL},
            {"number": 8, "headRefName": "factory/2050-orphan-scan", "url": OWN_PR_URL},
            {"number": 9, "headRefName": "feature/not-factory", "url": "https://github.com/example/factory/pull/9"}]))).start()
        self.which = patch.object(factory.shutil, "which", side_effect=lambda name, *a, **k:
                                  "/opt/example/paseo" if name == "paseo" else REAL_WHICH(name, *a, **k)).start()
        self.paseo = patch.object(factory, "paseo_run", return_value=(0, json.dumps([
            {"cwd": "/srv/example/worktrees/factory-2044-old-idea-2"},
            {"cwd": "/srv/example/worktrees/factory-2050-orphan-scan"},
            {"cwd": "/srv/example/worktrees/other-workspace"}]))).start()

    def git(self, *args):
        return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout

    def orphans(self):
        code, out, err = invoke(factory, ["orphans"])
        self.assertEqual((code, err), (0, ""))
        return json.loads(out)

    def test_lists_each_kind_with_reason(self):
        self.assertEqual(self.orphans(), {"orphans": [
            {"kind": "branch", "project": "factory", "job": "2031-stale-banner",
             "name": "factory/2031-stale-banner", "reason": "job merged"},
            {"kind": "pr", "project": "factory", "job": "1999-gone", "name": PR_URL,
             "reason": "no job on the board"},
            {"kind": "worktree", "project": "factory", "job": "2031-stale-banner",
             "name": str(self.worktrees / "2031-stale-banner"), "reason": "job merged"},
            {"kind": "workspace", "project": "factory", "job": "2044-old-idea",
             "name": "/srv/example/worktrees/factory-2044-old-idea-2", "reason": "job closed"},
        ], "skipped": []})
        self.gh.assert_called_once_with("pr", "list", "-R", "example/factory", "--state", "open",
                                        "--json", "number,headRefName,url", "--limit", "500")

    def test_scan_only_reads(self):
        before = snapshot(self.home)
        heads = self.git("-C", str(self.remote), "for-each-ref", "refs/heads")
        self.orphans()
        self.assertEqual(snapshot(self.home), before)
        self.assertEqual(self.git("-C", str(self.remote), "for-each-ref", "refs/heads"), heads)
        self.assertEqual(self.paseo.call_args_list, [call(["workspace", "ls", "--json"])])
        self.assertTrue(self.gh.call_args_list)
        self.assertTrue(all(c.args[:2] == ("pr", "list") for c in self.gh.call_args_list))

    def test_failed_sources_are_skipped(self):
        self.git("-C", str(self.checkout), "remote", "set-url", "origin", str(self.root / "missing.git"))
        self.gh.return_value = None
        self.which.side_effect = lambda name, *a, **k: None if name == "paseo" else REAL_WHICH(name, *a, **k)
        links = []
        if os.name != "nt":
            link = self.worktrees / "2099-linked"
            link.symlink_to(self.root)
            links = [f"worktrees: {link} is a symlink or junction"]
        result = self.orphans()
        self.assertEqual(result["skipped"], ["branches factory: git ls-remote failed (exit 128)",
                                             "prs: gh is not on PATH", *links,
                                             "workspaces: paseo is not on PATH"])
        self.assertEqual([o["kind"] for o in result["orphans"]], ["worktree"])
        self.paseo.assert_not_called()

    def test_failed_listings_are_skipped(self):
        (self.home / "projects" / "factory.md").write_text("---\ntitle: factory\n---\n", encoding="utf-8")
        self.paseo.return_value = (3, "")
        with patch.object(factory.subprocess, "run", side_effect=subprocess.TimeoutExpired("git", 20)):
            result = self.orphans()
        self.assertEqual(result["skipped"], ["branches factory: git ls-remote timed out after 20 s",
                                             "prs factory: the project file names no repo",
                                             "workspaces: paseo workspace ls failed (exit 3)"])


class OrphanSummaryTest(unittest.TestCase):
    def row(self, kind):
        return {"kind": kind, "project": "factory", "job": "2031-stale-banner", "name": "x", "reason": "job merged"}

    def test_example_line(self):
        rows = [self.row(k) for k in ("branch", "pr", "worktree", "workspace")]
        self.assertEqual(factory.orphan_summary(rows, []),
                         "orphan scan: 4 orphaned (1 branch, 1 open PR, 1 worktree, 1 Paseo workspace); "
                         "run factory.py orphans to list them")

    def test_plurals_and_skips(self):
        rows = [self.row("branch"), self.row("branch"), self.row("workspace"), self.row("workspace")]
        self.assertEqual(factory.orphan_summary(rows, ["prs: gh is not on PATH"]),
                         "orphan scan: 4 orphaned (2 branches, 2 Paseo workspaces), 1 source(s) skipped; "
                         "run factory.py orphans to list them")

    def test_nothing_orphaned(self):
        self.assertEqual(factory.orphan_summary([], []), "orphan scan: nothing orphaned")


class OrphanPickupTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        self.addCleanup(patch.stopall)
        self.scan = patch.object(factory, "orphan_scan", return_value=(
            [{"kind": "pr", "project": "factory", "job": "1999-gone", "name": PR_URL,
              "reason": "no job on the board"}], [])).start()

    def run_scan(self):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            factory.orphan_pickup_scan(self.home)
        return out.getvalue(), err.getvalue()

    def record(self):
        return json.loads((self.home / "orphan-scan.json").read_text(encoding="utf-8"))

    def test_once_per_local_date(self):
        today = datetime.now().astimezone().strftime("%Y-%m-%d")
        self.assertEqual(self.run_scan(), ("orphan scan: 1 orphaned (1 open PR); run factory.py orphans to list them\n", ""))
        self.assertEqual(self.record(), {"version": 1, "date": today})
        self.assertEqual(self.run_scan(), ("", ""))
        self.assertEqual(self.scan.call_count, 1)

    def test_new_date_scans_again(self):
        (self.home / "orphan-scan.json").write_text('{"version": 1, "date": "2000-01-01"}\n', encoding="utf-8")
        self.assertEqual(self.run_scan()[0].count("\n"), 1)
        self.assertEqual(self.scan.call_count, 1)

    def test_stopped_scan_costs_one_line_and_waits(self):
        self.scan.side_effect = SystemExit(5)
        self.assertEqual(self.run_scan(), ("", "factory.py: orphan scan stopped (exit 5); it runs again tomorrow\n"))
        self.assertEqual(self.run_scan(), ("", ""))
        self.assertEqual(self.scan.call_count, 1)


if __name__ == "__main__":
    unittest.main()
