import contextlib
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "cli"))
from support import ROOT, SKILL, IsolatedTestCase, snapshot


def fake_rustc(folder):
    (folder / "rustc").write_text("#!/bin/sh\nprintf 'rustc 1.100.0\\nhost: x86_64-unknown-linux-gnu\\n'\n")
    (folder / "rustc").chmod(0o755)


class InstallTest(IsolatedTestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("install", ROOT / "tools/install.py")
        self.installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.installer)
        self.root = Path(os.environ["HOME"]).resolve()
        self.destination = self.root / "skills"
        # The repo holds rust/Cargo.toml, so keep cargo out of every test but the Rust build ones.
        os.environ["FACTORY_RUNTIME"] = "python"

    def install(self, replace=False, source=SKILL):
        with contextlib.redirect_stdout(io.StringIO()) as out:
            result = self.installer.install(source, self.destination, replace)
        return result, out.getvalue().splitlines()

    def refused(self, call):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            call()
        self.assertEqual(error.exception.code, 2)

    def test_exact_copy_idempotence_and_unrelated_skills(self):
        self.destination.mkdir()
        other = self.destination / "other"
        other.mkdir()
        (other / "SKILL.md").write_text("other skill")
        target, lines = self.install()
        self.assertEqual(lines, [str(target)])
        installed = self.installer.tree(target)[0]
        self.assertEqual(set(installed), set(self.installer.REQUIRED) | {"bundle.json"})
        del installed["bundle.json"]
        self.assertEqual(installed, self.installer.tree(SKILL, source=True)[0])
        before = snapshot(self.destination)
        self.assertEqual(self.install()[0], target)
        self.assertEqual(snapshot(self.destination), before)
        self.assertEqual((other / "SKILL.md").read_text(), "other skill")
        self.assertFalse((self.destination / ".factory-install.lock").exists())

    def test_git_checkout_stamps_commit_and_tree(self):
        target, _ = self.install()
        stamp = (target / "bundle.json").read_text()
        head = subprocess.run(["git", "-C", str(SKILL), "rev-parse", "HEAD"], capture_output=True, text=True)
        files = self.installer.tree(SKILL, source=True)[0]
        tree = hashlib.sha256(b"".join(rel.encode() + b"\0" + hashlib.sha256(files[rel]).digest()
                                       for rel in sorted(files))).hexdigest()
        self.assertRegex(head.stdout.strip(), r"^[0-9a-f]{40}$")
        self.assertEqual(stamp, json.dumps({"commit": head.stdout.strip(), "tree": tree}, sort_keys=True) + "\n")
        spec = importlib.util.spec_from_file_location("installed", target / "runtime/factory.py")
        runtime = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runtime)
        self.assertEqual(runtime.bundle_stamp(), json.loads(stamp))

    def test_refusal_keeps_install_and_replacement_removes_its_backup(self):
        older = self.destination / "factory.backup-older"
        older.mkdir(parents=True)
        (older / "keep").write_text("keep")
        target, _ = self.install()
        (target / "SKILL.md").write_text("locally edited")
        old = snapshot(target)
        self.refused(self.install)
        self.assertEqual(snapshot(target), old)
        target, lines = self.install(replace=True)
        backup = Path(lines[1][len("removed "):])
        self.assertEqual(lines, [str(target), "removed " + str(backup)])
        self.assertTrue(backup.name.startswith("factory.backup-"))
        self.assertFalse(os.path.lexists(backup))
        (target / "SKILL.md").write_text("second edit")
        self.install(replace=True)
        self.assertEqual(list(self.destination.glob("factory.backup-*")), [older])
        self.assertEqual((older / "keep").read_text(), "keep")

    def test_backup_removal_failure_keeps_install_and_warns(self):
        target, _ = self.install()
        (target / "SKILL.md").write_text("old contents")
        rmtree = shutil.rmtree
        def fail_backup(path, *args, **kwargs):
            if Path(path).name.startswith("factory.backup-"):
                raise OSError("locked")
            return rmtree(path, *args, **kwargs)
        with patch.object(self.installer.shutil, "rmtree", side_effect=fail_backup), \
                contextlib.redirect_stderr(io.StringIO()) as err:
            result, lines = self.install(replace=True)
        self.assertEqual(result, target)
        self.assertEqual(lines, [str(target)])
        backups = list(self.destination.glob("factory.backup-*"))
        self.assertEqual(len(backups), 1)
        self.assertIn("install.py: could not remove " + str(backups[0]) + "; remove it by hand", err.getvalue())
        self.assertEqual((backups[0] / "SKILL.md").read_text(), "old contents")
        self.assertNotEqual((target / "SKILL.md").read_text(), "old contents")
        self.assertFalse((self.destination / ".factory-install.lock").exists())

    def test_exclusive_lock_is_never_broken(self):
        self.destination.mkdir()
        lock = self.destination / ".factory-install.lock"
        lock.write_text("owned by another installer")
        before = snapshot(self.destination)
        self.refused(self.install)
        self.assertEqual(snapshot(self.destination), before)

    def test_switch_failure_restores_old_target_without_removing_other_backups(self):
        target, _ = self.install()
        (target / "SKILL.md").write_text("old contents")
        old = snapshot(target)
        rename = os.rename
        def fail_switch(source, destination):
            if Path(source).name.startswith(".factory-stage-"):
                raise OSError("switch interrupted")
            return rename(source, destination)
        with patch.object(self.installer.os, "rename", side_effect=fail_switch):
            self.refused(lambda: self.install(replace=True))
        self.assertEqual(snapshot(target), old)
        self.assertFalse(list(self.destination.glob(".factory-stage-*")))
        self.assertFalse((self.destination / ".factory-install.lock").exists())

    def test_competing_target_is_not_overwritten_and_backup_survives(self):
        target, _ = self.install()
        (target / "SKILL.md").write_text("old contents")
        rename = os.rename
        def competitor(source, destination):
            result = rename(source, destination)
            if Path(destination).name.startswith("factory.backup-"):
                target.mkdir()
                (target / "other").write_text("competing target")
            return result
        with patch.object(self.installer.os, "rename", side_effect=competitor):
            self.refused(lambda: self.install(replace=True))
        self.assertEqual((target / "other").read_text(), "competing target")
        backups = list(self.destination.glob("factory.backup-*"))
        self.assertEqual(len(backups), 1)
        self.assertEqual((backups[0] / "SKILL.md").read_text(), "old contents")

    def test_detected_destination_edit_and_staging_failure_leave_old_install(self):
        target, _ = self.install()
        (target / "SKILL.md").write_text("old")
        tree = self.installer.tree
        def changed(root, source=False):
            if root == target and not source:
                before = tree(root, source)
                (target / "SKILL.md").write_text("new external edit")
                return before
            return tree(root, source)
        with patch.object(self.installer, "tree", side_effect=changed):
            self.refused(lambda: self.install(replace=True))
        self.assertEqual((target / "SKILL.md").read_text(), "new external edit")
        self.assertFalse(list(self.destination.glob("factory.backup-*")))
        with patch.object(self.installer.tempfile, "mkdtemp", side_effect=OSError("no space")):
            self.refused(lambda: self.install(replace=True))
        self.assertEqual((target / "SKILL.md").read_text(), "new external edit")

    def test_unsafe_paths_inventory_and_source_cache_exclusion(self):
        self.refused(lambda: self.installer.install(SKILL, Path("relative")))
        self.refused(lambda: self.installer.install(SKILL, SKILL / "inside"))
        clone = self.root / "source"
        shutil.copytree(SKILL, clone)
        cached = clone / "__pycache__"
        cached.mkdir()
        (cached / "factory.pyc").write_bytes(b"not shipped")
        (clone / ".development").write_text("not shipped")
        self.install(source=clone)
        self.assertFalse((self.destination / "factory/__pycache__").exists())
        self.assertFalse((self.destination / "factory/bundle.json").exists())
        missing = clone / "agents/factory-ux.md"
        missing.write_text("unexpected role")
        self.refused(lambda: self.install(source=clone))
        missing.unlink()
        (clone / "SKILL.md").unlink()
        self.refused(lambda: self.install(source=clone))
        if os.name != "nt":
            (clone / "SKILL.md").symlink_to(SKILL / "SKILL.md")
            self.refused(lambda: self.install(source=clone))
            alias = self.root / "alias"
            alias.symlink_to(self.destination, target_is_directory=True)
            self.refused(lambda: self.installer.install(SKILL, alias))

    def test_file_targets_and_parent_traversal_are_refused_without_writes(self):
        self.destination.mkdir()
        (self.destination / "factory").write_text("not a skill directory")
        before = snapshot(self.destination)
        self.refused(self.install)
        self.assertEqual(snapshot(self.destination), before)
        self.refused(lambda: self.installer.install(SKILL, self.root / "part/../skills"))

    def test_legacy_link_removed_on_install_and_identical_rerun(self):
        old = self.root / "old/huss-factory"
        old.mkdir(parents=True)
        (old / "SKILL.md").write_text("retired runtime")
        self.destination.mkdir()
        link = self.destination / "huss-factory"
        for _ in range(2):
            link.symlink_to(old, target_is_directory=True)
            target, lines = self.install()
            self.assertEqual(lines, [str(target), "removed " + str(link)])
            self.assertFalse(os.path.lexists(link))
            self.assertEqual((old / "SKILL.md").read_text(), "retired runtime")

    def test_legacy_folder_kept_and_refused_install_keeps_link(self):
        self.destination.mkdir()
        folder = self.destination / "huss-factory"
        folder.mkdir()
        with contextlib.redirect_stderr(io.StringIO()) as err:
            target, lines = self.install()
        self.assertEqual(lines, [str(target)])
        self.assertTrue(folder.is_dir())
        self.assertEqual(err.getvalue(), "install.py: " + str(folder)
                         + " is a folder, not a link; remove it by hand so agents load only factory\n")
        folder.rmdir()
        old = self.root / "old"
        old.mkdir()
        folder.symlink_to(old, target_is_directory=True)
        (target / "SKILL.md").write_text("locally edited")
        self.refused(self.install)
        self.assertTrue(folder.is_symlink())

    def test_legacy_link_removal_failure_exits_2(self):
        old = self.root / "old/huss-factory"
        old.mkdir(parents=True)
        self.destination.mkdir()
        link = self.destination / "huss-factory"
        link.symlink_to(old, target_is_directory=True)
        real_unlink, real_rmdir = os.unlink, os.rmdir

        def refuse(real):
            def call(path, *args, **kwargs):
                if os.fspath(path) == str(link):
                    raise PermissionError(path)
                return real(path, *args, **kwargs)
            return call

        with patch.object(self.installer.os, "unlink", refuse(real_unlink)), \
                patch.object(self.installer.os, "rmdir", refuse(real_rmdir)), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as err, \
                self.assertRaises(SystemExit) as error:
            self.installer.install(SKILL, self.destination)
        self.assertEqual(error.exception.code, 2)
        self.assertEqual(err.getvalue(), f"install.py: {link}: cannot safely install factory\n")
        self.assertTrue(link.is_symlink())
        self.assertFalse((self.destination / ".factory-install.lock").exists())

    def test_installed_only_offline_setup_adoption_roles_and_http_conflict(self):
        distribution = self.root / "distribution"
        (distribution / "tools").mkdir(parents=True)
        shutil.copyfile(ROOT / "tools/install.py", distribution / "tools/install.py")
        shutil.copytree(SKILL, distribution / "skills/factory")
        unrelated = self.root / "unrelated"
        unrelated.mkdir()
        subprocess.run([sys.executable, "-I", "-B", str(distribution / "tools/install.py"), "--destination", str(self.destination)],
                       cwd=unrelated, check=True, capture_output=True)
        shutil.rmtree(distribution)
        cli = self.destination / "factory/runtime/factory.py"
        def command(*args, input=None, expected=0):
            result = subprocess.run([sys.executable, "-I", "-B", str(cli)] + list(args), cwd=unrelated,
                                    input=input, capture_output=True, text=True)
            self.assertEqual(result.returncode, expected, result.stderr)
            self.assertNotIn("synthetic-secret", result.stdout + result.stderr)
            return result
        home = self.root / "retained-state"
        result = command("--home", str(home), "init", "--agents", "both")
        self.assertIn("Set api_url", result.stdout)
        self.assertEqual(len(list((self.root / ".claude/agents").glob("*.md"))), 9)
        command("adopt-home", str(home))
        command("doctor", expected=2)
        store = {"/projects.json": b'[{"slug":"sample","repo":"example/sample"}]',
                 "/api/projects/sample.md": b"---\nrepo: example/sample\nbase: main\n---\nProject\n",
                 "/api/board": b'{"jobs":[]}'}
        requests = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def respond(self, status, data):
                self.send_response(status)
                self.send_header("ETag", '"' + hashlib.sha256(data).hexdigest() + '"')
                self.end_headers()
                self.wfile.write(data)
            def do_GET(self):
                requests.append(("GET", self.path))
                if self.path.endswith("/?etags=1"):
                    prefix = self.path[:-len("?etags=1")]
                    data = json.dumps([{"name": key[len(prefix):], "etag": '"' + hashlib.sha256(value).hexdigest() + '"'}
                                       for key, value in store.items() if key.startswith(prefix)]).encode()
                    return self.respond(200, data)
                self.respond(200 if self.path in store else 404, store.get(self.path, b""))
            def do_PUT(self):
                requests.append(("PUT", self.path))
                data = self.rfile.read(int(self.headers["Content-Length"]))
                old = store.get(self.path)
                tag = '"' + hashlib.sha256(old).hexdigest() + '"' if old is not None else None
                if (self.headers.get("If-None-Match") == "*" and old is not None or
                        self.headers.get("If-Match") is not None and self.headers["If-Match"] != tag):
                    return self.respond(412, b"changed")
                store[self.path] = data
                self.respond(200, data)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            (home / "config.md").write_text("---\napi_url: http://127.0.0.1:" + str(server.server_port) + "\ndevice: test\n---\n")
            (home / ".env").write_text("FACTORY_ACCESS_CLIENT_ID=synthetic-id\nFACTORY_ACCESS_CLIENT_SECRET=synthetic-secret\n")
            report = json.loads(command("doctor").stdout)
            self.assertTrue(all(c["status"] == "pass" for c in report["checks"]))
            self.assertEqual(requests, [("GET", "/projects.json"), ("GET", "/api/board"), ("PUT", "/api/hosts/test/status")])
            self.assertIsNone(json.loads(store.pop("/api/hosts/test/status"))["bundle"])
            (home / "paths.md").write_text("---\nsample: " + str(unrelated) + "\n---\n")
            self.assertEqual(json.loads(command("where", "sample").stdout)["repo"], "example/sample")
            claim = command("claim", str(home), "test", "sample")
            job_id = claim.stdout.strip()
            self.assertEqual(job_id, "001-test")
            self.assertIn("new session", claim.stderr)
            session = json.loads(store["/api/board"])["jobs"][0]["claim"]["session"]
            command("board", str(home), job_id, "--session", session, input='{"stage":"build"}')
            artifact = home / "jobs/sample" / job_id / "build.md"
            artifact.write_text("initial")
            command("push", "sample/" + job_id)
            artifact.write_text("local edit")
            store["/api/jobs/sample/" + job_id + "/build.md"] = b"remote edit"
            command("sync", "sample", expected=6)
            self.assertEqual(artifact.read_text(), "local edit")
            self.assertEqual(artifact.with_name("build.md.remote").read_bytes(), b"remote edit")
            command("push", "sample/" + job_id, expected=6)
            command("init", "--agents", "both")
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
        self.assertFalse(distribution.exists())

    def checkout(self, cargo):
        """A git checkout holding the skill and a rust/Cargo.toml, and a fake cargo and rustc on PATH,
        cargo running the shell body cargo, or neither when it is None."""
        checkout = self.root / "checkout"
        shutil.copytree(SKILL, checkout / "skills/factory", ignore=shutil.ignore_patterns("__pycache__", ".*"))
        (checkout / "rust").mkdir()
        (checkout / "rust/Cargo.toml").write_text("[workspace]\n")
        for args in (["init", "-q"], ["add", "-A"], ["commit", "-q", "-m", "synthetic"]):
            subprocess.run(["git", "-C", str(checkout), *args], check=True, capture_output=True)
        if cargo is not None:
            bin_dir = self.root / "bin"
            bin_dir.mkdir()
            (bin_dir / "cargo").write_text("#!/bin/sh\n" + cargo)
            (bin_dir / "cargo").chmod(0o755)
            fake_rustc(bin_dir)
            os.environ["PATH"] = str(bin_dir) + os.pathsep + os.environ["PATH"]
        return checkout / "skills/factory"

    def install_rust(self, source):
        with contextlib.redirect_stderr(io.StringIO()) as err:
            target, _ = self.install(source=source)
        return target, err.getvalue(), json.loads((target / "bundle.json").read_text())

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_rust_build_installs_the_binary_and_its_entry(self):
        del os.environ["FACTORY_RUNTIME"]
        os.environ.pop("CARGO_TARGET_DIR", None)
        source = self.checkout('mkdir -p "$CARGO_TARGET_DIR/host" && printf "$STUB" > "$CARGO_TARGET_DIR/host/factory"\n'
                               'printf "%s\\n" "$PWD" "$@" > "$CARGO_TARGET_DIR/args"\n')
        os.environ["STUB"] = "stub"
        target, err, bundle = self.install_rust(source)
        self.assertEqual(err, "install: Rust binary built in 0 s\n")
        binary = target / "runtime/factory"
        self.assertEqual(binary.read_bytes(), b"stub")
        self.assertEqual(stat.S_IMODE(binary.stat().st_mode), 0o755)
        src = self.root / ".cache/huss-skills/factory/build-src"
        cache = self.root / ".cache/huss-skills/factory/cargo-target"
        self.assertEqual(stat.S_IMODE(cache.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(src.stat().st_mode), 0o700)
        self.assertEqual((cache / "args").read_text().splitlines(),
                         [str(src / "rust"), "build", "--profile", "host", "--locked", "--manifest-path",
                          str(src / "rust/Cargo.toml")])
        self.assertEqual((src / "rust/Cargo.toml").read_text(), "[workspace]\n")
        self.assertFalse((source.parents[1] / "rust/target").exists())
        self.assertEqual(bundle["entry"], ["runtime/factory"])
        self.assertEqual(bundle["binary_sha256"], hashlib.sha256(b"stub").hexdigest())
        files = self.installer.tree(target)[0]
        del files["bundle.json"]
        self.assertEqual(bundle["tree"], hashlib.sha256(b"".join(rel.encode() + b"\0" + hashlib.sha256(files[rel]).digest()
                                                                  for rel in sorted(files))).hexdigest())
        spec = importlib.util.spec_from_file_location("installed", target / "runtime/factory.py")
        runtime = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runtime)
        self.assertEqual(runtime.bundle_stamp(), {"commit": bundle["commit"], "tree": bundle["tree"]})
        self.assertEqual(runtime.catch_up_entry(target), [str(binary)])
        os.environ.update(CARGO_TARGET_DIR=str(self.root / "own-target"), STUB="stub2")
        (source.parents[1] / "rust/lib.rs").write_text("// changed\n")
        with contextlib.redirect_stderr(io.StringIO()):
            target, _ = self.install(replace=True, source=source)
        self.assertEqual((target / "runtime/factory").read_bytes(), b"stub2")
        self.assertTrue((self.root / "own-target/args").is_file())

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_unchanged_inputs_reuse_the_cached_binary_and_three_are_kept(self):
        del os.environ["FACTORY_RUNTIME"]
        os.environ.pop("CARGO_TARGET_DIR", None)
        source = self.checkout('echo >> "$HOME/runs" && mkdir -p "$CARGO_TARGET_DIR/host" && printf "$STUB" > "$CARGO_TARGET_DIR/host/factory"\n')
        lib = source.parents[1] / "rust/lib.rs"
        bins = self.root / ".cache/huss-skills/factory/bin"
        runs = lambda: len((self.root / "runs").read_text().splitlines())
        def rust(stub, text):
            os.environ["STUB"] = stub
            lib.write_text(text)
            with contextlib.redirect_stderr(io.StringIO()) as err:
                binary = self.installer.rust_binary(source.parents[1])
            return binary, err.getvalue()
        self.assertEqual(rust("one", "1")[0], b"one")
        binary, err = rust("other", "1")
        self.assertEqual((binary, runs()), (b"one", 1))
        key = [path.name for path in bins.iterdir()]
        self.assertEqual(err, "install: Rust binary from cache " + key[0][:8] + "\n")
        self.assertEqual(stat.S_IMODE(bins.stat().st_mode), 0o700)
        os.utime(bins / key[0], ns=(1, 1))
        for n in ("2", "3", "4"):
            self.assertEqual(rust(n, n), (n.encode(), "install: Rust binary built in 0 s\n"))
        self.assertEqual((runs(), len(list(bins.iterdir()))), (4, 3))
        self.assertFalse((bins / key[0]).exists())
        # The rustc version and host triple are part of the key.
        (self.root / "bin/rustc").write_text("#!/bin/sh\nprintf 'rustc 9.9.9\\nhost: aarch64-apple-darwin\\n'\n")
        self.assertEqual(rust("5", "4")[0], b"5")
        # A stored binary whose hash no longer matches is rebuilt, not installed.
        stored = max(bins.iterdir(), key=lambda path: path.stat().st_mtime_ns)
        (stored / "factory").write_bytes(b"tampered")
        self.assertEqual(rust("6", "4")[0], b"6")
        self.assertEqual(runs(), 6)

    def test_sync_rewrites_only_changed_files_and_drops_vanished_ones(self):
        root = self.root / "src"
        files = {"rust/a.rs": b"a", "rust/b.rs": b"b", "skills/factory/agents/x.md": b"x"}
        self.installer.sync(files, root)
        (root / self.installer.LOCK).write_text("")
        os.utime(root / "rust/a.rs", ns=(1, 1))
        os.utime(root / "rust/b.rs", ns=(1, 1))
        del files["skills/factory/agents/x.md"]
        files["rust/b.rs"] = b"b2"
        self.installer.sync(files, root)
        self.assertEqual((root / "rust/a.rs").stat().st_mtime_ns, 1)
        self.assertEqual((root / "rust/b.rs").read_bytes(), b"b2")
        self.assertNotEqual((root / "rust/b.rs").stat().st_mtime_ns, 1)
        self.assertFalse((root / "skills/factory/agents/x.md").exists())
        self.assertTrue((root / self.installer.LOCK).exists())

    def test_build_inputs_hold_every_file_the_crate_embeds(self):
        files = self.installer.build_inputs(ROOT)
        embedded = 0
        for rel in [rel for rel in files if rel.endswith(".rs")]:
            text = files[rel].decode()
            # An include the scan cannot read, such as include_str!(concat!(...)), fails here.
            self.assertEqual(len(re.findall(r"include_(?:str|bytes)!", text)), len(self.installer.EMBED.findall(text)), rel)
            for path in self.installer.EMBED.findall(text):
                self.assertIn(Path(os.path.normpath(Path(rel).parent / path)).as_posix(), files, rel)
                embedded += 1
        self.assertIn("skills/factory/agents/factory-builder.md", files)
        self.assertIn("skills/factory/templates/config.md", files)
        self.assertGreater(embedded, 10)
        self.assertFalse(any(rel.startswith("rust/target/") for rel in files))

    @unittest.skipIf(os.name == "nt", "modes and owners are POSIX")
    def test_build_folder_must_be_private(self):
        folder = self.root / "build-src"
        self.assertEqual(self.installer.private_dir(folder), folder)
        self.assertEqual(stat.S_IMODE(folder.stat().st_mode), 0o700)
        folder.chmod(0o755)
        with self.assertRaises(OSError):
            self.installer.private_dir(folder)
        (self.root / "link").symlink_to(self.root / "elsewhere", target_is_directory=True)
        (self.root / "elsewhere").mkdir(mode=0o700)
        with self.assertRaises(OSError):
            self.installer.private_dir(self.root / "link")

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_a_held_build_lock_makes_the_next_build_wait(self):
        del os.environ["FACTORY_RUNTIME"]
        os.environ.pop("CARGO_TARGET_DIR", None)
        source = self.checkout('touch "$HOME/ran" && mkdir -p "$CARGO_TARGET_DIR/host" && printf lock > "$CARGO_TARGET_DIR/host/factory"\n')
        src = self.installer.private_dir(self.root / ".cache/huss-skills/factory/build-src")
        results = []
        with contextlib.redirect_stderr(io.StringIO()) as err:
            with open(src / self.installer.LOCK, "a") as lock:
                self.installer.hold(lock)
                thread = threading.Thread(target=lambda: results.append(self.installer.rust_binary(source.parents[1])))
                thread.start()
                thread.join(0.5)
                self.assertTrue(thread.is_alive())
                self.assertFalse((self.root / "ran").exists())
            thread.join()
        self.assertEqual((results, err.getvalue()), ([b"lock"], "install: Rust binary built in 0 s\n"))

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_failed_build_keeps_the_python_runtime(self):
        del os.environ["FACTORY_RUNTIME"]
        source = self.checkout("echo 'error: synthetic failure' >&2\nexit 101\n")
        target, err, bundle = self.install_rust(source)
        self.assertEqual(err, "install: Rust build failed, keeping the Python runtime (error: synthetic failure)\n")
        self.assertFalse((target / "runtime/factory").exists())
        self.assertEqual(set(bundle), {"commit", "tree"})

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_python_runtime_or_no_cargo_skips_the_build(self):
        source = self.checkout('touch "$HOME/ran"\n')
        target, err, bundle = self.install_rust(source)
        self.assertEqual((err, set(bundle)), ("", {"commit", "tree"}))
        self.assertFalse((self.root / "ran").exists())
        del os.environ["FACTORY_RUNTIME"]
        with patch.object(self.installer.shutil, "which", return_value=None):
            target, err, bundle = self.install_rust(source)
        self.assertEqual(err, "install: cargo not found, keeping the Python runtime\n")
        self.assertFalse((target / "runtime/factory").exists())
        self.assertFalse((self.root / "ran").exists())

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_cargo_off_path_falls_back_to_home_cargo_bin(self):
        source = self.checkout(None)
        home_cargo = self.root / ".cargo/bin/cargo"
        home_cargo.parent.mkdir(parents=True)
        home_cargo.write_text('#!/bin/sh\nmkdir -p "$CARGO_TARGET_DIR/host" && printf home > "$CARGO_TARGET_DIR/host/factory"\n')
        home_cargo.chmod(0o755)
        fake_rustc(home_cargo.parent)
        os.environ.pop("CARGO_TARGET_DIR", None)
        with patch.object(self.installer.shutil, "which", return_value=None):
            self.assertIsNone(self.installer.rust_binary(source.parents[1]))
            del os.environ["FACTORY_RUNTIME"]
            target, err, bundle = self.install_rust(source)
        self.assertEqual(err, "install: Rust binary built in 0 s\n")
        self.assertEqual((target / "runtime/factory").read_bytes(), b"home")
        self.assertEqual(bundle["entry"], ["runtime/factory"])

    @unittest.skipIf(os.name == "nt", "the planted binary is a shell script")
    def test_launcher_hands_off_to_the_binary_beside_it(self):
        runtime = self.root / "launcher/runtime"
        runtime.mkdir(parents=True)
        shutil.copyfile(SKILL / "runtime/factory.py", runtime / "factory.py")
        (runtime / "factory").write_text('#!/bin/sh\nprintf "binary %s\\n" "$0" "$@"\n')
        (runtime / "factory").chmod(0o755)
        del os.environ["FACTORY_RUNTIME"]
        run = lambda: subprocess.run([sys.executable, "-B", str(runtime / "factory.py"), "where", "a b"],
                                     capture_output=True, text=True)
        self.assertEqual(run().stdout, f"binary {runtime / 'factory'}\nbinary where\nbinary a b\n")
        os.environ["FACTORY_RUNTIME"] = "python"
        result = run()
        self.assertNotIn("binary", result.stdout + result.stderr)
        self.assertTrue(result.stderr.startswith("factory.py: "), result.stderr)
        del os.environ["FACTORY_RUNTIME"]
        spec = importlib.util.spec_from_file_location("imported", runtime / "factory.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertTrue(callable(module.main))
