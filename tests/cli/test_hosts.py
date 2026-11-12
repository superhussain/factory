import json
import os
from contextlib import nullcontext
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from support import SKILL, IsolatedTestCase, http_fixture, invoke, load_runtime, snapshot

factory = load_runtime()
HOST = "host-a"
CODE = "ABCDE-23456"
PAIR = f"POST /api/hosts/{HOST}/pair"
STATUS = f"PUT /api/hosts/{HOST}/status"
DOCTOR = {"/projects.json": (200, []), "/api/board": (200, {"jobs": []})}
CONFIG = "---\n# synthetic test home\napi_url: {origin}\n{device}claim_stale_minutes: 60\n---\nnotes stay\n"


def stamp(**delta):
    return (datetime.now(timezone.utc) + timedelta(**delta)).strftime("%Y-%m-%dT%H:%M:%SZ")


class HostFixture(IsolatedTestCase):
    def setUp(self):
        self.home = Path(os.environ["HOME"]).resolve() / "state"
        self.home.mkdir()
        self.config = self.home / "config.md"
        (self.home / ".env").write_text("FACTORY_ACCESS_CLIENT_ID=synthetic-id\n"
                                        "FACTORY_ACCESS_CLIENT_SECRET=synthetic-secret\n")
        (self.home / "routes.md").write_text("---\nprimary: provider-a model-a high\n---\n")
        self.addCleanup(patch.stopall)
        patch.object(factory.socket, "gethostname", return_value=HOST).start()

    def write_config(self, origin, device=f"device: {HOST}\n"):
        self.config.write_text(CONFIG.format(origin=origin, device=device))

    def run_cli(self, *args):
        code, out, err = invoke(factory, [*args, "--home", str(self.home)])
        self.assertNotIn(CODE, out + err)
        for rel, (_, data) in snapshot(self.home).items():
            self.assertNotIn(CODE.encode(), data or b"", rel)
            self.assertNotIn(CODE.replace("-", "").encode(), data or b"", rel)
        return code, out, err

    def keys(self, server):
        return [f"{method} {path}" for method, path, _, _ in server.requests]


