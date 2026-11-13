import io
import json
import os
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

from datetime import datetime, timedelta, timezone

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
DEVICE = "host-a"
ROUTES = "---\nprimary: provider-a model-a high\nfallback: provider-b model-b\n---\n"
EXPERIMENT = "experiment: pilot provider-b model-b high 50\n"
# The PickupFixture fakes from test_experiment and test_pickup: a launcher that records its
# environment, and a stall adapter that lists the stalls in adapter.json.
FAKE = """import json, os, sys
path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "records.json")
records = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else []
records.append({k: v for k, v in os.environ.items() if k.startswith("FACTORY_")})
json.dump(records, open(path, "w", encoding="utf-8"))
"""
FAKE_ADAPTER = """import json, os, sys
root = os.path.dirname(os.path.abspath(__file__))
conf = json.load(open(os.path.join(root, "adapter.json"), encoding="utf-8"))
if sys.argv[1] == "list":
    print(json.dumps({"version": 1, "stalls": conf["stalls"]}), flush=True)
"""
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


def stamp(**delta):
    return (datetime.now(timezone.utc) + timedelta(**delta)).strftime("%Y-%m-%dT%H:%M:%SZ")


def idea(job_id):
    return {"id": job_id, "stage": "idea", "project": "alpha", "title": "Feature", "idea": "Feature",
            "updated": "2026-01-01T00:00:00Z"}


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

    def test_accepts_subagent_names(self):
        self.assertEqual(self.record("builder", "claude", "native", "claude-opus-5-5", "medium")[0], 0)
        self.assertEqual(self.record("factory-review-code", "claude", "native", "claude-opus-5-5")[0], 0)
        self.assertEqual([row["role"] for row in self.job()["agents"]], ["build", "review-code"])

    def test_refuses(self):
        roles = "orchestrator, spec, build, review-security, review-ux, review-ui, review-code, approve"
        for args, problem in (
                (("reviewer", "opencode", "native", "gpt-6.1-sol"), f'role "reviewer" is not one of {roles}'),
                (("build", "open code", "native", "gpt-6.1-sol"),
                 'harness "open code" must be 1 to 64 letters, digits, _ or -'),
                (("build", "h" * 65, "native", "gpt-6.1-sol"),
                 f'harness "{"h" * 65}" must be 1 to 64 letters, digits, _ or -'),
                (("build", "opencode", "subagent", "gpt-6.1-sol"),
                 'via "subagent" is not one of native, paseo, launcher, general, direct'),
                (("build", "opencode", "native", "gpt 6.1"),
                 'model "gpt 6.1" must be 1 to 200 letters, digits or _ . / : @ [ ] -'),
                (("build", "opencode", "native", "gpt-6.1-sol", "huge"),
                 'effort "huge" is not one of low, medium, high, xhigh, max')):
            self.assertEqual(self.record(*args), (2, "", USAGE[:-1] + ": " + problem + "\n"), args)
        code, out, err = self.record("build", "opencode", "native", "gpt-6.1-sol", session="other1")
        self.assertEqual((code, out), (6, ""))
        self.assertTrue(err.startswith(f"factory.py: {JOB} is held by {DEVICE} session d4e5f6 since "), err)
        self.assertTrue(err.endswith(". Nothing was written. Pass --session <tag> if this session holds it, "
                                     "or take it over with hold <project>/<id> take\n"), err)
        self.job().pop("claim")
        self.assertEqual(self.record("build", "opencode", "native", "gpt-6.1-sol"),
                         (6, "", f"factory.py: {JOB} has no claim from this session; run hold first\n"))
        self.assertEqual((self.puts, "agents" in self.job()), (0, False))

    def test_refuses_another_projects_job(self):
        result = invoke(factory, ["agent", "record", f"beta/{JOB}", "build", "opencode", "native", "gpt-6.1-sol",
                                  "--session", "d4e5f6"])
        self.assertEqual(result, (2, "", f"factory.py: {JOB} belongs to alpha, not beta\n"))
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

    def test_row_carries_paseo(self):
        with patch.object(factory, "paseo_session", return_value={"server": "srv-a", "agent": "agent-orch"}):
            code, out, err = self.record("review-code", "opencode", "native", "gpt-6.1-sol", "high")
        self.assertEqual((code, err), (0, ""))
        [row] = self.job()["agents"]
        self.assertEqual(row["paseo"], {"server": "srv-a", "agent": "agent-orch"})
        self.assertEqual(json.loads(out), row)


