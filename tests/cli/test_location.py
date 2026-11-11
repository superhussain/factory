import concurrent.futures
import contextlib
import errno
import io
import json
import os
import stat
import subprocess
import tempfile
import threading
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime, snapshot


class LocationTest(IsolatedTestCase):
    def setUp(self):
        self.runtime = load_runtime()
        self.root = Path(os.environ["HOME"]).resolve()
        self.home = self.state("existing")
        self.pointer = self.root / "config/factory-location.json"

    def state(self, name):
        home = self.root / name
        home.mkdir()
        (home / "config.md").write_bytes(b"---\napi_url:\nunknown: retained\n---\nBody\n")
        return home.resolve()

    def rejected(self, call):
        with self.assertRaises(SystemExit) as raised:
            call()
        self.assertEqual(raised.exception.code, 2)

    def test_precedence_empty_values_and_no_creation(self):
        r = self.runtime
        default = self.root / "config/factory"
        self.assertEqual(r.factory_home(), default.resolve())
        self.assertFalse(default.exists())
        r.adopt_home(self.home, self.pointer)
        self.assertEqual(r.factory_home(), self.home)
        with patch.dict(os.environ, {"FACTORY_HOME": str(self.root / "environment")}):
            self.assertEqual(r.factory_home(), (self.root / "environment").resolve())
            r._HOME_OVERRIDE = self.root / "explicit"
            self.assertEqual(r.factory_home(), (self.root / "explicit").resolve())
        r._HOME_OVERRIDE = None
        with patch.dict(os.environ, {"FACTORY_HOME": "", "XDG_CONFIG_HOME": ""}):
            self.assertEqual(r.factory_home(), self.root / ".config/factory")
        with patch.dict(os.environ, {"XDG_CONFIG_HOME": "relative"}):
            self.rejected(r.factory_home)
        with patch.dict(os.environ, {"FACTORY_HOME": "relative"}):
            self.rejected(r.factory_home)

    def test_bad_home_says_nothing_was_written(self):
        cases = [({"FACTORY_HOME": "relative/home"}, None, "factory home must be absolute"),
                 ({"FACTORY_HOME": str(self.root / "loop")}, RuntimeError, "cannot resolve factory home")]
        for env, error, message in cases:
            with self.subTest(message=message), patch.dict(os.environ, env), \
                    (patch.object(self.runtime.Path, "resolve", side_effect=error) if error else contextlib.nullcontext()), \
                    patch("sys.stderr", new_callable=io.StringIO) as err:
                self.rejected(self.runtime.factory_home)
                self.assertEqual(err.getvalue(), f"factory.py: {message}. Nothing was written.\n")

    def test_malformed_pointer_never_falls_back(self):
        self.pointer.parent.mkdir()
        values = [b"\xff", b"not json", b"[]", b'{"version":true,"home":"/"}',
                  json.dumps({"version": 1, "home": str(self.home), "extra": 1}).encode(),
                  b'{"version":1,"home":"relative"}',
                  json.dumps({"version": 1, "home": str(self.home / ".." / self.home.name)}).encode(),
                  json.dumps({"version": 1, "home": str(self.root / "missing")}).encode()]
        for value in values:
            self.pointer.write_bytes(value)
            self.rejected(self.runtime.factory_home)
        with patch.dict(os.environ, {"FACTORY_HOME": str(self.home)}):
            self.assertEqual(self.runtime.factory_home(), self.home)
        self.pointer.unlink()
        self.pointer.mkdir()
        self.rejected(self.runtime.factory_home)

    def test_pointer_and_metadata_links_refused_but_home_alias_allowed(self):
        if os.name == "nt":
            self.skipTest("symlink creation permissions are POSIX-specific; junction tested in legacy suite on Windows")
        alias = self.root / "alias"
        alias.symlink_to(self.home, target_is_directory=True)
        self.runtime.adopt_home(alias, self.pointer)
        self.assertEqual(self.runtime.factory_home(), self.home)
        self.pointer.unlink()
        other = self.root / "pointer-data"
        other.write_text(json.dumps({"version": 1, "home": str(self.home)}))
        self.pointer.symlink_to(other)
        self.rejected(self.runtime.factory_home)
        self.pointer.unlink()
        projects = self.home / "projects"
        projects.symlink_to(self.root, target_is_directory=True)
        self.rejected(lambda: self.runtime.adopt_home(self.home, self.pointer))
        self.assertFalse(self.pointer.exists())

    def test_one_command_validates_the_pointer_home_once(self):
        r = self.runtime
        r.adopt_home(self.home, self.pointer)
        board = {"updated": "", "jobs": [{"id": job_id, "project": "sample", "stage": "build"}
                                         for job_id in ("001-build", "002-build", "003-build")]}

        def api(home, method, path, *args, **kwargs):
            return (200, {}, r.dump_board(board)) if path == "/api/board" else (200, {}, "[]")

        with patch.object(r, "api", side_effect=api), \
                patch.object(r, "validate_home", wraps=r.validate_home) as validate:
            code, out, _ = invoke(r, ["sync", "sample"])
            self.assertEqual(code, 0)
            self.assertEqual(validate.call_count, 1)
            self.assertIn("sample/003-build: pulled 0 files", out)
            r.factory_home()
            self.assertEqual(validate.call_count, 2)

    def test_idempotence_conflicts_and_permissions(self):
        self.runtime.adopt_home(self.home, self.pointer)
        before = snapshot(self.pointer.parent)
        self.runtime.adopt_home(self.home, self.pointer)
        self.assertEqual(snapshot(self.pointer.parent), before)
        if os.name != "nt":
            self.assertEqual(stat.S_IMODE(self.pointer.stat().st_mode), 0o600)
        other = self.state("other")
        self.rejected(lambda: self.runtime.adopt_home(other, self.pointer))
        default = self.pointer.parent / "factory"
        default.mkdir()
        (default / "config.md").write_text("malformed but initialized")
        self.rejected(lambda: self.runtime.adopt_home(self.home, self.pointer))
        self.assertEqual(snapshot(self.pointer.parent)["factory-location.json"], before["factory-location.json"])

    def test_unreadable_pointer_and_trusted_default_alias(self):
        if os.name == "nt":
            self.skipTest("POSIX permissions and symlink alias; Windows owns real junction checks")
        self.runtime.adopt_home(self.home, self.pointer)
        (self.pointer.parent / "factory").symlink_to(self.home, target_is_directory=True)
        self.runtime.adopt_home(self.home, self.pointer)
        self.pointer.chmod(0)
        try:
            self.rejected(self.runtime.factory_home)
        finally:
            self.pointer.chmod(0o600)

    def test_metadata_validation_keeps_dirty_remote_and_empty_manifests(self):
        (self.home / "paths.md").write_text("---\nsample: " + str(self.root / "not-present") + "\n---\n")
        projects = self.home / "projects"
        projects.mkdir()
        (projects / "sample.md").write_text("---\npath: legacy\nunknown: retained\n---\nDirty")
        (projects / "sample.md.remote").write_bytes(b"remote")
        state = {"version": 1, "etag": '"base"', "sha256": "0" * 64}
        metadata = projects / "sample.sync.json"
        metadata.write_text(json.dumps(state))
        job = self.home / "jobs/sample/001-test"
        job.mkdir(parents=True)
        (job / ".sync.json").write_text("{}")
        (job / "build.md.remote").write_bytes(b"other device")
        before = snapshot(self.home)
        self.runtime.adopt_home(self.home, self.pointer)
        self.assertEqual(snapshot(self.home), before)
        for bad in ({}, dict(state, version=True), dict(state, etag='W/"base"')):
            metadata.write_text(json.dumps(bad))
            self.rejected(lambda: self.runtime.validate_home(self.home))
        metadata.write_text(json.dumps(state))
        for bad in ([], {"../escape.md": {"etag": "x", "sha256": "0" * 64}},
                    {"build.md": {"etag": "", "sha256": "0" * 64}},
                    {"build.md": {"etag": "x", "sha256": "A" * 64}}):
            (job / ".sync.json").write_text(json.dumps(bad))
            self.rejected(lambda: self.runtime.validate_home(self.home))

    def test_unreadable_metadata_and_unlistable_tree(self):
        if os.name == "nt":
            self.skipTest("POSIX permission bits; Windows real junction test owns its filesystem check")
        config = self.home / "config.md"
        config.chmod(0)
        try:
            self.rejected(lambda: self.runtime.adopt_home(self.home, self.pointer))
        finally:
            config.chmod(0o644)
        folder = self.home / "jobs"
        folder.mkdir()
        folder.chmod(0)
        try:
            self.rejected(lambda: self.runtime.adopt_home(self.home, self.pointer))
        finally:
            folder.chmod(0o755)

    def test_interrupted_publication_cleans_only_own_temporary(self):
        for operation in ("fsync", "link"):
            with patch.object(self.runtime.os, operation, side_effect=OSError(errno.EOPNOTSUPP, "unsupported")):
                self.rejected(lambda: self.runtime.adopt_home(self.home, self.pointer))
            self.assertFalse(self.pointer.exists())
            self.assertEqual(list(self.pointer.parent.iterdir()), [])
        original = self.runtime.os.fdopen
        class Interrupted:
            def __init__(self, fd, *args, **kwargs):
                self.stream = original(fd, *args, **kwargs)
            def __enter__(self):
                self.stream.close()
                raise OSError("interrupted write")
            def __exit__(self, *args):
                self.stream.close()
        with patch.object(self.runtime.os, "fdopen", Interrupted):
            self.rejected(lambda: self.runtime.adopt_home(self.home, self.pointer))
        self.assertEqual(list(self.pointer.parent.iterdir()), [])

    def test_simultaneous_different_adopters_and_identical_winner(self):
        other = self.state("other")
        actual = os.link
        for selections in ((self.home, other), (self.home, self.home)):
            barrier = threading.Barrier(2)
            def publish(source, target):
                barrier.wait(timeout=5)
                return actual(source, target)
            def adopt(home):
                try:
                    self.runtime.adopt_home(home, self.pointer)
                    return 0
                except SystemExit as exc:
                    return exc.code
            with patch.object(self.runtime.os, "link", side_effect=publish), concurrent.futures.ThreadPoolExecutor(2) as pool:
                results = list(pool.map(adopt, selections))
            self.assertEqual(sorted(results), [0, 2] if selections[0] != selections[1] else [0, 0])
            self.assertIn(self.runtime.read_location(self.pointer), selections)
            self.assertEqual(len(list(self.pointer.parent.iterdir())), 1)
            self.pointer.unlink()

    def test_real_git_worktree_and_state_unchanged(self):
        repo = self.root / "repo"
        repo.mkdir()
        def git(*args):
            return subprocess.run(["git", "-C", str(repo)] + list(args), check=True, capture_output=True).stdout
        git("init")
        git("commit", "--allow-empty", "-m", "initial")
        git("worktree", "add", "-b", "factory/001-test", str(self.home / "worktrees/sample/001-test"))
        (self.home / ".env").write_bytes(b"FACTORY_ACCESS_CLIENT_SECRET=synthetic-private\nunknown=retained\n")
        (self.home / ".env").chmod(0o640)
        before, admin, listed = snapshot(self.home), snapshot(repo / ".git"), git("worktree", "list", "--porcelain")
        self.runtime.adopt_home(self.home, self.pointer)
        self.assertEqual(snapshot(self.home), before)
        self.assertEqual(snapshot(repo / ".git"), admin)
        self.assertEqual(git("worktree", "list", "--porcelain"), listed)

    def test_global_options_preflight_scope_and_positional_authority(self):
        r = self.runtime
        for args in (["next", "--home"], ["next", "--session", ""],
                     ["next", "--session", "a", "--session", "b"],
                     ["next", "--home", str(self.home), "--home", str(self.home)],
                     ["next", "--home", "--session", "tag"],
                     ["adopt-home", str(self.home), "--home", str(self.home)]):
            self.assertEqual(invoke(r, args)[0], 2)
        for name, args in (("claim", [str(self.home), "feature", "sample"]), ("board", [str(self.home), "001-test"])):
            called = []
            def command(*operands):
                called.append((operands, r.factory_home(), os.environ.get("FACTORY_SESSION")))
            low, high = r.COMMANDS[name][1:]
            with patch.dict(r.COMMANDS, {name: (command, low, high)}):
                self.assertEqual(invoke(r, [name] + args + ["--home", str(self.root), "--session", "tag"])[0], 2)
                self.assertEqual(called, [])
                self.assertEqual(invoke(r, ["--home", str(self.home), name] + args + ["--session", "tag"])[0], 0)
                self.assertEqual(called[-1], (tuple(args), self.home, "tag"))
                with patch.dict(os.environ, {"FACTORY_HOME": str(self.root)}):
                    self.assertEqual(invoke(r, [name] + args)[0], 0)
                    self.assertEqual(called[-1][0], tuple(args))
            self.assertIsNone(r._HOME_OVERRIDE)
            self.assertNotIn("FACTORY_SESSION", os.environ)
