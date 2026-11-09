import json
import os
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime, snapshot


class DoctorTest(IsolatedTestCase):
    def setUp(self):
        self.runtime = load_runtime()
        self.home = Path(os.environ["HOME"]).resolve() / "state"
        self.home.mkdir()
        self.config = self.home / "config.md"
        self.env = self.home / ".env"
        self.config.write_text("---\napi_url: https://example.invalid\n---\n")
        self.env.write_text("FACTORY_ACCESS_CLIENT_ID=synthetic-private-id\nFACTORY_ACCESS_CLIENT_SECRET=opaque.private-secret\n")

    def run_doctor(self, expected):
        before = snapshot(self.home)
        with patch.object(self.runtime, "install_agents") as roles, patch.object(self.runtime, "git") as git:
            code, out, err = invoke(self.runtime, ["doctor", "--home", str(self.home)])
        self.assertEqual(code, expected)
        self.assertEqual(err, "")
        self.assertNotIn("synthetic-private-id", out)
        self.assertNotIn("opaque.private-secret", out)
        report = json.loads(out)
        self.assertEqual(set(report), {"version", "home", "api_url", "checks"})
        self.assertEqual(report["version"], 1)
        self.assertEqual(report["home"], str(self.home))
        self.assertEqual([c["name"] for c in report["checks"]], ["location", "config", "credentials", "projects", "board"])
        for check in report["checks"]:
            self.assertEqual(set(check), {"name", "status", "message"})
            self.assertIn(check["status"], ("pass", "fail"))
        self.assertEqual(snapshot(self.home), before)
        roles.assert_not_called()
        git.assert_not_called()
        return report

    def test_success_and_absent_board_are_two_read_only_requests(self):
        for status in (200, 404):
            with http_fixture({"/projects.json": (200, []), "/api/board": (status, {"jobs": []})}) as server:
                self.config.write_text("---\napi_url: " + server.origin + "/\n---\n")
                report = self.run_doctor(0)
                self.assertEqual(report["api_url"], server.origin)
                self.assertEqual([r[:2] for r in server.requests], [("GET", "/projects.json"), ("GET", "/api/board")])
                for method, path, headers, body in server.requests:
                    self.assertEqual(headers["Cf-Access-Client-Id"], "synthetic-private-id")
                    self.assertEqual(headers["Cf-Access-Client-Secret"], "opaque.private-secret")
                    self.assertEqual(headers["User-Agent"], "factory")
                    self.assertEqual(body, b"")
                    self.assertNotIn("private", path)
                self.assertEqual(report["checks"][-1]["message"], "Board is reachable." if status == 200 else "Board is absent; no board was created.")

    def test_missing_or_invalid_local_configuration_short_circuits(self):
        for endpoint in ("", "http://example.invalid", "https://example.invalid/path"):
            self.config.write_text("---\napi_url: " + endpoint + "\n---\n")
            with patch.object(self.runtime, "api") as api:
                report = self.run_doctor(2)
            api.assert_not_called()
            self.assertEqual(report["checks"][1]["status"], "fail")
            self.assertEqual(report["checks"][-1]["message"], "Not attempted because local configuration is invalid.")
        self.config.write_text("---\napi_url: https://example.invalid\n---\n")
        self.env.write_text("FACTORY_ACCESS_CLIENT_ID=synthetic-private-id\n")
        with patch.object(self.runtime, "api") as api:
            report = self.run_doctor(2)
        api.assert_not_called()
        self.assertEqual(report["checks"][2]["message"], self.runtime.token_message(self.env))

    def test_invalid_metadata_and_pointer_are_sanitized(self):
        self.config.write_text("not closed synthetic-private-id opaque.private-secret")
        with patch.object(self.runtime, "api") as api:
            report = self.run_doctor(2)
        api.assert_not_called()
        self.assertEqual(report["checks"][1]["message"], "Invalid local metadata: config.md.")
        root = Path(os.environ["XDG_CONFIG_HOME"])
        root.mkdir()
        (root / "factory-location.json").write_text("opaque.private-secret")
        code, out, err = invoke(self.runtime, ["doctor"])
        self.assertEqual((code, err), (2, ""))
        report = json.loads(out)
        self.assertEqual((report["home"], report["api_url"]), ("", ""))
        self.assertEqual(report["checks"], [{"name": "location", "status": "fail", "message": "Invalid factory location."}])
        self.assertNotIn("private-secret", out)

    def test_redirect_auth_and_malformed_responses_do_not_leak_or_follow(self):
        for status, body in ((302, b"opaque.private-secret"), (401, b"synthetic-private-id"),
                             (403, b"opaque.private-secret"), (200, b"not json"), (200, {})):
            with http_fixture({"/projects.json": (status, body), "/api/board": (200, {"jobs": []})}) as server:
                self.config.write_text("---\napi_url: " + server.origin + "\n---\n")
                report = self.run_doctor(5)
                self.assertEqual(report["checks"][3]["status"], "fail")
                self.assertEqual(report["checks"][4]["status"], "pass")
                self.assertEqual(len(server.requests), 2)
        with http_fixture({"/projects.json": (200, []), "/api/board": (200, {"jobs": {}})}) as server:
            self.config.write_text("---\napi_url: " + server.origin + "\n---\n")
            self.assertEqual(self.run_doctor(5)["checks"][-1]["status"], "fail")

    def test_timeout_uses_30_seconds_and_sanitizes_transport_diagnostics(self):
        with patch.object(self.runtime.urllib.request, "build_opener") as opener:
            opener.return_value.open.side_effect = OSError("opaque.private-secret synthetic-private-id")
            report = self.run_doctor(5)
        self.assertEqual(opener.return_value.open.call_count, 2)
        for call in opener.return_value.open.call_args_list:
            self.assertEqual(call.kwargs["timeout"], 30)
        self.assertTrue(all(c["status"] == "fail" for c in report["checks"][-2:]))
