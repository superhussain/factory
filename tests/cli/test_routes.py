import contextlib
import io
import json
import os
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, load_runtime

factory = load_runtime()
ROUTES = "---\nalpha: provider-a model-a\nbeta: provider-b vendor/model-b[1m]\n---\n\nThis host's routes.\n"


def stamp(minutes):
    return (datetime.now(timezone.utc) + timedelta(minutes=minutes)).strftime("%Y-%m-%dT%H:%M:%SZ")


class RoutesTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)
        self.routes = self.home / "routes.md"
        self.limits = self.home / "route-limits.json"
        self.routes.write_text(ROUTES, encoding="utf-8")
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "api", side_effect=AssertionError("routes must not call the board")).start()
        patch.object(factory.subprocess, "run", side_effect=AssertionError("routes must not run a process")).start()
        self.output, self.errors = io.StringIO(), io.StringIO()
        for redirect in (contextlib.redirect_stdout(self.output), contextlib.redirect_stderr(self.errors)):
            redirect.__enter__()
            self.addCleanup(redirect.__exit__, None, None, None)

    def run_routes(self, *args):
        factory.main(["routes", *args])
        return json.loads(self.output.getvalue())

    def exits(self, code, *args):
        with self.assertRaises(SystemExit) as raised:
            factory.main(["routes", *args])
        self.assertEqual(raised.exception.code, code)
        self.assertEqual(self.output.getvalue(), "")

    def write_limits(self, limits):
        self.limits.write_text(json.dumps(limits), encoding="utf-8")

    def test_pick_first(self):
        self.assertEqual(self.run_routes("pick"), {"route": "alpha", "provider": "provider-a", "model": "model-a"})
        self.assertFalse(self.limits.exists())

    def test_pick_skips_limited(self):
        self.write_limits({"alpha": stamp(30)})
        self.assertEqual(self.run_routes("pick"),
                         {"route": "beta", "provider": "provider-b", "model": "vendor/model-b[1m]"})

    def test_pick_ignores_expired(self):
        self.write_limits({"alpha": stamp(-1)})
        self.assertEqual(self.run_routes("pick")["route"], "alpha")

    def test_pick_all_limited(self):
        alpha, beta = stamp(30), stamp(90)
        self.write_limits({"alpha": alpha, "beta": beta})
        self.exits(3, "pick")
        self.assertEqual(self.errors.getvalue(),
                         f"factory.py: no available route: alpha until {alpha}, beta until {beta}\n")

    def test_routes_file_errors(self):
        cases = {"": "no routes", "---\n---\n": "no routes", "alpha: provider-a model-a\n": "no routes",
                 "---\nalpha: provider-a\n---\n": "bad route 'alpha'",
                 "---\nalpha: provider-a model-a extra\n---\n": "bad route 'alpha'",
                 "---\nalpha: provider/a model-a\n---\n": "bad route 'alpha'",
                 "---\nalpha: provider-a model;rm\n---\n": "bad route 'alpha'",
                 "---\nal pha: provider-a model-a\n---\n": "bad route 'al pha'"}
        for text, message in [(None, "no routes")] + list(cases.items()):
            with self.subTest(text=text):
                if text is None:
                    self.routes.unlink()
                else:
                    self.routes.write_text(text, encoding="utf-8")
                for args in (("pick",), ("list",), ("limit", "alpha"), ("clear", "alpha")):
                    self.errors.truncate(0), self.errors.seek(0)
                    self.exits(2, *args)
                    self.assertIn(message, self.errors.getvalue())
                    self.assertFalse(self.limits.exists())

    def test_limits_file_corrupt(self):
        for text in ("{", "[]", '{"alpha": 1}', '{"alpha": "soon"}'):
            with self.subTest(text=text):
                self.limits.write_text(text, encoding="utf-8")
                for args in (("pick",), ("list",), ("limit", "alpha"), ("clear", "alpha")):
                    self.errors.truncate(0), self.errors.seek(0)
                    self.exits(2, *args)
                    self.assertIn("delete it to clear every route limit", self.errors.getvalue())
                    self.assertEqual(self.limits.read_text(encoding="utf-8"), text)

    def test_limit_writes_and_prunes(self):
        self.write_limits({"beta": stamp(-5)})
        before = datetime.now(timezone.utc).replace(microsecond=0)
        printed = self.run_routes("limit", "alpha")
        after = datetime.now(timezone.utc)
        written = json.loads(self.limits.read_text(encoding="utf-8"))
        self.assertEqual(list(written), ["alpha"])
        self.assertEqual(printed, {"route": "alpha", "limited_until": written["alpha"]})
        until = datetime.strptime(written["alpha"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
        self.assertTrue(before + timedelta(minutes=60) <= until <= after + timedelta(minutes=60))
        self.output.truncate(0), self.output.seek(0)
        printed = self.run_routes("limit", "beta", "--minutes", "1440")
        written = json.loads(self.limits.read_text(encoding="utf-8"))
        self.assertEqual(sorted(written), ["alpha", "beta"])
        until = datetime.strptime(written["beta"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
        self.assertGreater(until, after + timedelta(minutes=1439))

    def test_limit_rejects(self):
        for args in (("gamma",), ("alpha", "--minutes", "0"), ("alpha", "--minutes", "abc"),
                     ("alpha", "--minutes", "1441"), ("alpha", "--minutes"), ("alpha", "--hours", "1")):
            with self.subTest(args=args):
                self.errors.truncate(0), self.errors.seek(0)
                self.exits(2, "limit", *args)
                self.assertFalse(self.limits.exists())
        self.assertIn("unknown route 'gamma'", self.exit_message("limit", "gamma"))
        self.assertIn("with N from 1 to 1440", self.exit_message("limit", "alpha", "--minutes", "0"))

    def exit_message(self, *args):
        self.errors.truncate(0), self.errors.seek(0)
        self.exits(2, *args)
        return self.errors.getvalue()

    def test_clear(self):
        beta = stamp(30)
        self.write_limits({"alpha": stamp(30), "beta": beta})
        factory.main(["routes", "clear", "alpha"])
        self.assertEqual(self.output.getvalue(), "")
        self.assertEqual(json.loads(self.limits.read_text(encoding="utf-8")), {"beta": beta})
        self.limits.unlink()
        factory.main(["routes", "clear", "alpha"])
        self.assertFalse(self.limits.exists())
        self.assertIn("unknown route 'gamma'", self.exit_message("clear", "gamma"))
        self.assertFalse(self.limits.exists())

    def test_list(self):
        alpha = stamp(30)
        self.write_limits({"alpha": alpha, "beta": stamp(-1)})
        self.assertEqual(self.run_routes("list"), [
            {"route": "alpha", "provider": "provider-a", "model": "model-a", "limited_until": alpha},
            {"route": "beta", "provider": "provider-b", "model": "vendor/model-b[1m]", "limited_until": None}])

    def test_effort(self):
        self.routes.write_text("---\nalpha: provider-a model-a high\nbeta: provider-b model-b\n---\n",
                               encoding="utf-8")
        self.assertEqual(self.run_routes("pick"),
                         {"route": "alpha", "provider": "provider-a", "model": "model-a", "effort": "high"})
        self.output.truncate(0), self.output.seek(0)
        self.assertEqual(self.run_routes("list"), [
            {"route": "alpha", "provider": "provider-a", "model": "model-a", "effort": "high",
             "limited_until": None},
            {"route": "beta", "provider": "provider-b", "model": "model-b", "limited_until": None}])
        self.output.truncate(0), self.output.seek(0)
        self.write_limits({"alpha": stamp(30)})
        self.assertEqual(self.run_routes("pick"), {"route": "beta", "provider": "provider-b", "model": "model-b"})
        self.output.truncate(0), self.output.seek(0)
        for effort in factory.ROUTE_EFFORTS:
            self.routes.write_text(f"---\nalpha: provider-a model-a {effort}\n---\n", encoding="utf-8")
            self.assertEqual(factory.read_routes(self.home), [("alpha", "provider-a", "model-a", effort)])
        for text in ("---\nalpha: provider-a model-a extra\n---\n", "---\nalpha: provider-a model-a High\n---\n",
                     "---\nalpha: provider-a model-a high extra\n---\n"):
            with self.subTest(text=text):
                self.routes.write_text(text, encoding="utf-8")
                self.assertIn("bad route 'alpha'", self.exit_message("pick"))
                self.assertIn("use <name>: <provider> <model> [<effort>]", self.exit_message("list"))

    def test_usage(self):
        for args in (("probe",), ("pick", "extra"), ("list", "extra"), ("clear",), ("clear", "a", "b")):
            with self.subTest(args=args):
                self.assertIn("usage: routes pick|list", self.exit_message(*args))


class UnclaimTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)
        self.claim = {"device": "device", "session": "session", "since": "2026-01-01T00:00:00Z",
                      "seen": "2026-01-01T00:00:00Z"}
        self.board = {"updated": "2026-01-01T00:00:00Z", "jobs": [
            {"id": "001-ready", "stage": "idea", "project": "sample", "idea": "ready",
             "origin": {"gate": "human"}, "needs": "", "updated": "2026-01-01T00:00:00Z", "claim": self.claim},
        ]}
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "holder", return_value=("device", "session", 120)).start()
        patch.object(factory.time, "sleep").start()
        patch.dict(os.environ, {"FACTORY_SESSION": "session"}).start()
        self.api = patch.object(factory, "api", side_effect=self.request).start()
        self.output = io.StringIO()
        redirect = contextlib.redirect_stdout(self.output)
        redirect.__enter__()
        self.addCleanup(redirect.__exit__, None, None, None)

    def request(self, home, method, path, body=None, ok=(200,), headers=None):
        self.assertEqual(path, "/api/board")
        if method == "GET":
            return 200, {"ETag": '"fixture"'}, factory.dump_board(self.board)
        self.assertEqual(headers, {"If-Match": '"fixture"'})
        self.board = json.loads(body)
        return 200, {}, ""

    def puts(self):
        return [c for c in self.api.call_args_list if c.args[1] == "PUT"]

    def fails(self, code, job_id="001-ready"):
        before = json.loads(json.dumps(self.board))
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            factory.unclaim(job_id)
        self.assertEqual(raised.exception.code, code)
        self.assertEqual(self.puts(), [])
        self.assertEqual(self.board, before)

    def test_unclaim_releases_only_the_claim(self):
        expected = json.loads(json.dumps(self.board["jobs"][0]))
        factory.unclaim("001-ready")
        job = self.board["jobs"][0]
        self.assertEqual(self.output.getvalue(), "")
        self.assertEqual(len(self.puts()), 1)
        self.assertNotIn("claim", job)
        self.assertNotEqual(job["updated"], expected["updated"])
        expected.pop("claim"), expected.pop("updated"), job.pop("updated")
        self.assertEqual(job, expected)

    def test_unclaim_other_holder(self):
        self.board["jobs"][0]["claim"] = dict(self.claim, session="other")
        self.fails(6)
        self.board["jobs"][0]["claim"] = dict(self.claim, device="other")
        self.fails(6)
        del self.board["jobs"][0]["claim"]
        self.fails(6)

    def test_unclaim_not_idea(self):
        self.board["jobs"][0]["stage"] = "spec"
        self.fails(6)

    def test_unclaim_unknown_job(self):
        self.fails(4, "002-missing")

    def test_unclaim_no_session(self):
        os.environ.pop("FACTORY_SESSION")
        self.fails(2)
        self.assertEqual(self.api.call_args_list, [])

    def test_unclaim_bad_id(self):
        for job_id in ("../x", "a..b", "-x"):
            with self.subTest(job_id=job_id):
                self.fails(2, job_id)
        self.assertEqual(self.api.call_args_list, [])

    def test_unclaim_via_main_session_flag(self):
        os.environ.pop("FACTORY_SESSION")
        factory.main(["unclaim", "001-ready", "--session", "session"])
        self.assertNotIn("claim", self.board["jobs"][0])
