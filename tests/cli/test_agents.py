import io
import json
import os
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke
from test_experiment import EXPERIMENT, PickupFixture, factory, idea, stamp
from test_pickup import FAKE_ADAPTER

DEVICE = "host-a"
JOB = "501-retry-uploads"
USAGE = "factory.py: usage: agent record <project>/<id> <role> <harness> <via> <model> [<effort>]\n"
# The made-up step 3 rows: one job that started on Claude and moved to opencode during build.
ROWS = [
    {"at": "2026-01-01T09:00:00Z", "stage": "idea", "role": "orchestrator", "harness": "claude", "via": "paseo",
     "model": "claude-opus-5-5", "effort": "medium", "session": "a1b2c3"},
    {"at": "2026-01-01T09:04:00Z", "stage": "spec", "role": "spec", "harness": "claude", "via": "native",
     "model": "claude-opus-5-5", "effort": "medium", "session": "a1b2c3"},
    {"at": "2026-01-01T10:10:00Z", "stage": "build", "role": "orchestrator", "harness": "opencode",
     "via": "paseo", "model": "glm-5.3-flash", "effort": "high", "session": "d4e5f6"},
    {"at": "2026-01-01T10:12:00Z", "stage": "build", "role": "build", "harness": "opencode", "via": "native",
     "model": "glm-5.3-flash", "effort": "high", "session": "d4e5f6"},
    {"at": "2026-01-01T11:00:00Z", "stage": "review", "round": 1, "role": "review-code", "harness": "opencode",
     "via": "native", "model": "gpt-6.1-sol", "effort": "high", "session": "d4e5f6"},
]
TABLE = """stage   role          harness   via     model            effort  count
idea    orchestrator  claude    paseo   claude-opus-5-5  medium  1
spec    spec          claude    native  claude-opus-5-5  medium  1
build   build         opencode  native  glm-5.3-flash    high    1
build   orchestrator  opencode  paseo   glm-5.3-flash    high    1
review  review-code   opencode  native  gpt-6.1-sol      high    1
"""