class PairTest(HostFixture):
    def test_pair_sets_device_sends_one_post_and_one_status(self):
        for device in ("device:\n", ""):
            with self.subTest(device=device), http_fixture(
                    {PAIR: (200, b"paired"), STATUS: (200, b""), **DOCTOR}) as server:
                self.write_config(server.origin, device)
                code, out, err = self.run_cli("host", "pair", HOST, CODE)
                self.assertEqual((code, err), (0, ""))
                self.assertEqual(out, f"paired {HOST}. config.md now sets device: {HOST}. "
                                      "Confirm the host in Settings, Hosts.\n")
                expected = CONFIG.format(origin=server.origin, device=device)
                self.assertEqual(self.config.read_text(), expected.replace("device:\n", f"device: {HOST}\n")
                                 if device else expected.replace("\n---\nnotes", f"\ndevice: {HOST}\n---\nnotes"))
                self.assertEqual(self.keys(server), [PAIR, "GET /projects.json", "GET /api/board", STATUS])
                _, _, headers, body = server.requests[0]
                self.assertEqual(json.loads(body), {"code": CODE})
                self.assertEqual(headers["Content-Type"], "application/json")
                status = json.loads(server.requests[-1][3])
                self.assertEqual((status["scheduler"], status["last_pickup"], status["doctor"]),
                                 ("none", None, "ok"))
                self.assertFalse((self.home / "host-status.json").exists())

    def test_pair_reuses_cached_scheduler_and_last_pickup(self):
        last = stamp(minutes=-5)
        (self.home / "host-status.json").write_text(json.dumps({"scheduler": "launchd", "last_pickup": last}))
        with http_fixture({PAIR: (200, b"paired"), STATUS: (200, b""), **DOCTOR}) as server:
            self.write_config(server.origin)
            self.assertEqual(self.run_cli("host", "pair", HOST, CODE)[0], 0)
            status = json.loads(server.requests[-1][3])
        self.assertEqual((status["scheduler"], status["last_pickup"]), ("launchd", last))

    def test_bad_input_exits_2_before_any_request(self):
        cases = (("pair", "-host", CODE), ("pair", HOST, "ABCD"), ("pair", HOST, CODE + "X"),
                 ("bind", HOST, CODE), ("pair", "host-b", CODE))
        with http_fixture({PAIR: (200, b"paired"), STATUS: (200, b"")}) as server:
            self.write_config(server.origin)
            before = snapshot(self.home)
            for args in cases:
                with self.subTest(args=args):
                    code, out, err = self.run_cli("host", *args)
                    self.assertEqual((code, out), (2, ""))
                    self.assertEqual(snapshot(self.home), before)
            self.assertEqual(server.requests, [])
        self.assertIn("usage: host pair <id> <code>", self.run_cli("host", "bind", HOST, CODE)[2])
        self.assertIn(f"host id host-b does not match this device's name {HOST}. Board claims use {HOST}, "
                      f"so add the host in Settings as {HOST}.", self.run_cli("host", "pair", "host-b", CODE)[2])

    def test_refusals_leave_config_alone_and_send_no_status(self):
        cases = ((403, factory.PAIRING_FAILED.encode(), 6,
                  "pairing code is wrong, used or expired. Ask for a new code in Settings, Hosts."),
                 (409, b"this service token is already bound to another host", 6,
                  "this service token is already bound to another host. "
                  "Each host needs its own Access service token."),
                 (403, b"Forbidden", 5, "Access rejected the service token"),
                 (401, b"", 5, "Access rejected the service token"),
                 (500, b"boom", 5, "returned 500"),
                 (500, b"boom\x1b[2Jdone", 5, "returned 500: boom [2Jdone"))
        for status, body, exit_code, message in cases:
            with self.subTest(status=status, body=body), http_fixture(
                    {PAIR: (status, body), STATUS: (200, b""), **DOCTOR}) as server:
                self.write_config(server.origin)
                before = snapshot(self.home)
                code, out, err = self.run_cli("host", "pair", HOST, CODE)
                self.assertEqual((code, out), (exit_code, ""))
                self.assertIn(message, err)
                self.assertEqual(snapshot(self.home), before)
                self.assertEqual(self.keys(server), [PAIR])

    def test_device_calls_only_pair_and_status(self):
        source = (SKILL / "runtime/factory.py").read_text(encoding="utf-8")
        lines = [line for line in source.splitlines() if "/api/hosts" in line]
        self.assertTrue(lines)
        read = '    status, _, text = api(home, "GET", "/api/hosts", ok=(200, 404))'
        self.assertEqual(lines.count(read), 1)
        for line in lines:
            if line != read:
                self.assertRegex(line, r"/api/hosts/(\{\w+\}|<id>)/(pair|status)\b")


class SetDeviceTest(HostFixture):
    def test_replaces_or_inserts_and_keeps_every_other_byte(self):
        cases = (("---\r\ndevice: old\r\nx: 1\r\n---\r\nbody\r\n", "---\r\ndevice: host-a\r\nx: 1\r\n---\r\nbody\r\n"),
                 ("---\nx: 1\n---", "---\nx: 1\ndevice: host-a\n---"),
                 ("---\ndevice: a\ndevice: b\n---\n", "---\ndevice: host-a\ndevice: b\n---\n"))
        for before, after in cases:
            self.config.write_bytes(before.encode())
            factory.set_device(self.home, HOST)
            self.assertEqual(self.config.read_bytes(), after.encode())
        mtime = self.config.stat().st_mtime_ns
        factory.set_device(self.home, HOST)
        self.assertEqual(self.config.stat().st_mtime_ns, mtime)

    def test_missing_frontmatter_exits_2(self):
        for text in ("api_url: x\n", "---\napi_url: x\n"):
            self.config.write_text(text)
            with self.assertRaises(SystemExit) as error, patch("sys.stderr"):
                factory.set_device(self.home, HOST)
            self.assertEqual(error.exception.code, 2)
            self.assertEqual(self.config.read_text(), text)


