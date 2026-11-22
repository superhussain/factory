import contextlib
import io
import json
import os
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, load_runtime
from test_autonomy_report import VISIBLE, VISIBLE_CHANGES, merged

factory = load_runtime()
NOW = "2026-11-22T10:00:00Z"
PROMPT = "Type alpha visible to promote alpha visible so its APPROVE PRs merge without a person: "


def board(scanned=20, reverted_at="2026-10-23T10:00:00Z", rounds=(VISIBLE,), **autonomy):
    """alpha visible held by 1019-add-z, reverted at reverted_at, then scanned held visible jobs merged."""
    revert = merged("1019-add-z", [VISIBLE], "reverted", days_ago=31)
    revert["outcome"]["at"] = reverted_at
    data = {"updated": NOW, "jobs": [revert, *(merged(f"20{n:02}-v", list(rounds)) for n in range(scanned))]}
    if autonomy:
        data["autonomy"] = {"paused": True, "projects": [], "self": [], **autonomy}
    return data


class AutonomyPromoteTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        os.environ["FACTORY_HOME"] = str(self.home)

    def run_promote(self, *args, data=None, tty=True, typed="alpha visible"):
        routes = {"GET /projects.json": (200, [{"slug": "alpha"}]), "/api/board": (200, data or board())}
        with http_fixture(routes) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n", encoding="utf-8")
            with patch.object(factory.sys.stdin, "isatty", return_value=tty), \
                    patch("builtins.input", return_value=typed) as asked, \
                    patch.object(factory, "holder", return_value=("device", "session", 120)), \
                    patch.object(factory, "now", return_value=NOW), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as err:
                try:
                    factory.autonomy_cmd("promote", *args)
                    code = 0
                except SystemExit as e:
                    code = e.code
        puts = [json.loads(body) for method, _, _, body in server.requests if method == "PUT"]
        return code, err.getvalue(), asked, puts

    def assert_refused(self, data, why, code=3):
        got, err, asked, puts = self.run_promote("alpha", "visible", data=data)
        self.assertEqual((got, err, puts), (code, f"factory.py: {why}\n", []))
        asked.assert_not_called()

    def test_promote_writes_record(self):
        code, err, asked, puts = self.run_promote("alpha", "visible")
        self.assertEqual(code, 0)
        asked.assert_called_once_with(PROMPT)
        self.assertEqual(err, "alpha visible promoted. PRs already held keep their needs line until a person "
                              "merges them.\n")
        self.assertEqual(len(puts), 1)
        record = puts[0]["autonomy"]
        self.assertEqual(record["restored"], {"alpha/visible": NOW})
        self.assertEqual(record["promoted"], {"alpha/visible": {"at": NOW, "by": "device", "jobs": 20, "days": 30,
                                                                "until": "2026-12-06T10:00:00Z"}})
        self.assertEqual(factory.autonomy_holds(puts[0]), {})

    def test_gate_refusals(self):
        self.assert_refused(board(scanned=12),
                            "alpha visible cannot be promoted yet: needs 8 more scanned jobs; nothing was changed")
        self.assert_refused(board(reverted_at="2026-11-19T10:00:00Z"),
                            "alpha visible cannot be promoted yet: 3 days held, needs 7; nothing was changed")
        self.assert_refused(board(rounds=(VISIBLE_CHANGES, VISIBLE)),
                            "alpha visible cannot be promoted yet: rounds 2.0 over 1.75; changes 25% over 10%; "
                            "nothing was changed")

    def test_not_held(self):
        self.assert_refused(board(restored={"alpha/visible": "2026-11-01T10:00:00Z"}),
                            "alpha visible is not held, so there is nothing to promote; nothing was changed")

    def test_cooldown(self):
        until = {"at": "2026-11-15T10:00:00Z", "by": "device", "jobs": 20, "days": 7, "until": "2026-11-29T10:00:00Z"}
        self.assert_refused(board(promoted={"alpha/visible": until}),
                            "alpha visible cannot be promoted yet: cooldown until 2026-11-29T10:00:00Z; "
                            "nothing was changed")
        self.assert_refused(board(promoted={"alpha/visible": {"at": "2026-11-15T10:00:00Z", "until": "soon"}}),
                            "alpha visible cannot be promoted yet: promoted record for alpha/visible is malformed; "
                            "nothing was changed")
        expired = {**until, "until": "2026-11-20T10:00:00Z"}
        code, _, _, puts = self.run_promote("alpha", "visible", data=board(promoted={"alpha/visible": expired}))
        self.assertEqual((code, puts[0]["autonomy"]["promoted"]["alpha/visible"]["until"]),
                         (0, "2026-12-06T10:00:00Z"))

    def test_prompt_refusals(self):
        code, err, _, puts = self.run_promote("beta", "visible")
        self.assertEqual((code, err, puts), (4, "factory.py: no factory project beta\n", []))
        code, _, asked, puts = self.run_promote("alpha", "bogus")
        self.assertEqual((code, puts), (2, []))
        asked.assert_not_called()
        code, err, _, puts = self.run_promote("alpha", "visible", tty=False)
        self.assertEqual((code, err, puts),
                         (2, "factory.py: autonomy promote needs an interactive terminal; a human runs it\n", []))
        code, _, _, puts = self.run_promote("alpha", "visible", typed="alpha")
        self.assertEqual((code, puts), (2, []))
