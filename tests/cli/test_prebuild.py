import contextlib
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, load_runtime

factory = load_runtime()
JOB = "900-sample-plugin"
FAKE_PNPM = """#!/bin/sh
echo "$* $(pwd)" >> "{record}"
[ -n "$FAIL" ] && exit 1
mkdir -p node_modules && echo done > node_modules/.modules.yaml
"""


class PrebuildTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.home, origin, seed = self.root / "home", self.root / "origin.git", self.root / "seed"
        self.wt = self.root / "wt"
        (self.home / "projects").mkdir(parents=True)
        (self.home / "projects" / "factory.md").write_text("---\nbase: main\n---\n", encoding="utf-8")
        self.git(self.root, "init", "-q", "--bare", str(origin))
        self.git(self.root, "init", "-q", str(seed))
        for repo in (origin, seed):
            self.git(repo, "symbolic-ref", "HEAD", "refs/heads/main")
        (seed / "sample").mkdir()
        (seed / "sample" / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
        (seed / "sample" / "package.json").write_text("{}\n", encoding="utf-8")
        (seed / "mise.toml").write_text("[tools]\nnode = '24'\n", encoding="utf-8")
        self.git(seed, "add", ".")
        self.git(seed, "commit", "-q", "-m", "sample")
        self.git(seed, "remote", "add", "origin", str(origin))
        self.git(seed, "push", "-q", "origin", "main")
        self.git(self.root, "clone", "-q", str(origin), str(self.wt))
        self.record = self.root / "calls.txt"
        self.pnpm = self.root / "pnpm"
        self.pnpm.write_text(FAKE_PNPM.format(record=self.record), encoding="utf-8")
        self.pnpm.chmod(0o755)
        tools = {"mise": None, "pnpm": str(self.pnpm)}
        self.addCleanup(patch.stopall)
        patch.object(factory.shutil, "which", side_effect=lambda name: tools[name]).start()
        self.log = self.home / "logs" / f"prebuild-{JOB}.log"

    def git(self, cwd, *args):
        return subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True,
                              check=True).stdout.strip()

    def config(self, dirs):
        (self.home / "prebuild.json").write_text(json.dumps({"version": 1, "prebuild": {"factory": dirs}}),
                                                 encoding="utf-8")

    def prebuild(self):
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            factory.prebuild(self.home, "factory", JOB, self.wt)
        return err.getvalue()

    def calls(self):
        return self.record.read_text(encoding="utf-8").splitlines() if self.record.exists() else []

    def test_no_file_does_nothing(self):
        self.assertEqual(self.prebuild(), "")
        self.assertEqual(self.calls(), [])

    def test_unlisted_project_does_nothing(self):
        (self.home / "prebuild.json").write_text('{"version": 1, "prebuild": {"other": ["sample"]}}',
                                                 encoding="utf-8")
        self.assertEqual(self.prebuild(), "")
        self.assertEqual(self.calls(), [])

    def test_installs_once(self):
        self.config(["sample"])
        self.assertEqual(self.prebuild(), "prebuilt sample with pnpm install --frozen-lockfile\n")
        self.assertEqual(self.calls(), [f"install --frozen-lockfile --ignore-pnpmfile {self.wt / 'sample'}"])
        self.assertTrue(self.log.exists())
        self.assertEqual(self.prebuild(), "")
        self.assertEqual(len(self.calls()), 1)

    def test_marker_present_skips(self):
        self.config(["sample"])
        (self.wt / "sample" / "node_modules").mkdir()
        (self.wt / "sample" / "node_modules" / ".modules.yaml").write_text("x\n", encoding="utf-8")
        self.assertEqual(self.prebuild(), "")
        self.assertEqual(self.calls(), [])

    def test_lockfile_changed_on_head_skips(self):
        self.config(["sample"])
        (self.wt / "sample" / "pnpm-lock.yaml").write_text("lockfileVersion: '9.1'\n", encoding="utf-8")
        self.git(self.wt, "commit", "-q", "-am", "builder lockfile")
        self.assertEqual(self.prebuild(),
                         "prebuild skips sample: its dependency files differ from origin/main\n")
        self.assertEqual(self.calls(), [])

    def test_uncommitted_lockfile_skips(self):
        self.config(["sample"])
        (self.wt / "sample" / "pnpm-lock.yaml").write_text("lockfileVersion: '9.1'\n", encoding="utf-8")
        self.assertEqual(self.prebuild(),
                         "prebuild skips sample: its dependency files differ from origin/main\n")
        self.assertEqual(self.calls(), [])

    def test_root_mise_toml_edit_skips(self):
        self.config(["sample"])
        (self.wt / "mise.toml").write_text("[env]\n_.source = 'x.sh'\n", encoding="utf-8")
        self.git(self.wt, "commit", "-q", "-am", "builder mise.toml")
        self.assertEqual(self.prebuild(),
                         "prebuild skips sample: its dependency files differ from origin/main\n")
        self.assertEqual(self.calls(), [])

    def test_untracked_pnpmfile_skips(self):
        self.config(["sample"])
        (self.wt / "sample" / ".pnpmfile.cjs").write_text("module.exports = {}\n", encoding="utf-8")
        self.assertEqual(self.prebuild(),
                         "prebuild skips sample: its dependency files differ from origin/main\n")
        self.assertEqual(self.calls(), [])

    def test_mise_runs_pnpm_through_exec(self):
        self.config(["sample"])
        with patch.object(factory.shutil, "which", return_value=str(self.pnpm)):
            self.assertEqual(self.prebuild(), "prebuilt sample with pnpm install --frozen-lockfile\n")
        self.assertEqual(self.calls(),
                         [f"exec -- pnpm install --frozen-lockfile --ignore-pnpmfile {self.wt / 'sample'}"])

    def test_timeout_reports_the_log(self):
        self.config(["sample"])
        real = subprocess.run

        def hang(argv, **kwargs):
            if argv[0] == "git":
                return real(argv, **kwargs)
            self.assertEqual(kwargs["timeout"], 600)
            raise subprocess.TimeoutExpired(argv, kwargs["timeout"])

        with patch.object(factory.subprocess, "run", side_effect=hang):
            self.assertEqual(self.prebuild(),
                             f"factory.py: prebuild of sample failed (timed out after 600 s); log: {self.log}\n")

    def test_unrunnable_pnpm_still_returns(self):
        self.config(["sample"])
        self.pnpm.chmod(0o644)
        out = self.prebuild()
        self.assertTrue(out.startswith("factory.py: prebuild of sample failed ([Errno 13] Permission denied"), out)
        self.assertTrue(out.endswith(f"; log: {self.log}\n"), out)

    def test_no_lockfile_skips(self):
        self.config(["missing"])
        self.assertEqual(self.prebuild(), "prebuild skips missing: no pnpm-lock.yaml\n")

    def test_parent_path_skips(self):
        self.config(["../x"])
        self.assertEqual(self.prebuild(), "prebuild skips ../x: not inside the worktree\n")
        self.assertEqual(self.calls(), [])

    def test_absolute_path_skips(self):
        self.config(["/abs"])
        self.assertEqual(self.prebuild(), "prebuild skips /abs: not inside the worktree\n")
        self.assertEqual(self.calls(), [])

    def test_bad_json_says_the_format(self):
        path = self.home / "prebuild.json"
        path.write_text("{not json", encoding="utf-8")
        self.assertEqual(self.prebuild(), f'factory.py: cannot read {path}: use {{"version": 1, "prebuild": '
                                          '{"<project>": ["<dir>", ...]}}\n')
        self.assertEqual(self.calls(), [])

    def test_no_pnpm_skips(self):
        self.config(["sample"])
        with patch.object(factory.shutil, "which", return_value=None):
            self.assertEqual(self.prebuild(), "prebuild skips sample: pnpm is not on PATH\n")

    def test_failed_install_reports_the_log(self):
        self.config(["sample"])
        with patch.dict("os.environ", {"FAIL": "1"}):
            self.assertEqual(self.prebuild(), f"factory.py: prebuild of sample failed (exit 1); log: {self.log}\n")
        self.assertEqual(len(self.calls()), 1)


if __name__ == "__main__":
    unittest.main()
