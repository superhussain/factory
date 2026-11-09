import json
import os
import stat
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime, snapshot


class SetupTest(IsolatedTestCase):
    def setUp(self):
        self.runtime = load_runtime()
        self.root = Path(os.environ["HOME"]).resolve()
        self.home = self.root / "state"
        self.descriptor = self.root / "connection.json"
        self.descriptor.write_text(json.dumps({"version": 1, "api_url": "https://example.invalid/"}))

    def call(self, *args):
        return invoke(self.runtime, ["--home", str(self.home), "init"] + list(args))

    def configured(self, endpoint="https://example.invalid"):
        self.home.mkdir(exist_ok=True)
        (self.home / "config.md").write_text("---\napi_url: " + endpoint + "\n---\n")
        (self.home / ".env").write_text("FACTORY_ACCESS_CLIENT_ID=synthetic-id\nFACTORY_ACCESS_CLIENT_SECRET=opaque.modern_test-secret\n")

    def test_clean_offline_setup_exact_copy_no_roles_git_or_network(self):
        r = self.runtime
        with patch.object(r, "api") as api, patch.object(r, "install_agents") as roles, patch.object(r, "main_checkout") as git:
            code, out, err = self.call()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out.splitlines(), [f"factory home: {self.home}",
            f"Set api_url in {self.home / 'config.md'}, then run /factory init again.", r.token_message(self.home / ".env")])
        api.assert_not_called()
        roles.assert_not_called()
        git.assert_not_called()
        self.assertEqual(r.frontmatter(self.home / "config.md")[0], {"api_url": "", "device": "", "claim_stale_minutes": "120"})
        self.assertEqual((self.home / ".env").read_bytes(), b"FACTORY_ACCESS_CLIENT_ID=\nFACTORY_ACCESS_CLIENT_SECRET=\n")
        if os.name != "nt":
            self.assertEqual(stat.S_IMODE((self.home / ".env").stat().st_mode), 0o600)

    def test_connection_validation_and_disagreement_make_no_writes(self):
        for data in (b"\xff", b"[]", b"{}", b'{"version":true,"api_url":"https://example.invalid"}',
                     b'{"version":1,"api_url":"http://localhost"}',
                     b'{"version":1,"api_url":"https://example.invalid","token":"synthetic"}'):
            self.descriptor.write_bytes(data)
            before = snapshot(self.root)
            self.assertEqual(self.call("--connection", str(self.descriptor))[0], 2)
            self.assertEqual(snapshot(self.root), before)
        self.configured()
        self.descriptor.write_text(json.dumps({"version": 1, "api_url": "https://other.invalid"}))
        before = snapshot(self.root)
        code, out, err = self.call("--connection", str(self.descriptor), "--agents", "both")
        self.assertEqual((code, out), (2, ""))
        self.assertIn(f"Connection api_url disagrees with {self.home / 'config.md'}; no files were changed.", err)
        self.assertEqual(snapshot(self.root), before)

    def test_import_seeds_only_endpoint_preserves_unknown_bytes_and_modes(self):
        self.home.mkdir()
        config = self.home / "config.md"
        config.write_bytes(b"---\r\n# retained\r\nunknown: legacy\r\napi_url:\r\ndevice: local\r\n---\r\nBody\r\n")
        env = self.home / ".env"
        env.write_bytes(b"# retained\r\nUNRELATED=opaque\r\n")
        env.chmod(0o640)
        config.chmod(0o644)
        before, mode = env.read_bytes(), stat.S_IMODE(env.stat().st_mode)
        self.assertEqual(self.call("--connection", str(self.descriptor))[0], 0)
        self.assertEqual(config.read_bytes(), b"---\r\n# retained\r\nunknown: legacy\r\napi_url: https://example.invalid\r\ndevice: local\r\n---\r\nBody\r\n")
        self.assertEqual((env.read_bytes(), stat.S_IMODE(env.stat().st_mode)), (before, mode))
        saved = snapshot(self.home)
        self.assertEqual(self.call("--connection", str(self.descriptor))[0], 0)
        self.assertEqual(snapshot(self.home), saved)

    def test_absent_endpoint_key_and_equivalent_trailing_slash(self):
        self.home.mkdir()
        config = self.home / "config.md"
        config.write_text("---\nunknown: retained\n---\nBody\n")
        self.assertEqual(self.call("--connection", str(self.descriptor))[0], 0)
        self.assertEqual(config.read_text(), "---\nunknown: retained\napi_url: https://example.invalid\n---\nBody\n")
        config.write_text("---\napi_url: https://example.invalid/\n---\n")
        before = snapshot(self.home)
        self.assertEqual(self.call("--connection", str(self.descriptor))[0], 0)
        self.assertEqual(snapshot(self.home), before)

    def test_setup_options_are_explicit_and_preflighted(self):
        for args in (("--agents",), ("--agents", ""), ("--agents", "other"),
                     ("--agents", "none", "--agents", "both"), ("--unknown",),
                     ("--connection",), ("--connection", str(self.descriptor), "--connection", str(self.descriptor))):
            self.assertEqual(self.call(*args)[0], 2)
            self.assertFalse(self.home.exists())
        for selected, runtimes in (("claude", ("claude",)), ("opencode", ("opencode",)), ("both", ("claude", "opencode"))):
            with patch.object(self.runtime, "install_agents") as install, patch.object(self.runtime, "api") as api:
                self.assertEqual(self.call("--agents", selected, str(self.root))[0], 0)
            self.assertEqual(install.call_args.args[1], runtimes)
            api.assert_not_called()

    def test_configured_init_continues_registration_with_opaque_credentials(self):
        self.configured()
        r = self.runtime
        body = b"---\nrepo: example/sample\nbase: main\ntitle: sample\n---\nBody\n"
        with patch.object(r, "main_checkout", return_value=self.root), patch.object(r, "remotes", return_value=["example/sample"]), patch.object(r, "api", return_value=(200, {"ETag": '"base"'}, body)) as api:
            self.assertEqual(self.call(str(self.root))[0], 0)
        self.assertTrue(all(call.args[1] == "GET" for call in api.call_args_list))
        self.assertIn("sample", r.read_paths(self.home))
        self.assertIn("opaque.modern_test-secret", (self.home / ".env").read_text())

    def test_origin_boundary_refuses_before_network(self):
        r = self.runtime
        for url in ("https://example.invalid/", "http://localhost:1234/", "http://127.0.0.1:4321"):
            self.assertEqual(r.normalized_origin(url), url.rstrip("/"))
        for url in ("http://example.invalid", "https://user:synthetic@example.invalid", "https://example.invalid/path",
                    "https://example.invalid?", "https://example.invalid#", "https://example.invalid:bad",
                    "https://example.invalid:99999", "https://example.invalid:", "https://example.invalid\n",
                    "https://example.invalid\\path", "https://", "file:///test"):
            self.configured(url)
            with patch.object(r, "frontmatter", return_value=({"api_url": url}, "")), patch.object(r.urllib.request, "build_opener") as opener:
                with self.assertRaises(SystemExit) as error:
                    r.api(self.home, "GET", "/api/board")
                self.assertEqual(error.exception.code, 2)
                opener.assert_not_called()