class BoardFixture(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "config.md").write_text(f"---\ndevice: {DEVICE}\n---\n", encoding="utf-8")
        self.board = {"updated": "2026-01-01T00:00:00Z", "jobs": [
            {"id": JOB, "stage": "review", "round": 1, "project": "alpha", "updated": "2026-01-01T00:00:00Z",
             "claim": {"device": DEVICE, "session": "d4e5f6", "since": stamp(), "seen": stamp()}}]}
        self.puts = 0
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "api", side_effect=self.request).start()

    def request(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        self.assertEqual(path, "/api/board")
        if method == "GET":
            return 200, {"ETag": '"board"'}, factory.dump_board(self.board)
        self.puts += 1
        self.board = json.loads(body)
        return 200, {}, ""

    def job(self):
        return next(j for j in self.board["jobs"] if j["id"] == JOB)

    def record(self, *args, session="d4e5f6"):
        return invoke(factory, ["agent", "record", f"alpha/{JOB}", *args, "--session", session])


class AgentRecordTest(BoardFixture):
    def test_appends_row(self):
        code, out, err = self.record("review-code", "opencode", "native", "gpt-6.1-sol", "high")
        self.assertEqual((code, err), (0, ""))
        [row] = self.job()["agents"]
        self.assertEqual(json.loads(out), row)
        self.assertRegex(row.pop("at"), r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        self.assertEqual(row, {k: v for k, v in ROWS[4].items() if k != "at"})

    def test_refuses(self):
        for args in (("reviewer", "opencode", "native", "gpt-6.1-sol"),
                     ("build", "open code", "native", "gpt-6.1-sol"),
                     ("build", "opencode", "subagent", "gpt-6.1-sol"),
                     ("build", "opencode", "native", "gpt 6.1"),
                     ("build", "opencode", "native", "gpt-6.1-sol", "huge")):
            self.assertEqual(self.record(*args), (2, "", USAGE), args)
        code, out, err = self.record("build", "opencode", "native", "gpt-6.1-sol", session="other1")
        self.assertEqual((code, out), (6, ""))
        self.assertTrue(err.startswith(f"factory.py: {JOB} is held by {DEVICE} session d4e5f6 since "), err)
        self.job().pop("claim")
        self.assertEqual(self.record("build", "opencode", "native", "gpt-6.1-sol"),
                         (6, "", f"factory.py: {JOB} has no claim from this session; run hold first\n"))
        self.assertEqual((self.puts, "agents" in self.job()), (0, False))

    def test_board_refuses_agents(self):
        with patch.object(factory.sys, "stdin", io.StringIO('{"agents": []}')):
            result = invoke(factory, ["board", str(self.home), JOB])
        self.assertEqual(result, (2, "", "factory.py: only factory.py agent and pickup set agents\n"))
        self.assertEqual(self.puts, 0)

    def test_caps_rows(self):
        old = [dict(ROWS[1], at=f"2026-01-01T09:{i:02d}:00Z") for i in range(32)]
        self.job()["agents"] = old
        row = factory.record_agent(self.home, JOB, {"role": "build", "harness": "opencode", "via": "native",
                                                    "model": "glm-5.3-flash", "session": "d4e5f6"})
        rows = self.job()["agents"]
        self.assertEqual(len(rows), 32)
        self.assertEqual((rows[:31], rows[31]), (old[1:], row))
        self.assertNotIn("effort", row)
        self.assertIsNone(factory.record_agent(self.home, "999-missing", {"role": "build"}))


class AgentReportTest(BoardFixture):
    def test_table(self):
        self.job()["agents"] = ROWS
        self.board["jobs"].append({"id": "502-other", "stage": "build", "project": "beta", "agents": ROWS[3:4]})
        self.assertEqual(invoke(factory, ["agent", "report", "alpha"]), (0, TABLE, ""))
        code, out, err = invoke(factory, ["agent", "report"])
        self.assertEqual((code, err), (0, ""))
        self.assertIn("build   build         opencode  native  glm-5.3-flash    high    2\n", out)
        self.assertEqual(invoke(factory, ["agent", "report", "gamma"]), (0, "no agent records\n", ""))


class AgentPickupTest(PickupFixture):
    def rows(self, job_id):
        rows = self.job(job_id)["agents"]
        for row in rows:
            self.assertRegex(row.pop("at"), r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        return rows

    def test_launch_records_row(self):
        self.board["jobs"] = [idea("016-doc-lint")]
        self.assertEqual(self.pickup()[0], 0)
        [record] = self.launched()
        self.assertEqual(self.rows("016-doc-lint"), [
            {"stage": "idea", "role": "orchestrator", "harness": "provider-a", "via": "launcher",
             "model": "model-a", "effort": "high", "session": record["env"]["FACTORY_SESSION"]}])

    def test_launch_records_the_arm_b_triple(self):
        self.configure(EXPERIMENT)
        self.board["jobs"] = [idea("014-inline-images")]
        self.assertEqual(self.pickup()[0], 0)
        [row] = self.rows("014-inline-images")
        self.assertEqual((row["harness"], row["model"], row["effort"]), ("provider-b", "model-b", "high"))

    def test_paseo_launcher_records_paseo(self):
        if os.name == "nt":
            self.skipTest("the fake paseo program is a shebang script")
        paseo = self.root / "paseo"
        paseo.write_text(f"#!{sys.executable}\n" + (self.root / "fake_launcher.py").read_text(encoding="utf-8"),
                         encoding="utf-8")
        paseo.chmod(0o755)
        self.write_launchers({"primary": [str(paseo), "{id}"], "fallback": [str(paseo), "{id}"]})
        self.board["jobs"] = [idea("016-doc-lint")]
        self.assertEqual(self.pickup()[0], 0)
        [row] = self.rows("016-doc-lint")
        self.assertEqual(row["via"], "paseo")

    def test_takeover_records_row(self):
        adapter = self.root / "fake_adapter.py"
        adapter.write_text(FAKE_ADAPTER, encoding="utf-8")
        (self.root / "adapter.json").write_text(json.dumps({"stalls": [
            {"session": "agent-1", "reset": stamp(hours=3), "provider": "provider-a", "model": "model-a"}]}),
            encoding="utf-8")
        (self.home / "stalls.json").write_text(json.dumps({"version": 1, "adapter": [sys.executable, str(adapter)]}),
                                               encoding="utf-8")
        self.write_launchers({"primary": [sys.executable, str(self.fake), "{id}"],
                              "fallback": [sys.executable, str(self.fake), "{id}"]})
        self.board["jobs"] = [{"id": JOB, "stage": "build", "project": "alpha", "updated": stamp(hours=-25),
                               "claim": {"device": DEVICE, "session": "a1b2c3", "since": stamp(minutes=-40),
                                         "seen": stamp(minutes=-40)},
                               "paseo": {"server": "srv", "agent": "agent-1"}}]
        code, out, err = self.pickup()
        self.assertEqual((code, err), (0, ""))
        [record] = self.launched()
        self.assertEqual(self.rows(JOB), [
            {"stage": "build", "role": "orchestrator", "harness": "provider-b", "via": "launcher",
             "model": "model-b", "session": record["env"]["FACTORY_SESSION"]}])