class StatusTest(HostFixture):
    def test_body_has_the_seven_keys_and_follows_each_rule(self):
        (self.home / "routes.md").write_text(
            "---\nprimary: provider-a model-a high\nstale: provider-b model-b\n_hidden: provider-a model-a\n"
            f"long: provider-a {'m' * 129}\nlast: provider-c model-c low\n---\n")
        future = stamp(minutes=30)
        (self.home / "route-limits.json").write_text(json.dumps({"primary": future, "stale": stamp(minutes=-30)}))
        alpha = self.home / "alpha"
        alpha.mkdir()
        (self.home / "paths.md").write_text(f"---\nalpha: {alpha}\nbeta: {self.home / 'gone'}\n"
                                            f"zulu_1: {alpha}\nnot.slug: {alpha}\n---\n")
        report = {"checks": [{"name": "config", "status": "pass"}, {"name": "board", "status": "fail"}]}
        body = factory.host_status(self.home, report, "cron", "2026-01-01T00:00:00Z")
        self.assertEqual(list(body), ["os", "scheduler", "last_pickup", "bundle", "doctor", "routes", "checkouts"])
        self.assertIn(body["os"], ("macOS", "Windows", "Linux", "Linux (WSL)", "Other"))
        self.assertEqual((body["scheduler"], body["last_pickup"], body["bundle"]),
                         ("cron", "2026-01-01T00:00:00Z", None))
        self.assertEqual(body["doctor"], ["board"])
        self.assertEqual(body["routes"], [
            {"name": "primary", "provider": "provider-a", "model": "model-a", "effort": "high", "limited_until": future},
            {"name": "stale", "provider": "provider-b", "model": "model-b", "effort": None, "limited_until": None},
            {"name": "last", "provider": "provider-c", "model": "model-c", "effort": "low", "limited_until": None}])
        self.assertEqual(body["checkouts"], ["alpha", "zulu_1"])
        self.assertNotIn(str(self.home), json.dumps(body))
        report["checks"][1]["status"] = "pass"
        (self.home / "routes.md").unlink()
        body = factory.host_status(self.home, report, "none", None)
        self.assertEqual((body["doctor"], body["routes"]), ("ok", []))

    def test_os_names(self):
        for platform, release, name in (("darwin", "", "macOS"), ("win32", "", "Windows"),
                                        ("linux", "5.15.0-microsoft-standard-WSL2", "Linux (WSL)"),
                                        ("linux", "6.1.0", "Linux"), ("freebsd14", "", "Other")):
            with patch.object(factory.sys, "platform", platform), \
                    patch.object(factory.platform, "uname", return_value=SimpleNamespace(release=release)):
                self.assertEqual(factory.host_os(), name)

    def test_bundle_stamp_and_cached_status(self):
        runtime = self.home / "bundle" / "runtime"
        runtime.mkdir(parents=True)
        stamp_file = runtime.parent / "bundle.json"
        with patch.object(factory, "HERE", str(runtime)):
            self.assertIsNone(factory.bundle_stamp())
            good = {"commit": "a" * 40, "tree": "b" * 64}
            stamp_file.write_text(json.dumps(good))
            self.assertEqual(factory.bundle_stamp(), good)
            for bad in ({"commit": "a" * 40}, {"commit": "A" * 40, "tree": "b" * 64},
                        {"commit": "a" * 40, "tree": "b" * 65}, {**good, "extra": "c" * 7}, ["x"]):
                stamp_file.write_text(json.dumps(bad))
                self.assertIsNone(factory.bundle_stamp())
            stamp_file.write_text("not json")
            self.assertIsNone(factory.bundle_stamp())
        cache = self.home / "host-status.json"
        self.assertEqual(factory.cached_status(self.home), ("none", None, None))
        for doc, expected in (({"scheduler": "cron", "last_pickup": "2026-01-01T00:00:00Z"},
                               ("cron", "2026-01-01T00:00:00Z", None)),
                              ({"scheduler": "schtasks", "last_pickup": None}, ("schtasks", None, None)),
                              ({"scheduler": "systemd", "last_pickup": None}, ("none", None, None)),
                              ({"scheduler": "cron", "last_pickup": "2026-1-1T00:00:00Z"}, ("none", None, None)),
                              ({"scheduler": "cron", "last_pickup": None, "triage_failed_since": "2026-01-01T09:00:00Z"},
                               ("cron", None, "2026-01-01T09:00:00Z")),
                              ({"scheduler": "cron", "last_pickup": None, "triage_failed_since": "2026-1-1T09:00:00Z"},
                               ("cron", None, None)),
                              ({"scheduler": "cron"}, ("none", None, None)), (["cron"], ("none", None, None))):
            cache.write_text(json.dumps(doc))
            self.assertEqual(factory.cached_status(self.home), expected)

    def test_triage_failed_since_is_sent_only_while_set(self):
        report = {"checks": []}
        self.assertNotIn("triage_failed_since", factory.host_status(self.home, report, "cron", None))
        body = factory.host_status(self.home, report, "cron", None, "2026-01-01T09:00:00Z")
        self.assertEqual(list(body)[-1], "triage_failed_since")
        self.assertEqual(body["triage_failed_since"], "2026-01-01T09:00:00Z")
        with http_fixture({STATUS: (200, b"")}) as server:
            self.write_config(server.origin)
            self.assertTrue(factory.report_status(self.home, "cron", None, report=report))
            self.assertTrue(factory.report_status(self.home, "cron", None, "2026-01-01T09:00:00Z", report=report))
            sent = [json.loads(body) for _, _, _, body in server.requests]
        self.assertNotIn("triage_failed_since", sent[0])
        self.assertEqual(sent[1]["triage_failed_since"], "2026-01-01T09:00:00Z")


