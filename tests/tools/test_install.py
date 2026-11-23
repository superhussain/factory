import contextlib
import errno
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
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "cli"))
from support import ROOT, SKILL, IsolatedTestCase


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
        os.environ.pop("CARGO_TARGET_DIR", None)
        # The installed runtime below runs as Python; the binary has its own suites.
        os.environ["FACTORY_RUNTIME"] = "python"

    def refused(self, call):
        with contextlib.redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit) as error:
            call()
        self.assertEqual(error.exception.code, 2)
        return err.getvalue()

    def test_installed_only_offline_setup_adoption_roles_and_http_conflict(self):
        # factory install's own copy is tested in Rust; this runs the Python runtime from a copy away from the checkout.
        shutil.copytree(SKILL, self.destination / "factory", ignore=shutil.ignore_patterns("__pycache__"))
        unrelated = self.root / "unrelated"
        unrelated.mkdir()
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

    def checkout(self, cargo):
        """A git checkout holding the skill, rust/Cargo.toml and this installer, and a fake cargo and rustc
        on PATH, cargo running the shell body cargo, or neither when it is None."""
        checkout = self.root / "checkout"
        shutil.copytree(SKILL, checkout / "skills/factory", ignore=shutil.ignore_patterns("__pycache__", ".*"))
        (checkout / "rust").mkdir()
        (checkout / "rust/Cargo.toml").write_text("[workspace]\n")
        (checkout / "tools").mkdir()
        shutil.copyfile(ROOT / "tools/install.py", checkout / "tools/install.py")
        if cargo is not None:
            bin_dir = self.root / "bin"
            bin_dir.mkdir()
            (bin_dir / "cargo").write_text("#!/bin/sh\n" + cargo)
            (bin_dir / "cargo").chmod(0o755)
            fake_rustc(bin_dir)
            os.environ["PATH"] = str(bin_dir) + os.pathsep + os.environ["PATH"]
        return checkout

    def rust(self, checkout):
        """The bytes of the binary rust_binary returns, and what it wrote to stderr."""
        with contextlib.redirect_stderr(io.StringIO()) as err:
            binary = self.installer.rust_binary(checkout)
        return binary.read_bytes(), err.getvalue()

    @unittest.skipIf(os.name == "nt", "the fake cargo and binary are shell scripts")
    def test_shim_builds_then_runs_the_binary_install(self):
        planted = self.root / "planted"
        planted.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$HOME/install-args"\nexit 3\n')
        planted.chmod(0o755)
        checkout = self.checkout('echo >> "$HOME/runs" && mkdir -p "$CARGO_TARGET_DIR/host" && cp "$HOME/planted" "$CARGO_TARGET_DIR/host/factory"\n')
        def shim(*args):
            return subprocess.run([sys.executable, "-B", str(checkout / "tools/install.py"), *args],
                                  capture_output=True, text=True)
        result = shim("--destination", str(self.destination), "--replace")
        self.assertEqual((result.returncode, result.stderr), (3, "install: Rust binary built in 0 s\n"))
        self.assertEqual((self.root / "install-args").read_text().splitlines(),
                         ["install", "--source", str(checkout), "--destination", str(self.destination), "--replace"])
        result = shim("--destination", "relative")
        self.assertTrue(result.stderr.startswith("install: Rust binary from cache "), result.stderr)
        self.assertEqual((self.root / "install-args").read_text().splitlines(),
                         ["install", "--source", str(checkout), "--destination", "relative"])
        self.assertEqual(len((self.root / "runs").read_text().splitlines()), 1)

    @unittest.skipIf(os.name == "nt", "PATH holds no cargo only on a POSIX test host")
    def test_no_cargo_refuses_before_any_write(self):
        checkout = self.checkout(None)
        empty = self.root / "empty-bin"
        empty.mkdir()
        result = subprocess.run([sys.executable, "-B", str(checkout / "tools/install.py"), "--destination",
                                 str(self.destination)], capture_output=True, text=True, env=dict(os.environ, PATH=str(empty)))
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (2, "", "install: cargo not found; install rustup from https://rustup.rs, then run the install again\n"))
        self.assertFalse(self.destination.exists())
        self.assertFalse((self.root / ".cache").exists())
        (checkout / "rust/Cargo.toml").unlink()
        self.assertEqual(self.refused(lambda: self.installer.rust_binary(checkout)),
                         "install: " + str(checkout) + " has no rust/Cargo.toml\n")

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_rust_build_uses_the_private_build_folder(self):
        checkout = self.checkout('mkdir -p "$CARGO_TARGET_DIR/host" && printf "$STUB" > "$CARGO_TARGET_DIR/host/factory"\n'
                                 'printf "%s\\n" "$PWD" "$@" > "$CARGO_TARGET_DIR/args"\n')
        os.environ["STUB"] = "stub"
        binary, err = self.rust(checkout)
        self.assertEqual((binary, err), (b"stub", "install: Rust binary built in 0 s\n"))
        src = self.root / ".cache/huss-skills/factory/build-src"
        cache = self.root / ".cache/huss-skills/factory/cargo-target"
        self.assertEqual(stat.S_IMODE(cache.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(src.stat().st_mode), 0o700)
        self.assertEqual((cache / "args").read_text().splitlines(),
                         [str(src / "rust"), "build", "--profile", "host", "--locked", "--manifest-path",
                          str(src / "rust/Cargo.toml")])
        self.assertEqual((src / "rust/Cargo.toml").read_text(), "[workspace]\n")
        self.assertFalse((checkout / "rust/target").exists())
        os.environ.update(CARGO_TARGET_DIR=str(self.root / "own-target"), STUB="stub2")
        (checkout / "rust/lib.rs").write_text("// changed\n")
        self.assertEqual(self.rust(checkout)[0], b"stub2")
        self.assertTrue((self.root / "own-target/args").is_file())

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_unchanged_inputs_reuse_the_cached_binary_and_three_are_kept(self):
        checkout = self.checkout('echo >> "$HOME/runs" && mkdir -p "$CARGO_TARGET_DIR/host" && printf "$STUB" > "$CARGO_TARGET_DIR/host/factory"\n')
        lib = checkout / "rust/lib.rs"
        bins = self.root / ".cache/huss-skills/factory/bin"
        runs = lambda: len((self.root / "runs").read_text().splitlines())
        def rust(stub, text):
            os.environ["STUB"] = stub
            lib.write_text(text)
            return self.rust(checkout)
        self.assertEqual(rust("one", "1")[0], b"one")
        binary, err = rust("other", "1")
        self.assertEqual((binary, runs()), (b"one", 1))
        key = [path.name for path in bins.iterdir()]
        self.assertEqual(err, "install: Rust binary from cache " + key[0][:8] + "\n")
        self.assertEqual(stat.S_IMODE(bins.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((bins / key[0] / "factory").stat().st_mode), 0o700)
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
        self.assertEqual(sorted(path.name for path in stored.iterdir()), ["factory", "factory.sha256"])
        self.assertEqual(len(list(bins.iterdir())), 3)
        # Rust flags and the user's cargo config are part of the key too.
        os.environ["RUSTFLAGS"] = "-C target-cpu=native"
        self.assertEqual(rust("7", "4")[0], b"7")
        del os.environ["RUSTFLAGS"]
        self.assertEqual(rust("8", "4")[0], b"6")
        os.environ["CARGO_HOME"] = str(self.root / "cargo-home")
        (self.root / "cargo-home").mkdir()
        (self.root / "cargo-home/config.toml").write_text("[build]\nrustflags = ['-Copt-level=1']\n")
        self.assertEqual(rust("9", "4")[0], b"9")
        self.assertEqual(runs(), 8)

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_rust_install_reuses_the_shim_build(self):
        """The binary's own install computes the same key, so it finds the shim's build in the cache."""
        factory = ROOT / "rust/target/host/factory"
        if not factory.is_file():
            self.skipTest("no host build of the binary")
        planted = self.root / "planted"
        shutil.copyfile(factory, planted)
        checkout = self.checkout('echo >> "$HOME/runs" && mkdir -p "$CARGO_TARGET_DIR/host" && cp "$HOME/planted" "$CARGO_TARGET_DIR/host/factory"\n')
        result = subprocess.run([sys.executable, "-B", str(checkout / "tools/install.py"), "--destination",
                                 str(self.destination)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        key = next((self.root / ".cache/huss-skills/factory/bin").iterdir()).name
        self.assertEqual(result.stderr, "install: Rust binary built in 0 s\ninstall: Rust binary from cache " + key[:8] + "\n")
        self.assertEqual(result.stdout, str(self.destination / "factory") + "\n")
        self.assertEqual((self.destination / "factory/runtime/factory").read_bytes(), planted.read_bytes())
        self.assertEqual(len((self.root / "runs").read_text().splitlines()), 1)

    def test_windows_lock_retries_only_while_busy_and_gives_up(self):
        busy = OSError(errno.EDEADLOCK, "busy")
        def hold(effects, clock=(0, 0, 0)):
            msvcrt = SimpleNamespace(LK_LOCK=1, locking=Mock(side_effect=effects))
            with patch.dict(sys.modules, msvcrt=msvcrt), patch.object(self.installer.os, "name", "nt"), \
                    patch.object(self.installer.time, "monotonic", side_effect=clock), open(self.root / "lock", "a") as lock:
                try:
                    self.installer.hold(lock)
                finally:
                    calls.append(msvcrt.locking.call_count)
        calls = []
        hold([busy, None])
        with self.assertRaises(OSError):
            hold([OSError(errno.EBADF, "bad"), None])
        with self.assertRaises(OSError):
            hold([busy, busy, None], clock=(0, 1, self.installer.LOCK_WAIT))
        self.assertEqual(calls, [2, 1, 2])

    def test_missing_localappdata_fails_the_build(self):
        checkout = self.checkout(None)
        os.environ.pop("LOCALAPPDATA", None)
        with patch.object(self.installer.shutil, "which", return_value="/no/cargo"), \
                patch.object(self.installer.os, "name", "nt"):
            err = self.refused(lambda: self.installer.rust_binary(checkout))
        self.assertEqual(err, "install: Rust build failed (LOCALAPPDATA is not set)\n")

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
        # The Rust build module names the macros in its own comments and patterns.
        for rel in [rel for rel in files if rel.endswith(".rs") and rel != "rust/factory/src/shared/build.rs"]:
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
        checkout = self.checkout('touch "$HOME/ran" && mkdir -p "$CARGO_TARGET_DIR/host" && printf lock > "$CARGO_TARGET_DIR/host/factory"\n')
        src = self.installer.private_dir(self.root / ".cache/huss-skills/factory/build-src")
        results = []
        with contextlib.redirect_stderr(io.StringIO()) as err:
            with open(src / self.installer.LOCK, "a") as lock:
                self.installer.hold(lock)
                thread = threading.Thread(target=lambda: results.append(self.installer.rust_binary(checkout).read_bytes()))
                thread.start()
                thread.join(0.5)
                self.assertTrue(thread.is_alive())
                self.assertFalse((self.root / "ran").exists())
            thread.join()
        self.assertEqual((results, err.getvalue()), ([b"lock"], "install: Rust binary built in 0 s\n"))

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_failed_build_exits_2(self):
        checkout = self.checkout("echo 'error: synthetic failure' >&2\nexit 101\n")
        err = self.refused(lambda: self.installer.rust_binary(checkout))
        self.assertEqual(err, "install: Rust build failed (error: synthetic failure)\n")

    @unittest.skipIf(os.name == "nt", "the fake cargo is a shell script")
    def test_cargo_off_path_falls_back_to_home_cargo_bin(self):
        checkout = self.checkout(None)
        home_cargo = self.root / ".cargo/bin/cargo"
        home_cargo.parent.mkdir(parents=True)
        home_cargo.write_text('#!/bin/sh\nmkdir -p "$CARGO_TARGET_DIR/host" && printf home > "$CARGO_TARGET_DIR/host/factory"\n')
        home_cargo.chmod(0o755)
        fake_rustc(home_cargo.parent)
        with patch.object(self.installer.shutil, "which", return_value=None):
            self.assertEqual(self.rust(checkout), (b"home", "install: Rust binary built in 0 s\n"))

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
