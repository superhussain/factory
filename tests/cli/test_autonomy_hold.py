import contextlib
import io
import json
import os
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, load_runtime
from test_autonomy_report import VISIBLE, merged, stamp

factory = load_runtime()
PR = "https://github.com/example/alpha/pull/631"
NEEDS = ("review and merge PR #631: earned autonomy holds alpha visible PRs after reverted 1019-add-z; "
         "a person lifts it with factory.py autonomy restore alpha visible")
PROMPT = "Type alpha visible to let alpha visible APPROVE PRs merge without a person again: "


def shipping(job_id="1031-add-w", **extra):
    return {"id": job_id, "project": "alpha", "stage": "pr-open", "automerge": True, "pr": PR,
            "rounds": [VISIBLE], "updated": "2026-01-10T10:00:00Z", **extra}


def board(*jobs, restored=None):
    data = {"updated": "2026-01-10T10:00:00Z", "jobs": [merged("1019-add-z", [VISIBLE], "reverted", days_ago=10),
                                                         *jobs]}
    if restored is not None:
        data["autonomy"] = {"paused": True, "projects": [], "self": [], "restored": restored}
    return data


class AutonomyHoldTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        os.environ["FACTORY_HOME"] = str(self.home)

    def run_pass(self, data, dry=False):
        """Run the pass on data against a board server holding data. Returns stdout and the PUT bodies."""
        with http_fixture({"/api/board": (200, data)}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n", encoding="utf-8")
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                factory.autonomy_hold_pass(self.home, data, dry)
        return out.getvalue(), [json.loads(body) for method, _, _, body in server.requests if method == "PUT"]

    def test_pass_holds(self):
        data = board(shipping())
        out, puts = self.run_pass(json.loads(json.dumps(data)))
        self.assertEqual(out, "held 1031-add-w alpha PR #631 for a person\n")
        self.assertEqual(len(puts), 1)
        job = next(j for j in puts[0]["jobs"] if j["id"] == "1031-add-w")
        self.assertEqual((job["automerge"], job["needs"], job["note"]),
                         (False, NEEDS, "earned autonomy hold: a person reviews and merges"))
        self.assertEqual(factory.autonomy_holds(data), {("alpha", "visible"): ["1019-add-z"]})

    def test_untouched(self):
        cases = {
            "internal": shipping(rounds=[{"security": "PASS", "ux": "SKIPPED", "ui": "SKIPPED", "code": "PASS"}]),
            "escalated": shipping(escalated=True),
            "approve": shipping(stage="approve"),
            "automerge false": shipping(automerge=False),
        }
        for name, job in cases.items():
            with self.subTest(name):
                self.assertEqual(self.run_pass(board(job)), ("", []))
        with self.subTest("restored after the revert"):
            self.assertEqual(self.run_pass(board(shipping(), restored={"alpha/visible": "2026-01-10T00:00:00Z"})),
                             ("", []))

    def test_restore_lifts(self):
        self.assertEqual(factory.autonomy_holds(board(restored={"alpha/visible": "2026-01-10T00:00:00Z"})), {})
        # merged() records the revert at 2026-01-09T12:00:00Z, after this restore.
        self.assertEqual(factory.autonomy_holds(board(restored={"alpha/visible": "2026-01-08T00:00:00Z"})),
                         {("alpha", "visible"): ["1019-add-z"]})
        # A malformed entry is ignored, so the hold stays.
        for bad in ({"alpha/visible": "yesterday"}, {"alpha/bogus": stamp(0)}, ["alpha/visible"]):
            with self.subTest(bad):
                self.assertEqual(factory.autonomy_holds(board(restored=bad)), {("alpha", "visible"): ["1019-add-z"]})
        out, puts = self.run_pass(board(shipping(), restored={"alpha/visible": "2026-01-08T00:00:00Z"}))
        self.assertEqual((out, len(puts)), ("held 1031-add-w alpha PR #631 for a person\n", 1))

    def test_dry_run(self):
        self.assertEqual(self.run_pass(board(shipping()), dry=True),
                         ("would hold 1031-add-w alpha PR #631 for a person\n", []))

    def run_restore(self, *args, tty=True, typed="alpha visible", data=None):
        with http_fixture({"GET /projects.json": (200, [{"slug": "alpha"}]),
                           "/api/board": (200, data or board())}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n", encoding="utf-8")
            with patch.object(factory.sys.stdin, "isatty", return_value=tty), \
                    patch("builtins.input", return_value=typed) as asked, \
                    patch.object(factory, "holder", return_value=("device", "session", 120)), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as err:
                try:
                    factory.autonomy_cmd("restore", *args)
                    code = 0
                except SystemExit as e:
                    code = e.code
        puts = [json.loads(body) for method, _, _, body in server.requests if method == "PUT"]
        return code, err.getvalue(), asked, puts

    def test_restore_command(self):
        with patch.object(factory, "now", return_value="2026-11-20T10:00:00Z"):
            code, err, asked, puts = self.run_restore("alpha", "visible")
        self.assertEqual(code, 0)
        asked.assert_called_once_with(PROMPT)
        self.assertEqual(err, "alpha visible restored. PRs already held keep their needs line until a person "
                              "merges them.\n")
        self.assertEqual(len(puts), 1)
        record = puts[0]["autonomy"]
        self.assertEqual((record["restored"], record["changed"]), ({"alpha/visible": "2026-11-20T10:00:00Z"},
                                                                    "2026-11-20T10:00:00Z"))
        self.assertEqual(factory.autonomy_holds(puts[0]), {})

    def test_restore_refusals(self):
        code, err, _, puts = self.run_restore("beta", "visible")
        self.assertEqual((code, err, puts), (4, "factory.py: no factory project beta\n", []))
        code, _, asked, puts = self.run_restore("alpha", "bogus")
        self.assertEqual((code, puts), (2, []))
        asked.assert_not_called()
        code, err, _, puts = self.run_restore("alpha", "visible", tty=False)
        self.assertEqual((code, err, puts),
                         (2, "factory.py: autonomy restore needs an interactive terminal; a human runs it\n", []))
        code, _, _, puts = self.run_restore("alpha", "visible", typed="alpha")
        self.assertEqual((code, puts), (2, []))

    def test_restore_keeps_malformed_entries(self):
        kept = {"alpha/internal": "soon", "Bad Key": "2026-01-01T00:00:00Z"}
        with patch.object(factory, "now", return_value="2026-11-20T10:00:00Z"):
            code, _, _, puts = self.run_restore("alpha", "visible", data=board(restored=kept))
        self.assertEqual((code, len(puts)), (0, 1))
        self.assertEqual(puts[0]["autonomy"]["restored"], {**kept, "alpha/visible": "2026-11-20T10:00:00Z"})
        code, err, asked, puts = self.run_restore("alpha", "visible", data=board(restored="bad"))
        self.assertEqual((code, err, puts),
                         (3, "factory.py: autonomy.restored is not a map; nothing was changed\n", []))
        asked.assert_not_called()
