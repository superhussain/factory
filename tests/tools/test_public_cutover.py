"""Tests for tools/public-cutover.py with a fake command runner; no test reaches git, gh or the network."""
import importlib.util
import io
import json
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent.parent

spec = importlib.util.spec_from_file_location("public_cutover", ROOT / "tools" / "public-cutover.py")
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)

REPO = "octo/factory"
ARCHIVE = "octo/factory-archive"
EMAIL = "dev@example.test"
VALUE = "synthetic-secret-value"
SECRETS, VARIABLES = tool.required_names(ROOT)

MUTATIONS = [
    ("gh", "repo", "rename"),
    ("gh", "repo", "create"),
    ("gh", "pr", "create"),
    ("gh", "pr", "close"),
    ("git", "commit-tree"),
    ("git", "push"),
]


def listing(names):
    return json.dumps([{"name": name, "value": VALUE, "updatedAt": "2026-01-01T00:00:00Z"} for name in sorted(names)])


class FakeRunner:
    """Answers argv prefixes with (exit code, stdout) and records every call; an unknown command raises."""

    def __init__(self, overrides=None, checkout=None):
        self.calls = []
        self.answers = [
            (("git", "status", "--porcelain"), (0, "")),
            (("git", "rev-parse", "HEAD", "origin/main"), (0, "c0ffee\nc0ffee\n")),
            (("git", "rev-parse", "HEAD^{tree}"), (0, "7ree\n")),
            (("gh", "auth", "status"), (0, "")),
            (("python3", "/synthetic/runtime/factory.py", "builds", "factory"),
             (0, json.dumps({"queued": [], "in_flight": [], "stale": [], "running": [], "due": False}))),
            (("gh", "pr", "list", "-R", REPO), (0, "[]")),
            (("gh", "repo", "view", ARCHIVE), (1, "")),
            (("python3", "tools/secret-scan.py"), (0, "")),
            (("python3", "/synthetic/runtime/factory.py", "push", "factory"), (0, "")),
            (("gh", "secret", "list", "-R", REPO), (0, listing(SECRETS))),
            (("gh", "variable", "list", "-R", REPO), (0, listing(VARIABLES))),
            (("gh", "repo", "rename"), (0, "")),
            (("gh", "repo", "create"), (0, "")),
            (("git", "commit-tree", "7ree", "-m"), (0, "1111\n")),
            (("git", "commit-tree", "7ree", "-p", "1111"), (0, "2222\n")),
            (("git", "push"), (0, "")),
            (("gh", "run", "list", "-R", REPO, "--workflow", "deploy-main.yml"), (0, '[{"databaseId": 41}]')),
            (("gh", "run", "list", "-R", REPO, "--workflow", "ci.yml"), (0, '[{"databaseId": 42}]')),
            (("gh", "run", "watch"), (0, "")),
            (("gh", "pr", "create"), (0, "")),
            (("gh", "pr", "close"), (0, "")),
        ]
        if checkout:
            self.answers += [
                (("git", "-C", checkout, "status", "--porcelain"), (0, "")),
                (("git", "-C", checkout, "remote", "get-url", "origin"), (0, f"git@github.com:{REPO}.git\n")),
                (("git", "-C", checkout, "remote", "get-url", "archive"), (2, "", "error: No such remote 'archive'")),
                (("git", "-C", checkout, "remote", "add"), (0, "")),
                (("git", "-C", checkout, "fetch"), (0, "")),
                (("git", "-C", checkout, "switch"), (0, "")),
                (("git", "-C", checkout, "for-each-ref"), (0, "main\trefs/remotes/origin/main\nfactory/650\trefs/remotes/origin/factory/650\nfresh\t\narchive/old\t\n")),
                (("git", "-C", checkout, "merge-base", "--is-ancestor", "fresh"), (0, "")),
                (("git", "-C", checkout, "merge-base"), (1, "")),
                (("git", "-C", checkout, "branch"), (0, "")),
            ]
        self.answers = list((overrides or {}).items()) + self.answers

    def __call__(self, argv, cwd=None, env=None):
        self.calls.append((tuple(argv), cwd, env))
        for prefix, (code, out, *err) in self.answers:
            if tuple(argv[: len(prefix)]) == prefix:
                return tool.subprocess.CompletedProcess(argv, code, out, err[0] if err else "")
        raise AssertionError(f"unexpected command: {argv}")

    def argvs(self):
        return [call[0] for call in self.calls]

    def mutations(self):
        return [argv for argv in self.argvs() if any(argv[: len(m)] == m for m in MUTATIONS)
                or argv[3:4] in (("remote",), ("fetch",), ("switch",), ("branch",)) and argv[4:5] != ("get-url",)]


