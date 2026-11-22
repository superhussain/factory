import contextlib
import io
import json
import unittest

from support import invoke
from test_pickup import TakeoverTest, factory, idea
import test_factory

HALT = {"halted": True, "reason": "Runaway launches on wsl", "by_kind": "host", "by": "wsl",
        "at": "2026-11-22T10:00:00Z"}
LINE = "factory stopped by wsl at 2026-11-22T10:00:00Z: Runaway launches on wsl"


class HaltPickupTest(TakeoverTest):
    def setUp(self):
        super().setUp()
        self.halt = HALT
        self.set_adapter([self.stall()])
        self.set_stalls()
        self.board["jobs"] = [self.stall_job(), idea("001-alpha", "alpha")]

    def test_pickup_launches_and_takes_over_nothing(self):
        self.assertEqual(self.pickup(), (0, "", f"factory.py: pickup launches nothing; {LINE}\n"))
        self.assertEqual(self.puts(), [])
        self.assertEqual(self.launched(), [])
        self.assertEqual(self.recorded(), [])

    def test_a_hidden_person_reads_as_a_person(self):
        self.halt = {"halted": True, "reason": "Runaway launches on wsl", "by_kind": "person",
                     "at": "2026-11-22T10:00:00Z"}
        self.assertIn("factory stopped by a person at", self.pickup()[2])

    def test_dry_run_skips_all(self):
        code, out, err = self.pickup("--dry-run")
        self.assertEqual((code, out, err), (0, f"skip all: {LINE}\n", ""))

    def test_not_halted_launches_as_before(self):
        self.halt = {"halted": False}
        code, out, err = self.pickup("--limit", "2", "--max-inflight", "2")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(len(self.launched()), 2)

    def test_an_unreadable_reply_launches_nothing(self):
        self.halt = "not json"
        code, _, err = self.pickup()
        self.assertEqual(code, 0)
        self.assertIn("pickup launches nothing; factory stopped by an unknown source at an unknown time", err)
        self.assertEqual(self.puts(), [])
        self.assertEqual(self.launched(), [])


class HaltCommandTest(test_factory.FactoryTest):
    def api(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if method == "PUT" and path == "/api/halt":
            self.put_paths.append(path)
            self.halt_puts.append((body, headers, ok))
            return 200, {}, json.dumps(dict(HALT, reason=json.loads(body)["reason"]))
        return super().api(home, method, path, body, ok, headers, raw)

    def setUp(self):
        super().setUp()
        self.halt_puts = []

    def test_stuck_exits_3_and_leaves_the_board(self):
        (self.home / "paths.md").write_text(f"---\nsample: {self.home / 'sample'}\n---\n")
        self.save(dict(self.job, claim=dict(self.job["claim"], seen="2020-01-01T00:00:00Z")))
        self.store["/api/halt"] = json.dumps(HALT).encode()
        before = self.store["/api/board"]
        err = io.StringIO()
        with contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as raised:
            test_factory.factory.stuck()
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(err.getvalue(), f"factory.py: stuck resumes nothing; {LINE}\n")
        self.assertEqual(self.store["/api/board"], before)

    def test_halt_set_sends_one_put_and_prints_the_record(self):
        code, out, err = invoke(test_factory.factory, ["halt", "set", "--reason", "Runaway launches on wsl"])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(self.halt_puts, [(json.dumps({"halted": True, "reason": "Runaway launches on wsl"}),
                                           {"Content-Type": "application/json"}, (200,))])
        self.assertEqual(json.loads(out), HALT)

    def test_halt_status_prints_the_record(self):
        self.store["/api/halt"] = json.dumps(HALT).encode()
        code, out, err = invoke(test_factory.factory, ["halt", "status"])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, json.dumps(HALT, indent=2) + "\n")

    def test_halt_status_without_the_route_exits_4(self):
        self.assertEqual(invoke(test_factory.factory, ["halt", "status"]),
                         (4, "", "factory.py: this Worker has no /api/halt route; deploy the Worker first\n"))

    def test_bad_reason_and_resume_exit_2(self):
        self.assertEqual(invoke(test_factory.factory, ["halt", "set", "--reason", ""])[0], 2)
        code, _, err = invoke(test_factory.factory, ["halt", "resume"])
        self.assertEqual(code, 2)
        self.assertIn("only a signed-in person resumes the factory, from the dashboard", err)
        self.assertEqual(self.halt_puts, [])


def load_tests(loader, tests, pattern):
    """Only the tests defined here, not the ones the fixtures' classes already run."""
    suite = unittest.TestSuite()
    for case in (HaltPickupTest, HaltCommandTest):
        suite.addTests(case(name) for name in sorted(vars(case)) if name.startswith("test_"))
    return suite