class PickupHeartbeatTest(HostFixture):
    def setUp(self):
        super().setUp()
        repo = self.home / "repo"
        repo.mkdir()
        (self.home / "paths.md").write_text(f"---\nalpha: {repo}\n---\n")

    def pickup(self, *args, heartbeat=True):
        if heartbeat:
            return self.run_cli("pickup", *args)
        with patch.object(factory, "report_status"):
            return self.run_cli("pickup", *args)

    def test_one_status_put_after_every_other_request(self):
        def launches(home, *args):
            factory.read_board(home)
            print("launched alpha-1 alpha on primary session abc")

        def fails(home, *args):
            factory.read_board(home)
            factory.fail("launch of alpha-1 on primary failed: exit 1; see log", 1)

        for name, side_effect, exit_code in (("nothing to do", None, 0), ("launches", launches, 0),
                                             ("fails", fails, 1)):
            with self.subTest(name), http_fixture({STATUS: (200, b""), **DOCTOR}) as server:
                self.write_config(server.origin)
                with patch.object(factory, "pickup_pass", side_effect=side_effect) if side_effect else nullcontext():
                    plain = self.pickup("--triage-now", heartbeat=False)
                    del server.requests[:]
                    beat = self.pickup("--triage-now")
                self.assertEqual(beat, plain)
                self.assertEqual(beat[0], exit_code)
                self.assertEqual(self.keys(server), ["GET /api/backlog", "GET /api/board", "GET /projects.json",
                                                     "GET /api/board", STATUS])
                body = json.loads(server.requests[-1][3])
                cache = json.loads((self.home / "host-status.json").read_text())
                self.assertEqual(cache, body)
                self.assertEqual(body["scheduler"], factory.PICKUP_SCHEDULER)
                self.assertRegex(body["last_pickup"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
                (self.home / "host-status.json").unlink()

    def test_dry_run_and_no_device_send_no_status(self):
        for args, device in ((("--dry-run",), f"device: {HOST}\n"), ((), "")):
            with self.subTest(args=args), http_fixture({STATUS: (200, b""), **DOCTOR}) as server:
                self.write_config(server.origin, device)
                self.assertEqual(self.pickup(*args), self.pickup(*args, heartbeat=False))
                self.assertEqual(set(self.keys(server)), {"GET /api/board"} | (set() if args else {"GET /api/backlog"}))
                self.assertFalse((self.home / "host-status.json").exists())

    def test_failed_status_put_changes_nothing_and_writes_no_cache(self):
        api = factory.api

        def offline(home, method, path, *args, **kwargs):
            if method == "PUT":
                factory.fail(f"can't reach {path}: connection refused", 5)
            return api(home, method, path, *args, **kwargs)

        for name, status in (("403", (403, b"status is written only by the host's bound service token")),
                             ("500", (500, b"boom")), ("network", (200, b""))):
            with self.subTest(name), http_fixture({STATUS: status, **DOCTOR}) as server:
                self.write_config(server.origin)
                with patch.object(factory, "api", side_effect=offline) if name == "network" else nullcontext():
                    beat = self.pickup()
                    self.assertEqual(self.keys(server)[-1], "GET /api/board" if name == "network" else STATUS)
                    self.assertEqual(beat, self.pickup(heartbeat=False))
                self.assertFalse((self.home / "host-status.json").exists())

    def test_a_failing_triage_sweep_is_reported_until_it_recovers(self):
        def failing(home):
            factory.fail("GET .../api/backlog returned 500: boom", 5)

        cache = self.home / "host-status.json"
        with http_fixture({STATUS: (200, b""), **DOCTOR}) as server:
            self.write_config(server.origin)

            def sent():
                self.assertEqual(self.keys(server)[-1], STATUS)
                return json.loads(server.requests[-1][3])

            with patch.object(factory, "backlog_triage_pass", side_effect=failing):
                cache.write_text(json.dumps({"scheduler": factory.PICKUP_SCHEDULER, "last_pickup": None,
                                             "triage_failed_since": "2026-01-01T09:00:00Z"}))
                code, _, err = self.pickup("--triage-now")
                self.assertEqual(code, 0)
                self.assertIn("backlog triage stopped", err)
                self.assertEqual(sent()["triage_failed_since"], "2026-01-01T09:00:00Z")
                self.assertEqual(json.loads(cache.read_text())["triage_failed_since"], "2026-01-01T09:00:00Z")
                cache.unlink()
                self.assertEqual(self.pickup("--triage-now")[0], 0)
                body = sent()
                self.assertEqual(body["triage_failed_since"], body["last_pickup"])
            self.run_cli("doctor")
            self.assertEqual(sent()["triage_failed_since"], body["last_pickup"])
            with patch.object(factory, "backlog_triage_pass", return_value=None):
                self.assertEqual(self.pickup("--triage-now")[0], 0)
            self.assertNotIn("triage_failed_since", sent())
            self.assertNotIn("triage_failed_since", json.loads(cache.read_text()))


class DoctorHeartbeatTest(HostFixture):
    def test_doctor_sends_one_status_after_its_two_gets_and_changes_nothing(self):
        with http_fixture({STATUS: (200, b""), **DOCTOR}) as server:
            self.write_config(server.origin)
            with patch.object(factory, "report_status"):
                plain = self.run_cli("doctor")
            del server.requests[:]
            before = snapshot(self.home)
            self.assertEqual(self.run_cli("doctor"), plain)
            self.assertEqual(snapshot(self.home), before)
            self.assertEqual(self.keys(server), ["GET /projects.json", "GET /api/board", STATUS])
            self.assertEqual(json.loads(server.requests[-1][3])["doctor"], "ok")
        self.assertEqual(plain[0], 0)

    def test_doctor_with_bad_config_sends_no_status(self):
        with http_fixture({STATUS: (200, b""), **DOCTOR}) as server:
            self.write_config(server.origin)
            (self.home / ".env").write_text("FACTORY_ACCESS_CLIENT_ID=synthetic-id\n")
            self.assertEqual(self.run_cli("doctor")[0], 2)
            self.assertEqual(server.requests, [])
