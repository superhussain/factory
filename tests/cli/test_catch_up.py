import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest.mock import Mock, patch

from support import ROOT, IsolatedTestCase, invoke, load_runtime


GITHUB = "https://github.com/example/factory.git"
RUN_URL = "https://github.com/example/factory/actions/runs/7"


def git(cwd, *args):
    return subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True).stdout.strip()


def runs(status="completed", conclusion="success", attempt=1):
    return 0, json.dumps([{"databaseId": 7, "status": status, "conclusion": conclusion, "attempt": attempt,
                           "url": RUN_URL}])


class CatchUpTest(IsolatedTestCase):
    """A src repo pushed to a bare origin.git, a checkout cloned from it, and a skill installed
    from commit A while origin is at commit B."""

    def setUp(self):
        self.runtime = load_runtime()
        self.root = Path(os.environ["HOME"]).resolve()
        self.src, self.origin, self.checkout = self.root / "src", self.root / "origin.git", self.root / "checkout"
        (self.src / "tools").mkdir(parents=True)
        shutil.copy(ROOT / "tools" / "install.py", self.src / "tools")
        shutil.copytree(ROOT / "skills" / "factory", self.src / "skills" / "factory",
                        ignore=shutil.ignore_patterns("__pycache__"))
        (self.src / ".github" / "workflows").mkdir(parents=True)
        (self.src / ".github" / "workflows" / "deploy-main.yml").write_text("name: deploy-main\n")
        git(self.root, "init", "-q", str(self.src))
        git(self.src, "symbolic-ref", "HEAD", "refs/heads/main")
        self.a = self.commit("A")
        git(self.root, "clone", "-q", "--bare", str(self.src), str(self.origin))
        git(self.src, "remote", "add", "origin", str(self.origin))
        git(self.root, "clone", "-q", str(self.origin), str(self.checkout))
        git(self.checkout, "config", f"url.{self.origin}.insteadOf", GITHUB)
        git(self.checkout, "remote", "set-url", "origin", GITHUB)
        self.skills = self.root / "skills"
        subprocess.run([sys.executable, "-B", str(self.src / "tools" / "install.py"), "--destination",
                        str(self.skills)], check=True, capture_output=True)
        self.skill = self.skills / "factory"
        with open(self.src / "skills" / "factory" / "SKILL.md", "a") as f:
            f.write("\nCommit B.\n")
        self.b = self.commit("B")

        self.home = self.root / "state"
        self.home.mkdir()
        (self.home / "paths.md").write_text(f"---\nfactory: {self.checkout}\n---\n")
        (self.home / "config.md").write_text("---\ndevice: example-host\n---\n")
        (self.home / ".env").write_text("FACTORY_ACCESS_CLIENT_ID=synthetic-id\n")
        (self.home / ".env").chmod(0o600)

        self.gh = Mock(return_value=runs())
        self.timer = Mock(return_value=(False, ""))
        for name, value in (("HERE", str(self.skill / "runtime")), ("timer_installed", self.timer),
                            ("catch_up_gh", self.gh), ("PICKUP_SCHEDULER", "cron"),
                            ("setup_run", Mock(return_value=(0, "# synthetic crontab\n")))):
            patcher = patch.object(self.runtime, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def commit(self, message, push=True):
        git(self.src, "add", "-A")
        git(self.src, "commit", "-q", "-m", message)
        if push and message != "A":
            git(self.src, "push", "-q", "origin", "main")
        return git(self.src, "rev-parse", "HEAD")

    def edit_installer(self, old, new):
        path = self.src / "tools" / "install.py"
        text = path.read_text()
        self.assertIn(old, text)
        path.write_text(text.replace(old, new))

    def call(self):
        return invoke(self.runtime, ["--home", str(self.home), "catch-up"])

    def installed(self):
        return json.loads((self.skill / "bundle.json").read_text())["commit"]

    def backups(self):
        folder = self.home / "backups"
        return sorted(folder.iterdir()) if folder.exists() else []

    def worktrees(self):
        return git(self.checkout, "worktree", "list", "--porcelain")

    def test_installs_origin_head_with_backup(self):
        (self.home / "paths.md").write_text(f"---\nother: {self.root}\n---\n")
        self.assertEqual(self.call(), (1, "", "factory.py: no factory checkout in paths.md; "
                                       "run factory.py init <factory checkout>\n"))
        (self.home / "paths.md").write_text(f"---\nfactory: {self.checkout}\n---\n")
        with patch.object(self.runtime, "HERE", str(self.checkout / "skills" / "factory" / "runtime")):
            self.assertEqual(self.call(), (2, "", "factory.py: catch-up runs from the installed skill, "
                                           f"not from the checkout {self.checkout}\n"))
        self.assertEqual(self.backups(), [])

        code, out, err = self.call()
        self.assertEqual((code, err), (0, ""))
        [backup] = self.backups()
        a7, b7 = self.a[:7], self.b[:7]
        self.assertEqual(out.splitlines(), [
            f"catch-up: factory checkout {self.checkout}, origin/main at {b7}",
            f"checkout: fast-forwarded to {b7}",
            f"skill: {a7} installed, {b7} on origin/main",
            f"backup: {backup}",
            f"skill: installed {b7} into {self.skill}",
            "pickup timer: none on this host, left alone",
            f"worker: deployed at {b7}",
            "catch-up: done"])
        self.assertRegex(backup.name, r"^catch-up-\d{8}T\d{6}Z-[0-9a-f]{6}$")
        self.assertEqual(self.installed(), self.b)
        self.assertIn("Commit B.", (self.skill / "SKILL.md").read_text())
        self.assertNotIn("Commit B.", (backup / "skill" / "factory" / "SKILL.md").read_text())
        self.assertEqual(json.loads((backup / "skill" / "factory" / "bundle.json").read_text())["commit"], self.a)
        self.assertEqual((backup / "home" / "paths.md").read_text(), (self.home / "paths.md").read_text())
        self.assertEqual((backup / "crontab.txt").read_text(), "# synthetic crontab\n")
        self.assertEqual(stat.S_IMODE(backup.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((backup / "home" / ".env").stat().st_mode), 0o600)
        self.assertFalse((backup / "home" / "routes.md").exists())
        self.assertEqual(self.worktrees().count("worktree "), 1)

    def test_leaves_detached_dirty_checkout(self):
        def state():
            return (git(self.checkout, "rev-parse", "HEAD"), git(self.checkout, "status", "--porcelain"),
                    git(self.checkout, "branch", "--show-current"), self.worktrees())

        git(self.checkout, "checkout", "-q", "--detach")
        (self.checkout / "tools" / "install.py").write_text("# local edit\n")
        (self.checkout / "untracked.txt").write_text("synthetic\n")
        cases = [("detached HEAD", lambda: None),
                 ("dirty", lambda: git(self.checkout, "checkout", "-q", "main")),
                 ("on branch topic", lambda: (git(self.checkout, "checkout", "-q", "--", "."),
                                              (self.checkout / "untracked.txt").unlink(),
                                              git(self.checkout, "checkout", "-q", "-b", "topic")))]
        for why, arrange in cases:
            with self.subTest(why):
                arrange()
                before = state()
                code, out, err = self.call()
                self.assertEqual((code, err), (0, ""))
                self.assertIn(f"checkout: left as is ({why})", out.splitlines())
                self.assertEqual(state(), before)
                self.assertEqual(before[0], self.a)
        self.assertEqual(self.installed(), self.b)

    def test_fast_forwards_clean_checkout(self):
        self.assertEqual(git(self.checkout, "rev-parse", "HEAD"), self.a)
        code, out, _ = self.call()
        self.assertEqual(code, 0)
        self.assertIn(f"checkout: fast-forwarded to {self.b[:7]}", out.splitlines())
        self.assertEqual(git(self.checkout, "rev-parse", "HEAD"), self.b)
        self.assertEqual(git(self.checkout, "status", "--porcelain"), "")
        self.assertIn(f"checkout: already at {self.b[:7]}", self.call()[1].splitlines())

    def test_second_run_nothing_new(self):
        self.assertEqual(self.call()[0], 0)
        backups = self.backups()
        code, out, err = self.call()
        self.assertEqual((code, err), (0, ""))
        b7 = self.b[:7]
        self.assertEqual(out.splitlines(), [
            f"catch-up: factory checkout {self.checkout}, origin/main at {b7}",
            f"checkout: already at {b7}",
            f"skill: current at {b7}",
            "pickup timer: none on this host, left alone",
            f"worker: deployed at {b7}",
            "catch-up: nothing new"])
        self.assertEqual(self.backups(), backups)

    def test_runs_origin_installer(self):
        marker = self.root / "marker"
        self.edit_installer("    args = parser.parse_args(argv)\n",
                            f"    args = parser.parse_args(argv)\n    Path({str(marker)!r}).write_text(__file__)\n")
        c = self.commit("C")
        git(self.checkout, "checkout", "-q", "--detach")
        code, out, _ = self.call()
        self.assertEqual(code, 0, out)
        ran = Path(marker.read_text())
        self.assertEqual(ran.parts[-3:], ("src", "tools", "install.py"))
        self.assertTrue(ran.parent.parent.parent.name.startswith("factory-catch-up-"))
        self.assertEqual(ran.parent.parent.parent.parent, Path(tempfile.gettempdir()))
        self.assertFalse(ran.parent.parent.parent.exists())
        self.assertEqual(self.installed(), c)
        self.assertEqual(self.worktrees().count("worktree "), 1)

    def timer_calls(self, out="installed cron timer: crontab\n1. ok home: synthetic\n", code=3):
        real, calls = subprocess.run, []

        def run(argv, *args, **kwargs):
            if "pickup" in argv:
                calls.append(argv)
                self.assertEqual(kwargs.get("timeout"), 120)
                return subprocess.CompletedProcess(argv, code, out, "")
            return real(argv, *args, **kwargs)
        patcher = patch.object(self.runtime.subprocess, "run", run)
        patcher.start()
        self.addCleanup(patcher.stop)
        return calls

    def test_timer_uses_bundle_entry(self):
        self.timer.return_value = (True, "")
        calls = self.timer_calls()
        tail = ["--home", str(self.home), "pickup", "setup", "--install"]
        code, out, _ = self.call()
        self.assertEqual(code, 0)
        self.assertIn("pickup timer: installed cron timer: crontab", out.splitlines())
        self.assertEqual(calls, [[sys.executable, str(self.skill / "runtime" / "factory.py")] + tail])

        with patch.object(self.runtime, "timer_drift", Mock(return_value=None)):
            code, out, _ = self.call()
        self.assertEqual((code, out.splitlines()[-3:]), (0, ["pickup timer: current", f"worker: deployed at {self.b[:7]}",
                                                         "catch-up: nothing new"]))
        with patch.object(self.runtime, "timer_drift", Mock(return_value="the timer runs an old runtime")):
            code, out, _ = self.call()
        self.assertEqual((code, len(calls), len(self.backups())), (0, 2, 2))
        self.assertEqual(out.splitlines()[-1], "catch-up: done")

        self.edit_installer('json.dumps({"commit": commit, "tree": tree.hexdigest()}',
                            'json.dumps({"commit": commit, "tree": tree.hexdigest(), "entry": ["bin/factory", "-q"]}')
        self.commit("C")
        self.assertEqual(self.call()[0], 0)
        self.assertEqual(calls[-1], [str(self.skill / "bin" / "factory"), "-q"] + tail)

        folder = self.root / "entries"
        folder.mkdir()
        fallback = [sys.executable, str(folder / "runtime" / "factory.py")]
        self.assertEqual(self.runtime.catch_up_entry(folder), fallback)
        for entry in (["/usr/bin/factory"], ["../factory"], ["bin/../../factory"], [], [""], [1], "bin/factory", None):
            (folder / "bundle.json").write_text(json.dumps({"commit": self.b, "entry": entry}))
            self.assertEqual(self.runtime.catch_up_entry(folder), fallback, entry)

    def test_timer_skipped_without_timer(self):
        calls = self.timer_calls()
        code, out, _ = self.call()
        self.assertEqual(code, 0)
        self.assertIn("pickup timer: none on this host, left alone", out.splitlines())
        self.assertEqual(calls, [])

    def test_worker_states(self):
        self.assertEqual(self.call()[0], 0)
        b7 = self.b[:7]
        self.assertEqual(self.gh.call_args.args, (
            "example/factory", "run", "list", "--workflow", "deploy-main.yml", "--commit", self.b,
            "--json", "databaseId,status,conclusion,attempt,url", "--limit", "1"))
        cases = [
            ([runs()], f"worker: deployed at {b7}", 0, "catch-up: nothing new"),
            ([runs("queued", "")], f"worker: deploy run {RUN_URL} is queued", 0, "catch-up: nothing new"),
            ([runs("in_progress", "")], f"worker: deploy run {RUN_URL} is in_progress", 0, "catch-up: nothing new"),
            ([(0, "[]")], f"worker: no deploy run for {b7} yet; run catch-up again later", 0, "catch-up: nothing new"),
            ([runs(conclusion="failure"), (0, "")], f"worker: reran deploy run {RUN_URL}", 0, "catch-up: done"),
            ([runs(conclusion="cancelled"), (0, "")], f"worker: reran deploy run {RUN_URL}", 0, "catch-up: done"),
            ([runs(conclusion="failure", attempt=2)],
             f"worker: deploy run {RUN_URL} ended failure after 2 attempts; check it by hand", 1,
             "catch-up: 1 step(s) failed"),
            ([(127, "No such file or directory: 'gh'")], "worker: skipped, gh did not run: No such file or directory: 'gh'",
             0, "catch-up: nothing new"),
        ]
        for replies, line, exit_code, last in cases:
            with self.subTest(line):
                self.gh.reset_mock()
                self.gh.side_effect = replies
                code, out, _ = self.call()
                self.assertEqual((code, out.splitlines()[-2:]), (exit_code, [line, last]))
                self.assertEqual(self.gh.call_count, len(replies))
                if len(replies) == 2:
                    self.assertEqual(self.gh.call_args.args, ("example/factory", "run", "rerun", "7"))

        self.gh.reset_mock()
        git(self.checkout, "remote", "set-url", "origin", str(self.origin))
        self.assertEqual(self.call()[1].splitlines()[-2], "worker: skipped, origin is not on GitHub")
        git(self.checkout, "remote", "set-url", "origin", GITHUB)
        shutil.rmtree(self.src / ".github")
        self.commit("C")
        self.assertEqual(self.call()[1].splitlines()[-2],
                         "worker: skipped, origin/main has no .github/workflows/deploy-main.yml")
        self.gh.assert_not_called()

    def test_failed_install_keeps_backup(self):
        self.timer.return_value = (True, "")
        calls = self.timer_calls()
        (self.src / "tools" / "install.py").write_text("import sys\nsys.exit('install.py: synthetic failure')\n")
        self.commit("C")
        code, out, err = self.call()
        self.assertEqual((code, err), (1, ""))
        lines = out.splitlines()
        self.assertIn("skill: install.py: synthetic failure", lines)
        self.assertEqual(lines[-1], "catch-up: 1 step(s) failed")
        self.assertFalse(any(line.startswith("pickup timer:") for line in lines))
        self.assertEqual(calls, [])
        [backup] = self.backups()
        self.assertTrue((backup / "skill" / "factory" / "SKILL.md").is_file())
        self.assertEqual(self.installed(), self.a)
        self.assertEqual(self.worktrees().count("worktree "), 1)
