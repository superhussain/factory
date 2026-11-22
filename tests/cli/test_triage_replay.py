import json
import os
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

USAGE = "factory.py: usage: triage replay <project-slug> [--threshold N] [--rules-file PATH]\n"
AT = "2026-10-27T12:00:00Z"
MODEL = "@cf/cloudflare/clef-flash"


def job(n, line, decision, at=AT, **backlog):
    return {"id": f"{n:03d}-item", "project": "sample", "stage": "idea",
            "backlog": {"line": f"- [ ] {line}", "decision": decision, "at": at, **backlog}}


def hold(confidence, **extra):
    return {"decision": "hold", "confidence": confidence, "threshold": 90,
            "reason": "Rated by clef-flash against the project rules.", "model": MODEL, **extra}


EXAMPLE_JOBS = [
    job(1, "sample: Add CSV export to reports", "approved", decider="person"),
    job(2, "sample: Add a dark mode toggle", "approved", decider="person"),
    job(3, "sample: Rotate the deploy token", "denied", decider="person"),
    job(4, "sample: Pick export scope", "approved", decider="person"),
]
EXAMPLE_STORED = {
    "sample: Add CSV export to reports": hold(86, decider="model"),
    "sample: Add a dark mode toggle": hold(81, decider="model"),
    "sample: Rotate the deploy token": hold(72, decider="model"),
    "sample: Pick export scope": hold(64, reason="it asks questions a person must answer", decider="rule"),
}
EXAMPLE_OUT = {
    "project": "sample", "labels": 4, "scored": 3,
    "unscored": {"rule": 1, "no_verdict": 0}, "model_queued": 0,
    "thresholds": [
        {"threshold": 90, "current": True, "queue_approved": 0, "queue_denied": 0, "hold_approved": 2,
         "hold_denied": 1, "agreement": 33},
        {"threshold": 80, "current": False, "queue_approved": 2, "queue_denied": 0, "hold_approved": 0,
         "hold_denied": 1, "agreement": 100}]}
RULES = "Pick up small export and settings features. Hold anything that touches deploy.\n"
EXAMPLE_RATED = {
    "sample: Add CSV export to reports": 93,
    "sample: Add a dark mode toggle": 88,
    "sample: Rotate the deploy token": 41,
}
RULES_OUT = {
    "project": "sample", "rules_file": "rules.txt", "labels": 4, "scored": 3,
    "unscored": {"rule": 1, "no_verdict": 0, "rating_failed": 0}, "model_queued": 0,
    "thresholds": [
        {"threshold": 90, "current": True, "queue_approved": 1, "queue_denied": 0, "hold_approved": 1,
         "hold_denied": 1, "agreement": 67},
        {"threshold": 80, "current": False, "queue_approved": 2, "queue_denied": 0, "hold_approved": 0,
         "hold_denied": 1, "agreement": 100}]}


