"""Fixed JSON deployment projection: validation, determinism, nonmutation and locked
invocation, with Wrangler/Node subprocess fakes for preflight and deploy."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURES = ROOT / "tests" / "fixtures"
VALID = FIXTURES / "deployment-valid.json"
INVALID = FIXTURES / "deployment-invalid.json"
CONFIG = ROOT / ".build" / "wrangler.production.json"
CONNECTION = ROOT / ".build" / "connection.json"

spec = importlib.util.spec_from_file_location("factory_deploy", ROOT / "tools" / "deploy.py")
deploy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(deploy)


def run_cli(*args, cwd=None):
    return subprocess.run(
        ["python3", str(ROOT / "tools" / "deploy.py"), *args],
        capture_output=True, text=True, cwd=cwd or ROOT)


def outputs():
    return {path: path.read_bytes() for path in (CONFIG, CONNECTION) if path.exists()}


class ProfileValidationTest(unittest.TestCase):
    def test_valid_check_writes_nothing(self):
        before = outputs()
        result = run_cli("check", "--profile", str(VALID))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("profile ok: factory-example", result.stdout)
        self.assertEqual(outputs(), before)

    def test_named_invalid_cases(self):
        base = json.loads(VALID.read_text())
        document = json.loads(INVALID.read_text())
        self.assertEqual(document["version"], 1)
        names = set()
        for case in document["cases"]:
            with self.subTest(case=case["name"]):
                self.assertNotIn(case["name"], names, "unique case names")
                names.add(case["name"])
                with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
                    if "raw" in case:
                        handle.write(case["raw"])
                    else:
                        profile = dict(base)
                        for key in case.get("remove", []):
                            profile.pop(key)
                        profile.update(case.get("set", {}))
                        json.dump(profile, handle)
                    path = handle.name
                try:
                    before = outputs()
                    result = run_cli("render", "--profile", path)
                    self.assertEqual(result.returncode, 2, case["name"] + ": " + result.stderr)
                    self.assertIn(case["error"], result.stderr, case["name"])
                    self.assertEqual(outputs(), before, "validation failure changed no output")
                finally:
                    os.unlink(path)

    def test_overlay_and_test_entry_keys_rejected(self):
        base = json.loads(VALID.read_text())
        for key, value in [("main", "tests/support/local-entry.js"), ("vars", {}), ("env", {}),
                           ("routes", []), ("secrets", {}), ("assets", "./web"), ("auth", {})]:
            with self.subTest(key=key):
                with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
                    json.dump({**base, key: value}, handle)
                    path = handle.name
                try:
                    before = outputs()
                    result = run_cli("render", "--profile", path)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn("unknown profile keys", result.stderr)
                    self.assertEqual(outputs(), before)
                finally:
                    os.unlink(path)

    def test_usage_errors(self):
        for args in [(), ("check",), ("render", "--profile"), ("bogus", "--profile", str(VALID)),
                     ("check", "--profile", str(VALID), "extra")]:
            with self.subTest(args=args):
                self.assertEqual(run_cli(*args).returncode, 2)


class RenderTest(unittest.TestCase):
    def test_render_is_deterministic_and_fixed(self):
        first = run_cli("render", "--profile", str(VALID))
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout.strip().split("\n"), [str(CONFIG), str(CONNECTION)])
        snapshot = outputs()
        second = run_cli("render", "--profile", str(VALID))
        self.assertEqual(second.returncode, 0)
        self.assertEqual(outputs(), snapshot, "render is byte deterministic")
        config = json.loads(snapshot[CONFIG])
        self.assertEqual(config, {
            "account_id": "00000000000000000000000000000000",
            "ai": {"binding": "AI"},
            "compatibility_date": "2026-06-01",
            "durable_objects": {"bindings": [{"name": "BoardEvents", "class_name": "BoardEvents"}]},
            "main": str(ROOT / "worker" / "index.js"),
            "migrations": [{"tag": "v1", "new_sqlite_classes": ["BoardEvents"]}],
            "name": "factory-example",
            "preview_urls": False,
            "r2_buckets": [{"binding": "BUCKET", "bucket_name": "factory-example"}],
            "routes": [{"custom_domain": True, "pattern": "factory.example.test"}],
            "vars": {
                "ACCESS_AUDIENCE": "example-application-audience",
                "ACCESS_ISSUER": "https://example-team.cloudflareaccess.com",
                "FACTORY_AGENT_LINKS": "{}",
                "FACTORY_IMAGE_ORIGINS": "[]",
                "FACTORY_ORIGIN": "https://factory.example.test",
                "FACTORY_TITLE": "Factory",
            },
            "workers_dev": False,
        })
        self.assertEqual(json.loads(snapshot[CONNECTION]),
                         {"api_url": "https://factory.example.test", "version": 1})
        for text in (snapshot[CONFIG], snapshot[CONNECTION]):
            self.assertTrue(text.endswith(b"\n"))
            self.assertNotIn(b"tests/support", text)
            self.assertNotIn(b"local-entry", text)

    def test_render_from_another_cwd(self):
        before = outputs()
        with tempfile.TemporaryDirectory() as elsewhere:
            result = run_cli("render", "--profile", str(VALID), cwd=elsewhere)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(outputs(), before, "render from another cwd writes the same bytes")

    def test_render_with_image_publication_configured_and_links_disabled(self):
        profile = {**json.loads(VALID.read_text()), "image_origins": ["https://images.example.test"]}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "profile.json"
            path.write_text(json.dumps(profile))
            config = deploy.project_wrangler(deploy.load_profile(path), ROOT, Path(directory) / "out")
            variables = json.loads(Path(config).read_text())["vars"]
        self.assertEqual(variables["FACTORY_IMAGE_ORIGINS"], '["https://images.example.test"]')
        self.assertEqual(variables["FACTORY_AGENT_LINKS"], "{}")


    def test_render_with_jira_origin(self):
        profile = {**json.loads(VALID.read_text()), "jira_origin": "https://jira.example.test"}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "profile.json"
            path.write_text(json.dumps(profile))
            config = deploy.project_wrangler(deploy.load_profile(path), ROOT, Path(directory) / "out")
            variables = json.loads(Path(config).read_text())["vars"]
        self.assertEqual(variables["FACTORY_JIRA_ORIGIN"], "https://jira.example.test")

class PreflightTest(unittest.TestCase):
    def fake_run(self, calls, wrangler_dry_run=0, wrangler_deploy=0, inspect=0):
        def fake(argv, **kwargs):
            calls.append(argv)
            if argv == ["node", "--version"]:
                return subprocess.CompletedProcess(argv, 0, stdout="v24.14.0\n", stderr="")
            if "wrangler" in os.path.basename(str(argv[0])):
                if "--dry-run" in argv:
                    return subprocess.CompletedProcess(argv, wrangler_dry_run,
                        stdout="Total Upload: 1000.00 KiB / gzip: 200.00 KiB\n", stderr="")
                return subprocess.CompletedProcess(argv, wrangler_deploy, stdout="deployed\n", stderr="")
            if "inspectBundle" in argv[-1]:
                return subprocess.CompletedProcess(argv, inspect, stdout='{"limit": 1}\n', stderr="")
            return subprocess.CompletedProcess(argv, 0, stdout="", stderr="")
        return fake

    def test_deploy_preflight_sequence_with_fakes(self):
        calls = []
        with mock.patch.object(deploy.subprocess, "run", self.fake_run(calls)):
            self.assertEqual(deploy.main(["deploy", "--profile", str(VALID)]), 0)
        kinds = [("wrangler" if "wrangler" in os.path.basename(str(c[0])) else "node") for c in calls]
        self.assertEqual(kinds, ["node", "node", "wrangler", "node", "wrangler"],
                         "version, prepare, dry-run, inspect, deploy")
        dry_run, real = calls[2], calls[4]
        self.assertIn("--dry-run", dry_run)
        self.assertNotIn("--dry-run", real)
        self.assertEqual(real[1:], ["deploy", "--config", str(CONFIG)])
        self.assertTrue(str(dry_run[0]).endswith(os.path.join("node_modules", ".bin", "wrangler")))
        self.assertIn("--config", dry_run)
        self.assertIn(str(CONFIG), dry_run)

    def test_failed_dry_run_never_deploys(self):
        calls = []
        with mock.patch.object(deploy.subprocess, "run", self.fake_run(calls, wrangler_dry_run=1)):
            self.assertEqual(deploy.main(["deploy", "--profile", str(VALID)]), 1)
        self.assertEqual(len([c for c in calls if "wrangler" in os.path.basename(str(c[0]))]), 1)

    def test_failed_inspection_never_deploys(self):
        calls = []
        with mock.patch.object(deploy.subprocess, "run", self.fake_run(calls, inspect=1)):
            self.assertEqual(deploy.main(["deploy", "--profile", str(VALID)]), 1)
        self.assertEqual(len([c for c in calls if "wrangler" in os.path.basename(str(c[0]))]), 1)

    def test_dry_run_command_stops_after_inspection(self):
        calls = []
        with mock.patch.object(deploy.subprocess, "run", self.fake_run(calls)):
            self.assertEqual(deploy.main(["dry-run", "--profile", str(VALID)]), 0)
        kinds = [("wrangler" if "wrangler" in os.path.basename(str(c[0])) else "node") for c in calls]
        self.assertEqual(kinds, ["node", "node", "wrangler", "node"])

    def test_preflight_reports_missing_wrangler(self):
        with tempfile.TemporaryDirectory() as temp:
            self.assertEqual(deploy.node_tooling_problem(temp),
                             "locked Wrangler is not installed; run mise exec -- pnpm install --frozen-lockfile first")

    def test_preflight_rejects_old_node(self):
        old = subprocess.CompletedProcess(["node", "--version"], 0, stdout="v20.16.0\n", stderr="")
        with mock.patch.object(deploy.subprocess, "run", return_value=old):
            self.assertEqual(deploy.node_tooling_problem(ROOT),
                             "node v20.16.0 is older than package.json engines >=22.18.0; "
                             "run this command through mise exec --")

    def test_preflight_rejects_suffixed_old_node(self):
        old = subprocess.CompletedProcess(["node", "--version"], 0, stdout="v20.16.0-nightly\n", stderr="")
        with mock.patch.object(deploy.subprocess, "run", return_value=old):
            self.assertEqual(deploy.node_tooling_problem(ROOT),
                             "node v20.16.0-nightly is older than package.json engines >=22.18.0; "
                             "run this command through mise exec --")

    def test_preflight_reports_unreadable_node_version(self):
        empty = subprocess.CompletedProcess(["node", "--version"], 0, stdout="", stderr="")
        with mock.patch.object(deploy.subprocess, "run", return_value=empty):
            self.assertEqual(deploy.node_tooling_problem(ROOT),
                             'could not read node --version output ""; '
                             "run this command through mise exec --")

    def test_preflight_reports_node_stderr(self):
        broken = subprocess.CompletedProcess(
            ["node", "--version"], 127, stdout="",
            stderr="node: error while loading shared libraries: libatomic.so.1: cannot open shared object file\n")
        with mock.patch.object(deploy.subprocess, "run", return_value=broken):
            self.assertEqual(deploy.node_tooling_problem(ROOT),
                             "node --version failed: node: error while loading shared libraries: "
                             "libatomic.so.1: cannot open shared object file; "
                             "run this command through mise exec --")

    def test_preflight_rejects_trailing_version_text(self):
        trailing = subprocess.CompletedProcess(["node", "--version"], 0, stdout="v22.18.0x\n", stderr="")
        with mock.patch.object(deploy.subprocess, "run", return_value=trailing):
            self.assertEqual(deploy.node_tooling_problem(ROOT),
                             'could not read node --version output "v22.18.0x"; '
                             "run this command through mise exec --")

    def test_preflight_accepts_exact_node_version(self):
        exact = subprocess.CompletedProcess(["node", "--version"], 0, stdout="v22.18.0\n", stderr="")
        with mock.patch.object(deploy.subprocess, "run", return_value=exact):
            self.assertIsNone(deploy.node_tooling_problem(ROOT))

    def test_preflight_reports_missing_node(self):
        with mock.patch.object(deploy.subprocess, "run", side_effect=FileNotFoundError):
            self.assertEqual(deploy.node_tooling_problem(ROOT),
                             "node is not on PATH; run this command through mise exec --")


if __name__ == "__main__":
    unittest.main()