def no_subprocess(*args, **kwargs):
    raise AssertionError("subprocess.run reached")


class PublicCutoverTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.home = Path(tmp.name) / "home"
        (self.home / "projects").mkdir(parents=True)
        self.project = self.home / "projects" / "factory.md"
        self.project.write_text(f"---\nrepo: {REPO}\nbase: main\n---\n# Factory\n")
        self.checkout = str(Path(tmp.name) / "checkout")
        for patcher in (mock.patch.object(tool.subprocess, "run", no_subprocess),
                        mock.patch.object(tool.time, "sleep", lambda seconds: None),
                        mock.patch("builtins.input", lambda prompt="": "done")):
            patcher.start()
            self.addCleanup(patcher.stop)

    def argv(self, *extra):
        return ["--repo", REPO, "--archive", ARCHIVE, "--author-name", "Dev Example", "--author-email", EMAIL,
                "--project", "factory", "--runtime", "/synthetic/runtime/factory.py", "--home", str(self.home),
                "--checkout", self.checkout, *extra]

    def main(self, fake, *extra):
        out = io.StringIO()
        with mock.patch.object(tool, "run", fake), redirect_stdout(out):
            try:
                code = tool.main(self.argv(*extra))
            except SystemExit as stop:
                code = stop.code
        return code, out.getvalue()

    def test_required_names_from_workflows(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflows = Path(tmp) / ".github" / "workflows"
            workflows.mkdir(parents=True)
            (workflows / "a.yml").write_text("env:\n  T: ${{ secrets.API_TOKEN }}\n  G: ${{ secrets.GITHUB_TOKEN }}\n")
            (workflows / "b.yml").write_text("env:\n  P: ${{ vars.DEPLOY_PROFILE }}\n  T: ${{secrets.API_TOKEN}}\n")
            self.assertEqual(tool.required_names(Path(tmp)), ({"API_TOKEN"}, {"DEPLOY_PROFILE"}))
        self.assertEqual((SECRETS, VARIABLES), ({"CLOUDFLARE_API_TOKEN"}, {"FACTORY_DEPLOY_PROFILE"}))

    def test_dry_run_runs_only_reads(self):
        fake = FakeRunner(checkout=self.checkout)
        code, out = self.main(fake)
        self.assertEqual(code, 0)
        self.assertEqual(fake.mutations(), [])
        self.assertIn(f"[plan] gh repo rename factory-archive -R {REPO} --yes", out)
        self.assertIn(f"[plan] gh repo create {REPO} --public", out)
        self.assertIn(f"[human] gh secret set CLOUDFLARE_API_TOKEN -R {REPO}", out)
        self.assertIn(f"[human] gh variable set FACTORY_DEPLOY_PROFILE -R {REPO}", out)
        self.assertIn(f'[plan] git commit-tree <HEAD tree> -m "Initial public release"  (author {EMAIL})', out)
        self.assertIn(f"[plan] git push https://github.com/{REPO}.git <commit>:refs/heads/main", out)
        self.assertTrue(out.rstrip().endswith("Dry run: nothing changed. Re-run with --apply to act."))
        self.assertNotIn("[fail]", out)
        builds = next(call for call in fake.calls if call[0][2:3] == ("builds",))
        self.assertEqual(builds[2]["FACTORY_HOME"], str(self.home))

    def test_names_only(self):
        fake = FakeRunner(checkout=self.checkout)
        code, out = self.main(fake)
        self.assertEqual(code, 0)
        self.assertIn("CLOUDFLARE_API_TOKEN", out)
        self.assertNotIn(VALUE, out)
        for argv in fake.argvs():
            if argv[1:2] in (("secret",), ("variable",)):
                self.assertEqual(argv[argv.index("--json") + 1], "name")
                self.assertNotIn("value", " ".join(argv))

    def test_blockers_stop_apply(self):
        blockers = {
            "in-flight job": {("python3", "/synthetic/runtime/factory.py", "builds", "factory"):
                              (0, json.dumps({"in_flight": ["708-x"], "stale": []}))},
            "stale job": {("python3", "/synthetic/runtime/factory.py", "builds", "factory"):
                          (0, json.dumps({"in_flight": [], "stale": ["700-y"]}))},
            "open PR": {("gh", "pr", "list", "-R", REPO): (0, '[{"number": 7}]')},
            "scan failure": {("python3", "tools/secret-scan.py"): (1, "")},
            "archive name taken": {("gh", "repo", "view", ARCHIVE): (0, '{"name": "factory-archive"}')},
            "missing name": {("gh", "variable", "list", "-R", REPO): (0, "[]")},
            "dirty checkout": {("git", "-C", self.checkout, "status", "--porcelain"): (0, " M README.md\n")},
        }
        for name, override in blockers.items():
            with self.subTest(name):
                fake = FakeRunner(override, checkout=self.checkout)
                code, out = self.main(fake, "--apply")
                self.assertEqual(code, 1)
                self.assertIn("[fail]", out)
                self.assertIn("Checks failed: nothing changed.", out)
                self.assertEqual(fake.mutations(), [])

    def test_apply_order(self):
        fake = FakeRunner(checkout=self.checkout)
        code, out = self.main(fake, "--apply")
        self.assertEqual(code, 0, out)
        steps = [argv[:3] for argv in fake.mutations()]
        self.assertEqual(steps, [
            ("gh", "repo", "rename"),
            ("gh", "repo", "create"),
            ("git", "commit-tree", "7ree"),
            ("git", "push", f"https://github.com/{REPO}.git"),
            ("git", "-C", self.checkout),
            ("git", "-C", self.checkout),
            ("git", "-C", self.checkout),
            ("git", "-C", self.checkout),
            ("git", "-C", self.checkout),
            ("git", "-C", self.checkout),
            ("git", "-C", self.checkout),
            ("git", "commit-tree", "7ree"),
            ("git", "push", f"https://github.com/{REPO}.git"),
            ("gh", "pr", "create"),
            ("gh", "pr", "close"),
        ])
        argvs = fake.argvs()
        self.assertIn(("gh", "repo", "rename", "factory-archive", "-R", REPO, "--yes"), argvs)
        self.assertIn(("git", "push", f"https://github.com/{REPO}.git", "1111:refs/heads/main"), argvs)
        self.assertIn(("git", "-C", self.checkout, "remote", "add", "archive", f"git@github.com:{ARCHIVE}.git"), argvs)
        self.assertIn(("gh", "run", "watch", "41", "-R", REPO, "--exit-status"), argvs)
        self.assertIn(("gh", "run", "watch", "42", "-R", REPO, "--exit-status"), argvs)
        for argv, cwd, env in fake.calls:
            if argv[:2] == ("git", "commit-tree"):
                for key in ("GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"):
                    self.assertEqual(env[key], EMAIL)
                for key in ("GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"):
                    self.assertEqual(env[key], "Dev Example")
        self.assertIn("unchanged", out)
        self.assertEqual(self.project.read_text(), f"---\nrepo: {REPO}\nbase: main\n---\n# Factory\n")
        self.assertNotIn(VALUE, out)

    def test_missing_name_stops_push(self):
        fake = FakeRunner(checkout=self.checkout)
        lists = iter([listing(VARIABLES), "[]", listing(VARIABLES)])
        original = fake.__call__

        def runner(argv, cwd=None, env=None):
            if tuple(argv[:3]) == ("gh", "variable", "list"):
                fake.calls.append((tuple(argv), cwd, env))
                return tool.subprocess.CompletedProcess(argv, 0, next(lists), "")
            return original(argv, cwd, env)

        prompts = mock.Mock(side_effect=["done", "done"])
        out = io.StringIO()
        with mock.patch.object(tool, "run", runner), mock.patch("builtins.input", prompts), redirect_stdout(out):
            code = tool.main(self.argv("--apply"))
        self.assertEqual(code, 0, out.getvalue())
        self.assertEqual(prompts.call_count, 2)
        self.assertIn("[fail] names: missing FACTORY_DEPLOY_PROFILE", out.getvalue())
        self.assertIn(f"[human] gh variable set FACTORY_DEPLOY_PROFILE -R {REPO}", out.getvalue().split("[fail] names")[1])
        argvs = fake.argvs()
        last_list = max(i for i, argv in enumerate(argvs) if argv[:3] == ("gh", "variable", "list"))
        first_push = min(i for i, argv in enumerate(argvs) if argv[:2] == ("git", "push"))
        self.assertLess(last_list, first_push)

    def test_step_failure_shows_stderr(self):
        fake = FakeRunner({("gh", "repo", "create"): (1, "", "GraphQL: Name already exists on this account\n")},
                          checkout=self.checkout)
        code, out = self.main(fake, "--apply")
        self.assertEqual(code, 1)
        self.assertIn(f"[fail] gh repo create {REPO} --public\nGraphQL: Name already exists on this account\n"
                      "See docs/public-cutover.md, Recovery.", out)
        self.assertEqual([argv[:3] for argv in fake.mutations()], [("gh", "repo", "rename"), ("gh", "repo", "create")])

    def test_ctrl_c_at_prompt(self):
        fake = FakeRunner(checkout=self.checkout)
        with mock.patch("builtins.input", side_effect=KeyboardInterrupt):
            code, out = self.main(fake, "--apply")
        self.assertEqual(code, 1)
        self.assertIn("See docs/public-cutover.md, Recovery.", out)
        self.assertEqual([argv[:3] for argv in fake.mutations()], [("gh", "repo", "rename"), ("gh", "repo", "create")])

    def test_ctrl_d_at_prompt(self):
        fake = FakeRunner(checkout=self.checkout)
        with mock.patch("builtins.input", side_effect=EOFError):
            code, out = self.main(fake, "--apply")
        self.assertEqual(code, 1)
        self.assertIn("Stopped before the push.", out)
        self.assertIn("See docs/public-cutover.md, Recovery.", out)
        self.assertEqual([argv[:3] for argv in fake.mutations()], [("gh", "repo", "rename"), ("gh", "repo", "create")])

    def test_project_file_rewrite(self):
        self.project.write_text("---\nrepo: octo/old\nbase: main\n---\n# Factory\n")
        fake = FakeRunner(checkout=self.checkout)
        code, out = self.main(fake, "--apply")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.project.read_text(), f"---\nrepo: {REPO}\nbase: main\n---\n# Factory\n")
        push = next(call for call in fake.calls if call[0][2:3] == ("push",))
        self.assertEqual(push[0], ("python3", "/synthetic/runtime/factory.py", "push", "factory"))
        self.assertEqual(push[2]["FACTORY_HOME"], str(self.home))

    def test_repoint_only(self):
        fake = FakeRunner(checkout=self.checkout)
        code, out = self.main(fake, "--repoint-only")
        self.assertEqual(code, 0, out)
        git = ("git", "-C", self.checkout)
        self.assertEqual(fake.argvs(), [
            (*git, "remote", "get-url", "origin"),
            (*git, "remote", "get-url", "archive"),
            (*git, "remote", "add", "archive", f"git@github.com:{ARCHIVE}.git"),
            (*git, "fetch", "origin", "--prune"),
            (*git, "switch", "--detach", "origin/main"),
            (*git, "for-each-ref", "--format=%(refname:lstrip=2)%09%(upstream)", "refs/heads"),
            (*git, "merge-base", "--is-ancestor", "main", "origin/main"),
            (*git, "merge-base", "--is-ancestor", "factory/650", "origin/main"),
            (*git, "merge-base", "--is-ancestor", "fresh", "origin/main"),
            (*git, "branch", "-m", "main", "archive/main"),
            (*git, "branch", "-m", "factory/650", "archive/factory/650"),
            (*git, "branch", "--unset-upstream", "archive/main"),
            (*git, "branch", "--unset-upstream", "archive/factory/650"),
        ])
        self.assertIn(f"[ok] {self.checkout}: renamed main -> archive/main, factory/650 -> archive/factory/650", out)
        self.assertIn(f"[ok] {self.checkout}: unset upstream on archive/main, archive/factory/650", out)

    def test_repoint_rerun(self):
        fake = FakeRunner({("git", "-C", self.checkout, "remote", "get-url", "archive"): (0, f"git@github.com:{ARCHIVE}.git\n"),
                           ("git", "-C", self.checkout, "for-each-ref"): (0, "archive/main\t\n")}, checkout=self.checkout)
        code, out = self.main(fake, "--repoint-only")
        self.assertEqual(code, 0, out)
        actions = [argv[3:5] for argv in fake.argvs()]
        self.assertNotIn(("remote", "add"), actions)
        self.assertNotIn(("branch", "-m"), actions)
        self.assertNotIn(("branch", "--unset-upstream"), actions)
        self.assertIn(f"[ok] {self.checkout}: no private branches left", out)

    def test_repoint_rerun_clears_old_upstream(self):
        fake = FakeRunner({("git", "-C", self.checkout, "remote", "get-url", "archive"): (0, f"git@github.com:{ARCHIVE}.git\n"),
                           ("git", "-C", self.checkout, "for-each-ref"): (0, "archive/main\trefs/remotes/origin/main\n")},
                          checkout=self.checkout)
        code, out = self.main(fake, "--repoint-only")
        self.assertEqual(code, 0, out)
        argvs = fake.argvs()
        self.assertEqual([a for a in argvs if a[3:5] == ("branch", "--unset-upstream")],
                         [("git", "-C", self.checkout, "branch", "--unset-upstream", "archive/main")])
        self.assertNotIn(("branch", "-m"), [argv[3:5] for argv in argvs])
        self.assertIn("no private branches left", out)
        self.assertIn("unset upstream on archive/main", out)

    def test_repoint_shadowed_archive_branch(self):
        git = ("git", "-C", self.checkout)
        fake = FakeRunner({
            (*git, "remote", "get-url", "archive"): (0, f"git@github.com:{ARCHIVE}.git\n"),
            (*git, "for-each-ref", "--format=%(refname:short)%09%(upstream)", "refs/heads"):
                (0, "heads/archive/main\t\n"),
            (*git, "for-each-ref", "--format=%(refname:lstrip=2)%09%(upstream)", "refs/heads"):
                (0, "archive/main\t\n"),
        }, checkout=self.checkout)
        code, out = self.main(fake, "--repoint-only")
        self.assertEqual(code, 0, out)
        self.assertNotIn(("branch", "-m"), [argv[3:5] for argv in fake.argvs()])
        self.assertIn("no private branches left", out)

    def test_bad_usage_exits_2(self):
        no_checkout = [arg for arg in self.argv("--repoint-only") if arg not in ("--checkout", self.checkout)]
        for argv in (["--repo", REPO], no_checkout):
            with redirect_stdout(io.StringIO()), mock.patch("sys.stderr", io.StringIO()):
                with self.assertRaises(SystemExit) as raised:
                    tool.main(argv)
            self.assertEqual(raised.exception.code, 2, argv)


if __name__ == "__main__":
    unittest.main()