class AgentHoldLinkTest(BoardFixture):
    PASEO = {"server": "srv-a", "agent": "agent-orch"}

    def test_links_only_this_sessions_unlinked_rows(self):
        other = {"server": "srv-a", "agent": "agent-old"}
        job = {"agents": [dict(ROWS[0]), dict(ROWS[1], paseo=other), dict(ROWS[2]), "a hand-edited row"]}
        factory.link_agent_rows(job, "a1b2c3", self.PASEO)
        self.assertEqual(job["agents"][0]["paseo"], self.PASEO)
        self.assertEqual(job["agents"][1]["paseo"], other)
        self.assertNotIn("paseo", job["agents"][2])
        self.assertEqual(job["agents"][3], "a hand-edited row")
        factory.link_agent_rows({}, "a1b2c3", self.PASEO)

    def test_hold_links_the_launch_row(self):
        self.job()["agents"] = [dict(ROWS[2]), dict(ROWS[0])]
        patch.object(factory, "paseo_session", return_value=self.PASEO).start()
        patch.object(factory, "pull_job", return_value=("pulled", [])).start()
        patch.object(factory, "record_paseo_claim").start()
        code, out, err = invoke(factory, ["hold", f"alpha/{JOB}", "--session", "d4e5f6"])
        self.assertEqual((code, err), (0, ""))
        rows = self.job()["agents"]
        self.assertEqual(rows[0]["paseo"], self.PASEO)
        self.assertNotIn("paseo", rows[1])


class AgentReportTest(BoardFixture):
    def test_table(self):
        self.job()["agents"] = ROWS + ["a hand-edited row", None]
        self.board["jobs"].append({"id": "502-other", "stage": "build", "project": "beta", "agents": ROWS[3:4]})
        self.assertEqual(invoke(factory, ["agent", "report", "alpha"]), (0, TABLE, ""))
        code, out, err = invoke(factory, ["agent", "report"])
        self.assertEqual((code, err), (0, ""))
        self.assertIn("build   build         opencode  native  glm-5.3-flash    high    2\n", out)
        self.assertEqual(invoke(factory, ["agent", "report", "gamma"]), (0, "no agent records\n", ""))


class PickupFixture(BoardFixture):
    def setUp(self):
        super().setUp()
        self.root = self.home
        self.repo = self.root / "repo-a"
        self.repo.mkdir()
        (self.home / "paths.md").write_text(f"---\nalpha: {self.repo}\n---\n", encoding="utf-8")
        self.routes = self.home / "routes.md"
        self.routes.write_text(ROUTES, encoding="utf-8")
        self.fake = self.root / "fake_launcher.py"
        self.fake.write_text(FAKE, encoding="utf-8")
        self.write_launchers({"primary": [sys.executable, str(self.fake), "{effort}"],
                              "fallback": [sys.executable, str(self.fake)]})
        self.board["jobs"] = []
        patch.object(factory, "LAUNCH_WAIT_SECONDS", 60).start()
        patch.object(factory, "report_status").start()
        patch.object(factory, "backlog_triage_pass").start()
        patch.object(factory, "checkout_problem", return_value=None).start()

    def request(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if path == "/api/hosts":
            return 404, {}, ""
        if path == "/api/pipeline/effective/alpha":
            return 200, {}, json.dumps({
                "project": "alpha", "merge": "auto", "review_rounds": 2,
                "reviewers": ["security", "ux", "ui", "code"], "gates": [],
                "pickup": {"enabled": True, "hosts": [DEVICE]}, "stage_notes": {},
                "versions": {"global": 1, "project": 1}, "floors": []})
        return super().request(home, method, path, body, ok, headers, raw)

    def configure(self, line):
        self.routes.write_text(ROUTES[:4] + line + ROUTES[4:], encoding="utf-8")

    def write_launchers(self, launchers):
        (self.home / "launchers.json").write_text(json.dumps({"version": 1, "launchers": launchers}),
                                                  encoding="utf-8")

    def pickup(self):
        return invoke(factory, ["pickup"])

    def launched(self):
        path = self.root / "records.json"
        return json.loads(path.read_text(encoding="utf-8")) if path.exists() else []

    def job(self, job_id=JOB):
        return next(j for j in self.board["jobs"] if j["id"] == job_id)


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
             "model": "model-a", "effort": "high", "session": record["FACTORY_SESSION"]}])

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
        paseo.write_text(f"#!{sys.executable}\n" + FAKE, encoding="utf-8")
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
        self.board["jobs"] = [{"id": JOB, "stage": "build", "project": "alpha", "updated": stamp(hours=-25),
                               "claim": {"device": DEVICE, "session": "a1b2c3", "since": stamp(minutes=-40),
                                         "seen": stamp(minutes=-40)},
                               "paseo": {"server": "srv", "agent": "agent-1"}}]
        code, out, err = self.pickup()
        self.assertEqual((code, err), (0, ""))
        [record] = self.launched()
        self.assertEqual(self.rows(JOB), [
            {"stage": "build", "role": "orchestrator", "harness": "provider-b", "via": "launcher",
             "model": "model-b", "session": record["FACTORY_SESSION"]}])