class TriageReplayTests(IsolatedTestCase):
    def setUp(self):
        self.module = load_runtime()
        self.board = {"updated": "", "jobs": list(EXAMPLE_JOBS)}
        self.archive = [[]]
        self.archive_status = 200
        self.stored = dict(EXAMPLE_STORED)
        self.view = (200, {"triage": {"enabled": True, "threshold": 90, "rules": ""}})
        self.calls = []
        self.rated = dict(EXAMPLE_RATED)
        self.rate_bodies = []

        def api(home, method, path, body=None, ok=(200,), headers=None, raw=False):
            self.calls.append((method, path))
            if (method, path) == ("GET", "/projects.json"):
                return 200, {}, json.dumps([{"slug": "sample"}])
            if (method, path) == ("GET", "/api/board"):
                return 200, {}, json.dumps(self.board)
            if method == "GET" and path.startswith("/api/board/archive?page="):
                self.assertEqual(ok, (200, 404))
                if self.archive_status == 404:
                    return 404, {}, "not found"
                n = int(path.rsplit("=", 1)[1])
                return 200, {}, json.dumps({"jobs": self.archive[n - 1], "page": n, "pages": len(self.archive),
                                            "total": sum(map(len, self.archive))})
            if (method, path) == ("GET", "/api/pipeline/effective/sample"):
                status, view = self.view
                return status, {}, json.dumps(view)
            if (method, path) == ("POST", "/api/backlog/triage/rate"):
                self.assertEqual(ok, (200,))
                payload = json.loads(body)
                self.rate_bodies.append(payload)
                return 200, {}, json.dumps({"model": MODEL,
                                            "confidences": [self.rated.get(i) for i in payload["items"]]})
            self.assertEqual((method, path), ("POST", "/api/backlog/triage/verdicts"))
            items = json.loads(body)["items"]
            self.assertLessEqual(len(items), 50)
            return 200, {}, json.dumps({"verdicts": [self.stored.get(item) for item in items]})

        for target, fake in (("factory_home", lambda: "/nonexistent-home"), ("api", api)):
            patcher = patch.object(self.module, target, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def rules_file(self, text=RULES):
        self.addCleanup(os.chdir, os.getcwd())
        os.chdir(os.environ["HOME"])
        with open("rules.txt", "w", encoding="utf-8") as f:
            f.write(text)

    def replay(self, *args):
        code, out, err = invoke(self.module, ["triage", "replay", *args])
        return code, json.loads(out) if code == 0 else out, err

    def test_example_prints_the_spec_json_and_only_reads(self):
        code, out, err = invoke(self.module, ["triage", "replay", "sample", "--threshold", "80"])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), EXAMPLE_OUT)
        self.assertEqual(out, json.dumps(EXAMPLE_OUT, indent=2) + "\n")
        self.assertEqual(self.calls, [
            ("GET", "/projects.json"), ("GET", "/api/pipeline/effective/sample"), ("GET", "/api/board"),
            ("GET", "/api/board/archive?page=1"), ("POST", "/api/backlog/triage/verdicts")])

    def test_same_threshold_gives_one_row_and_no_flag_uses_current(self):
        for args in ((), ("--threshold", "90")):
            code, out, _ = self.replay("sample", *args)
            self.assertEqual(code, 0)
            self.assertEqual(out["thresholds"], EXAMPLE_OUT["thresholds"][:1])

    def test_old_records_infer_their_decider(self):
        self.board["jobs"] = [
            job(1, "sample: Add CSV export to reports", "approved", via="chat", by="wsl"),
            job(2, "sample: Add a dark mode toggle", "approved"),
            job(3, "sample: Rotate the deploy token", "denied"),
            job(4, "sample: Pick export scope", "approved"),
            job(5, "sample: Queued by triage", "approved", by="triage"),
            job(6, "sample: Asked to rework", "rework"),
            job(7, "other: Another project", "denied"),
            job(8, "sample: Deny recommended", "denied"),
            job(9, "sample: Never rated", "approved"),
        ]
        self.stored = {k: {key: val for key, val in v.items() if key != "decider"}
                       for k, v in EXAMPLE_STORED.items()}
        self.stored["sample: Deny recommended"] = {"decision": "hold", "confidence": None, "threshold": 90,
                                                   "reason": "its author recommends deny", "model": None}
        self.stored["sample: Pick export scope"]["confidence"] = 64
        code, out, _ = self.replay("sample", "--threshold", "80")
        self.assertEqual(code, 0)
        self.assertEqual(out["labels"], 6)
        self.assertEqual(out["scored"], 3)
        self.assertEqual(out["unscored"], {"rule": 2, "no_verdict": 1})
        self.assertEqual(out["model_queued"], 1)
        self.assertEqual(out["thresholds"], EXAMPLE_OUT["thresholds"])

    def test_inference_helpers(self):
        m = self.module
        self.assertEqual(m.record_decider({"decider": "model", "by": "wsl"}), "model")
        self.assertEqual(m.record_decider({"by": "triage"}), "model")
        self.assertEqual(m.record_decider({"by": "wsl"}), "person")
        self.assertEqual(m.record_decider({}), "person")
        self.assertEqual(m.verdict_decider({"decider": "rule", "confidence": 80}), "rule")
        self.assertEqual(m.verdict_decider({"confidence": None, "reason": "x"}), "rule")
        self.assertEqual(m.verdict_decider({"confidence": 64, "reason": "it asks questions a person must answer"}),
                         "rule")
        self.assertEqual(m.verdict_decider({"confidence": 72, "reason": "x"}), "model")

    def test_reads_every_archive_page_and_latest_decision_wins(self):
        self.board["jobs"] = EXAMPLE_JOBS[:2]
        self.archive = [[EXAMPLE_JOBS[2]],
                        [EXAMPLE_JOBS[3], job(5, "sample: Add CSV export to reports", "denied",
                                              at="2026-10-01T00:00:00Z", decider="person")]]
        code, out, _ = self.replay("sample", "--threshold", "80")
        self.assertEqual(code, 0)
        self.assertEqual(out, EXAMPLE_OUT)
        self.assertIn(("GET", "/api/board/archive?page=2"), self.calls)
        self.assertNotIn(("GET", "/api/board/archive?page=3"), self.calls)

    def test_archive_404_counts_as_empty(self):
        self.archive_status = 404
        code, out, _ = self.replay("sample", "--threshold", "80")
        self.assertEqual(code, 0)
        self.assertEqual(out, EXAMPLE_OUT)

    def test_verdicts_are_read_in_chunks_of_50(self):
        self.board["jobs"] = [job(n, f"sample: Item {n}", "denied") for n in range(1, 61)]
        code, out, _ = self.replay("sample")
        self.assertEqual(code, 0)
        self.assertEqual(out["unscored"], {"rule": 0, "no_verdict": 60})
        self.assertEqual(out["thresholds"][0]["agreement"], None)
        self.assertEqual(self.calls.count(("POST", "/api/backlog/triage/verdicts")), 2)

    def test_bad_arguments_exit_2(self):
        for args in (["triage", "rerun", "sample"], ["triage", "replay", "sample", "--threshold"],
                     ["triage", "replay", "sample", "--limit", "80"], ["triage", "replay", "bad slug!"],
                     ["triage", "replay"], ["triage"], ["triage", "replay", "sample", "--threshold", "80", "x"]):
            code, out, err = invoke(self.module, args)
            self.assertEqual((code, out, err), (2, "", USAGE), args)
        for value in ("0", "101", "8.5", "x", "-5"):
            code, _, err = invoke(self.module, ["triage", "replay", "sample", "--threshold", value])
            self.assertEqual((code, err), (2, "factory.py: --threshold: must be a whole number from 1 to 100\n"),
                             value)
        self.assertEqual(self.calls, [])

    def test_unregistered_slug_exits_4(self):
        code, _, err = invoke(self.module, ["triage", "replay", "missing"])
        self.assertEqual((code, err), (4, "factory.py: no factory project missing\n"))

    def test_no_threshold_and_no_flag_exits_2(self):
        self.view = (200, {"triage": {"enabled": False}})
        code, _, err = invoke(self.module, ["triage", "replay", "sample"])
        self.assertEqual((code, err), (2, "factory.py: project sample has no triage threshold; pass --threshold N\n"))
        code, out, _ = self.replay("sample", "--threshold", "80")
        self.assertEqual(code, 0)
        self.assertEqual(out["thresholds"], [dict(EXAMPLE_OUT["thresholds"][1], current=False)])

    def test_missing_or_invalid_pipeline_view_exits_2(self):
        for status, message in ((404, "no pipeline view; register the project or deploy the current Worker"),
                                (409, "stored pipeline configuration no longer validates")):
            self.view = (status, {})
            for args in ((), ("--threshold", "80")):
                code, out, err = invoke(self.module, ["triage", "replay", "sample", *args])
                self.assertEqual((code, out, err), (2, "", f"factory.py: {message}\n"), (status, args))

    def test_rules_file_rerates_scored_lines_and_prints_the_spec_json(self):
        self.rules_file()
        code, out, err = invoke(self.module, ["triage", "replay", "sample", "--threshold", "80",
                                              "--rules-file", "rules.txt"])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, json.dumps(RULES_OUT, indent=2) + "\n")
        self.assertEqual(self.calls, [
            ("GET", "/projects.json"), ("GET", "/api/pipeline/effective/sample"), ("GET", "/api/board"),
            ("GET", "/api/board/archive?page=1"), ("POST", "/api/backlog/triage/verdicts"),
            ("POST", "/api/backlog/triage/rate")])
        self.assertEqual(self.rate_bodies, [{"rules": RULES, "items": list(EXAMPLE_RATED)}])
        code, out, _ = self.replay("sample", "--rules-file", "rules.txt", "--threshold", "80")
        self.assertEqual((code, out), (0, RULES_OUT))

    def test_rules_file_rates_eight_per_call_and_counts_null_as_rating_failed(self):
        self.rules_file()
        self.board["jobs"] = [job(n, f"sample: Item {n}", "approved", decider="person") for n in range(1, 10)]
        self.stored = {f"sample: Item {n}": hold(50, decider="model") for n in range(1, 10)}
        self.rated = {f"sample: Item {n}": 95 for n in range(1, 9)}
        code, out, _ = self.replay("sample", "--rules-file", "rules.txt")
        self.assertEqual(code, 0)
        self.assertEqual([len(b["items"]) for b in self.rate_bodies], [8, 1])
        self.assertEqual(out["scored"], 8)
        self.assertEqual(out["unscored"], {"rule": 0, "no_verdict": 0, "rating_failed": 1})
        self.assertEqual(out["thresholds"][0]["queue_approved"], 8)

    def test_rules_file_errors_exit_2_before_any_call(self):
        self.rules_file()
        with open("bad.txt", "wb") as f:
            f.write(b"\xff\xfe rules")
        for path in ("missing.txt", "bad.txt", "."):
            code, out, err = invoke(self.module, ["triage", "replay", "sample", "--rules-file", path])
            self.assertEqual((code, out, err), (2, "", f"factory.py: --rules-file: cannot read {path}\n"), path)
        for args in (["--rules-file"], ["--rules-file", "rules.txt", "--rules-file", "rules.txt"],
                     ["--threshold", "80", "--threshold", "80"], ["--rules-file", "rules.txt", "--threshold"]):
            code, out, err = invoke(self.module, ["triage", "replay", "sample", *args])
            self.assertEqual((code, out, err), (2, "", USAGE), args)
        self.assertEqual(self.calls, [])
