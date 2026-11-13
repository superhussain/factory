import contextlib
import io
import json
import os
import shlex
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
DEVICE = "host-a"
UNITS = ("014-inline-images", "015-report-cache", "016-doc-lint", "017-another", "018-more")
PLACEHOLDERS = ["{id}", "{project}", "{repo_path}", "{session}", "{provider}", "{model}", "{effort}",
                "{prompt}", "{unknown}", "x-{id}-y"]
ROUTES = "---\nprimary: provider-a model-a high\nfallback: provider-b model-b\n---\n"
EXPERIMENT = "experiment: pilot provider-b model-b high 50\n"
RECOVER_NOTE = "spec approved in Tracer"
# Records the launch, then a usage limit only on the model named in fail-model.
LIMIT_FAKE = """import json, os, sys
root = os.path.dirname(os.path.abspath(__file__))
model = os.environ.get("FACTORY_MODEL")
path = os.path.join(root, "records.json")
records = []
if os.path.exists(path):
    with open(path, encoding="utf-8") as f:
        records = json.load(f)
records.append({"argv": sys.argv[1:], "cwd": os.getcwd(), "pid": os.getpid(),
                "env": {k: v for k, v in os.environ.items() if k.startswith("FACTORY_")}})
with open(path, "w", encoding="utf-8") as f:
    json.dump(records, f)
marker = os.path.join(root, "fail-model")
if os.path.exists(marker) and open(marker, encoding="utf-8").read().strip() == model:
    print("usage limit reached; resets in 45 minutes", flush=True)
    sys.exit(1)
print("started", flush=True)
"""


def stamp(**delta):
    return (datetime.now(timezone.utc) + timedelta(**delta)).strftime("%Y-%m-%dT%H:%M:%SZ")


def effective(slug):
    return {"project": slug, "merge": "auto", "review_rounds": 2, "reviewers": ["security", "ux", "ui", "code"],
            "gates": [], "pickup": {"enabled": True, "hosts": ["other-host", DEVICE]},
            "stage_notes": {}, "versions": {"global": 1, "project": 1}, "floors": []}


def idea(job_id, project="alpha", **extra):
    return dict({"id": job_id, "stage": "idea", "project": project, "title": "Feature", "idea": "Feature",
                 "updated": "2026-01-01T00:00:00Z"}, **extra)


class ParseTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)
        self.routes = self.home / "routes.md"
        self.routes.write_text(ROUTES, encoding="utf-8")
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "api", side_effect=AssertionError("parsing must not call the board")).start()
        self.errors = io.StringIO()
        errors = contextlib.redirect_stderr(self.errors)
        errors.__enter__()
        self.addCleanup(errors.__exit__, None, None, None)

    def read(self, line=None):
        if line is not None:
            self.routes.write_text(ROUTES[:4] + line + ROUTES[4:], encoding="utf-8")
        return factory.read_experiment(self.home, ["primary", "fallback"])

    def exits(self, line):
        self.errors.seek(0)
        self.errors.truncate()
        with self.assertRaises(SystemExit) as raised:
            self.read(line)
        self.assertEqual(raised.exception.code, 2)
        self.assertEqual(self.errors.getvalue(),
                         "factory.py: bad experiment in " + str(self.routes)
                         + ": use experiment: <name> <provider> <model> [<effort>] <percent>\n")

    def test_no_line_reads_none(self):
        self.assertIsNone(self.read())

    def test_accepts_the_documented_line(self):
        self.assertEqual(self.read(EXPERIMENT),
                         {"name": "pilot", "provider": "provider-b", "model": "model-b",
                          "effort": "high", "percent": 50})
        self.assertEqual(self.read("experiment: pilot provider-b model-b 50\n")["effort"], None)
        self.assertEqual(self.read("experiment: pilot provider-b model-b max 0\n")["percent"], 0)
        self.assertEqual(self.read("experiment: pilot provider-b model-b low 100\n")["percent"], 100)

    def test_malformed_lines_exit_2(self):
        for line in ("experiment: pilot provider-b model-b\n",
                     "experiment: pilot provider-b model-b high\n",
                     "experiment: pilot provider-b model-b high extra 50\n",
                     "experiment: pilot provider-b model;rm 50\n",
                     "experiment: pilot pro vider model-b 50\n",
                     "experiment: pri mary provider-b model-b 50\n",
                     "experiment: pilot provider-b model-b sideways 50\n",
                     "experiment: pilot provider-b model-b high 101\n",
                     "experiment: pilot provider-b model-b high -1\n",
                     "experiment: pilot provider-b model-b high fifty\n"):
            self.exits(line)

    def test_the_name_must_not_collide_with_a_route(self):
        self.exits("experiment: primary provider-b model-b 50\n")


class ArmTest(IsolatedTestCase):
    def test_percent_zero_sends_every_id_to_arm_a(self):
        for job_id in UNITS:
            self.assertEqual(factory.experiment_arm(job_id, 0), "a")

    def test_percent_100_sends_every_id_to_arm_b(self):
        for job_id in UNITS:
            self.assertEqual(factory.experiment_arm(job_id, 100), "b")

    def test_the_same_id_always_lands_in_the_same_arm(self):
        for job_id in UNITS:
            arms = {factory.experiment_arm(job_id, 50) for _ in range(5)}
            self.assertEqual(len(arms), 1, job_id)

    def test_the_documented_buckets(self):
        self.assertEqual(factory.experiment_arm("014-inline-images", 50), "b")
        self.assertEqual(factory.experiment_arm("015-report-cache", 50), "b")
        self.assertEqual(factory.experiment_arm("016-doc-lint", 50), "a")


class PickupFixture(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.home = self.root / "home"
        self.repo = self.root / "repo-a"
        self.home.mkdir()
        self.repo.mkdir(parents=True)
        (self.home / "config.md").write_text(f"---\ndevice: {DEVICE}\n---\n", encoding="utf-8")
        (self.home / "paths.md").write_text(f"---\nalpha: {self.repo}\n---\n", encoding="utf-8")
        self.routes = self.home / "routes.md"
        self.routes.write_text(ROUTES, encoding="utf-8")
        self.fake = self.root / "fake_launcher.py"
        self.fake.write_text(LIMIT_FAKE, encoding="utf-8")
        self.records = self.root / "records.json"
        self.write_launchers({"primary": [sys.executable, str(self.fake)] + PLACEHOLDERS,
                              "fallback": [sys.executable, str(self.fake)] + PLACEHOLDERS})
        self.board = {"updated": "2026-01-01T00:00:00Z", "jobs": []}
        self.views = {}
        self.calls = []
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "api", side_effect=self.request).start()
        patch.object(factory, "LAUNCH_WAIT_SECONDS", 60).start()
        patch.object(factory, "report_status").start()
        patch.object(factory, "backlog_triage_pass").start()
        patch.object(factory, "checkout_problem", return_value=None).start()

    def configure(self, line=None):
        if line is None:
            self.routes.write_text(ROUTES, encoding="utf-8")
        else:
            self.routes.write_text(ROUTES[:4] + line + ROUTES[4:], encoding="utf-8")

    def write_launchers(self, launchers):
        (self.home / "launchers.json").write_text(json.dumps({"version": 1, "launchers": launchers}),
                                                  encoding="utf-8")

    def request(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        self.calls.append((method, path))
        if path == "/api/board":
            if method == "GET":
                return 200, {"ETag": '"board"'}, factory.dump_board(self.board)
            self.assertEqual((method, headers), ("PUT", {"If-Match": '"board"'}))
            self.board = json.loads(body)
            return 200, {}, ""
        if path == "/api/hosts":
            return 404, {}, ""
        self.assertEqual(path[len("/api/pipeline/effective/"):], "alpha")
        return 200, {}, json.dumps(effective("alpha"))

    def pickup(self, *args):
        return invoke(factory, ["pickup", *args])

    def launched(self):
        if not self.records.exists():
            return []
        return json.loads(self.records.read_text(encoding="utf-8"))

    def job(self, job_id):
        return next(j for j in self.board["jobs"] if j["id"] == job_id)

    def argv(self, job_id, provider, model, effort="high"):
        return shlex.join([sys.executable, str(self.fake), job_id, "alpha", str(self.repo), "<session>",
                           provider, model, effort,
                           f"/factory idea {job_id} --session <session>\n\n" + factory.UNATTENDED,
                           "{unknown}", f"x-{job_id}-y"])

    def limits(self):
        path = self.home / "route-limits.json"
        return json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}


class DryRunTest(PickupFixture):
    def test_dry_run_prints_the_arm_with_the_line(self):
        self.configure(EXPERIMENT)
        self.board["jobs"] = [idea("014-inline-images"), idea("016-doc-lint")]
        code, out, err = self.pickup("--dry-run", "--max-inflight", "2")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"would launch 014-inline-images alpha on primary arm b model model-b: "
                              f"{self.argv('014-inline-images', 'provider-b', 'model-b')}\n"
                              f"would launch 016-doc-lint alpha on primary arm a model model-a: "
                              f"{self.argv('016-doc-lint', 'provider-a', 'model-a')}\n"
                              f"host {DEVICE}: would launch 2\n")
        self.assertEqual((self.launched(), self.board["jobs"][0].get("experiment")), ([], None))

    def test_dry_run_prints_the_recorded_arm_for_a_relaunch(self):
        self.board["jobs"] = [{"id": "001-sample", "stage": "build", "project": "alpha", "note": RECOVER_NOTE,
                               "updated": stamp(),
                               "experiment": {"name": "pilot", "arm": "b", "provider": "provider-x",
                                              "model": "model-x", "effort": "low"}}]
        code, out, err = self.pickup("--dry-run")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, "would launch 001-sample alpha on primary arm b model model-x: "
                              + shlex.join([sys.executable, str(self.fake), "001-sample", "alpha", str(self.repo),
                                            "<session>", "provider-x", "model-x", "low",
                                            f"/factory recover 001-sample --session <session>\n\n"
                                            + factory.UNATTENDED, "{unknown}", "x-001-sample-y"])
                              + f"\nhost {DEVICE}: would launch 1\n")

    def test_dry_run_without_the_line_is_byte_identical(self):
        self.board["jobs"] = [idea("014-inline-images"), idea("016-doc-lint")]
        code, out, err = self.pickup("--dry-run", "--max-inflight", "2")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"would launch 014-inline-images alpha on primary: "
                              f"{self.argv('014-inline-images', 'provider-a', 'model-a')}\n"
                              f"would launch 016-doc-lint alpha on primary: "
                              f"{self.argv('016-doc-lint', 'provider-a', 'model-a')}\n"
                              f"host {DEVICE}: would launch 2\n")


class ClaimTest(PickupFixture):
    def test_a_launch_stamps_arm_b_with_the_experiment_triple(self):
        self.configure(EXPERIMENT)
        self.board["jobs"] = [idea("014-inline-images")]
        code, out, err = self.pickup()
        self.assertEqual((code, err), (0, ""))
        [record] = self.launched()
        self.assertEqual((record["env"]["FACTORY_PROVIDER"], record["env"]["FACTORY_MODEL"],
                          record["env"]["FACTORY_EFFORT"]), ("provider-b", "model-b", "high"))
        self.assertEqual(self.job("014-inline-images")["experiment"],
                         {"name": "pilot", "arm": "b", "provider": "provider-b", "model": "model-b",
                          "effort": "high"})

    def test_a_launch_stamps_arm_a_with_the_route_triple(self):
        self.configure(EXPERIMENT)
        self.board["jobs"] = [idea("016-doc-lint")]
        code, out, err = self.pickup()
        self.assertEqual((code, err), (0, ""))
        [record] = self.launched()
        self.assertEqual((record["env"]["FACTORY_PROVIDER"], record["env"]["FACTORY_MODEL"],
                          record["env"]["FACTORY_EFFORT"]), ("provider-a", "model-a", "high"))
        self.assertEqual(self.job("016-doc-lint")["experiment"],
                         {"name": "pilot", "arm": "a", "provider": "provider-a", "model": "model-a",
                          "effort": "high"})

    def relaunch_job(self, **record):
        return {"id": "001-sample", "stage": "build", "project": "alpha", "note": RECOVER_NOTE,
                "updated": stamp(), "experiment": record}

    def test_a_relaunch_uses_the_recorded_triple_when_the_line_changed(self):
        self.configure(EXPERIMENT)
        self.board["jobs"] = [self.relaunch_job(name="pilot", arm="b", provider="provider-x",
                                                model="model-x", effort="low")]
        code, out, err = self.pickup()
        self.assertEqual((code, err), (0, ""))
        [record] = self.launched()
        self.assertEqual((record["env"]["FACTORY_PROVIDER"], record["env"]["FACTORY_MODEL"],
                          record["env"]["FACTORY_EFFORT"]), ("provider-x", "model-x", "low"))
        self.assertEqual(self.job("001-sample")["experiment"]["model"], "model-x")

    def test_a_relaunch_uses_the_recorded_triple_when_the_line_is_removed(self):
        self.board["jobs"] = [self.relaunch_job(name="pilot", arm="b", provider="provider-x",
                                                model="model-x", effort="low")]
        code, out, err = self.pickup()
        self.assertEqual((code, err), (0, ""))
        [record] = self.launched()
        self.assertEqual((record["env"]["FACTORY_PROVIDER"], record["env"]["FACTORY_MODEL"]),
                         ("provider-x", "model-x"))
        self.assertEqual(self.job("001-sample")["experiment"]["model"], "model-x")

    def test_a_fresh_claim_is_not_rewritten_by_a_second_run(self):
        self.configure(EXPERIMENT)
        self.board["jobs"] = [idea("016-doc-lint")]
        self.pickup()
        stamp_before = self.job("016-doc-lint")["experiment"]
        self.job("016-doc-lint").pop("claim")
        self.pickup()
        self.assertEqual(self.job("016-doc-lint")["experiment"], stamp_before)


class LimitTest(PickupFixture):
    def test_an_arm_b_limit_failure_limits_the_experiment_not_the_route(self):
        self.configure(EXPERIMENT)
        (self.root / "fail-model").write_text("model-b", encoding="utf-8")
        self.board["jobs"] = [idea("014-inline-images")]
        code, out, err = self.pickup()
        self.assertEqual((code, err), (0, ""))
        limits = self.limits()
        self.assertEqual(len(limits), 1)
        self.assertIn("pilot", limits)
        self.assertNotIn("primary", limits)
        self.assertNotIn("claim", self.job("014-inline-images"))
        self.assertEqual(out, f"limited experiment pilot until {limits['pilot']}\nreleased 014-inline-images\n")
        self.assertEqual(len(self.launched()), 1)

    def test_an_arm_a_limit_failure_limits_the_route(self):
        self.configure(EXPERIMENT)
        (self.root / "fail-model").write_text("model-a", encoding="utf-8")
        self.board["jobs"] = [idea("016-doc-lint")]
        code, out, err = self.pickup()
        self.assertEqual((code, err), (0, ""))
        limits = self.limits()
        self.assertIn("primary", limits)
        self.assertNotIn("pilot", limits)

    def test_limited_arm_b_jobs_are_skipped_while_arm_a_launches(self):
        self.configure(EXPERIMENT)
        until = stamp(minutes=45)
        (self.home / "route-limits.json").write_text(json.dumps({"pilot": until}), encoding="utf-8")
        self.board["jobs"] = [idea("014-inline-images"), idea("016-doc-lint")]
        code, out, err = self.pickup("--limit", "2")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out.count("launched "), 1)
        [record] = self.launched()
        self.assertEqual(record["argv"][0], "016-doc-lint")
        self.assertEqual((self.job("014-inline-images")["stage"], self.job("014-inline-images").get("claim")),
                         ("idea", None))
        self.assertEqual(self.job("016-doc-lint")["experiment"]["arm"], "a")

    def test_dry_run_prints_the_experiment_skip_line(self):
        self.configure(EXPERIMENT)
        until = stamp(minutes=45)
        (self.home / "route-limits.json").write_text(json.dumps({"pilot": until}), encoding="utf-8")
        self.board["jobs"] = [idea("014-inline-images"), idea("016-doc-lint")]
        code, out, err = self.pickup("--dry-run", "--max-inflight", "2")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"skip 014-inline-images alpha: experiment pilot limited until {until}\n"
                              f"would launch 016-doc-lint alpha on primary arm a model model-a: "
                              f"{self.argv('016-doc-lint', 'provider-a', 'model-a')}\n"
                              f"host {DEVICE}: would launch 1\n")

    def test_a_limited_arm_b_lead_does_not_stall_the_batch(self):
        self.configure(EXPERIMENT)
        until = stamp(minutes=45)
        (self.home / "route-limits.json").write_text(json.dumps({"pilot": until}), encoding="utf-8")
        follow = [{"id": "013-old", "kind": "follow-up-of"}]
        self.board["jobs"] = [idea("014-inline-images", refs=follow, updated=stamp(hours=-13)),
                              idea("016-doc-lint", refs=follow, updated=stamp(hours=-13))]
        code, out, err = self.pickup("--dry-run", "--max-inflight", "2")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"skip 014-inline-images alpha: experiment pilot limited until {until}\n"
                              f"would launch 016-doc-lint alpha on primary arm a model model-a: "
                              f"{self.argv('016-doc-lint', 'provider-a', 'model-a')}\n"
                              f"host {DEVICE}: would launch 1\n")


class ReportFixture(PickupFixture):
    """A fixture board and local folders for the report command."""

    def setUp(self):
        super().setUp()
        self.configure(EXPERIMENT)

    def folder(self, job_id, files):
        base = self.home / "jobs" / "alpha" / job_id
        for name, text in files.items():
            path = base / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")

    def report(self, name="pilot"):
        self.calls.clear()
        return invoke(factory, ["experiment", "report", name])


class ReportTest(ReportFixture):
    def setUp(self):
        super().setUp()
        spec = {"spec.md": "# spec\n"}
        review = {f"round-{i + 1}/review-{r}.md": "PASS" for i in range(2)
                  for r in ("security", "ux", "ui", "code")}
        self.folder("016-doc-lint", {**spec, **review, "decision.md": "APPROVE\n\nEvidence.\n"})
        self.folder("021-extra", spec)
        self.folder("014-inline-images", {**spec,
                                          **{f"round-1/review-{r}.md": "PASS" for r in ("security", "ux", "ui", "code")}})
        self.folder("017-skipped", {**spec, "decision.md": "let's talk first\n"})
        self.folder("023-blank", spec)
        (self.home / "jobs" / "alpha" / "023-blank" / "decision.md").write_bytes(b"\xff\xfe\x00bad")
        round_a = [{"security": "PASS", "ux": "PASS", "ui": "PASS", "code": "CHANGES"},
                   {"security": "PASS", "ux": "PASS", "ui": "PASS", "code": "PASS"}]
        self.board["jobs"] = [
            idea("016-doc-lint", stage="merged", round=2, rounds=round_a, pr="https://github.com/o/r/pull/1",
                 done=stamp(days=-2), started=stamp(days=-3),
                 experiment={"name": "pilot", "arm": "a", "provider": "provider-a", "model": "model-a",
                             "effort": "high"}),
            idea("021-extra", stage="approve", round=1,
                 rounds=[{"security": "pending", "ux": "pending", "ui": "pending", "code": "pending"}],
                 resumes={"count": 2, "key": "000000000000"}, started="not-a-stamp",
                 experiment={"name": "pilot", "arm": "a", "provider": "provider-a", "model": "model-a",
                             "effort": "high"}),
            idea("014-inline-images", stage="closed", round=1,
                 rounds=[{"security": "CHANGES", "ux": "PASS", "ui": "PASS", "code": "PASS"}],
                 pr="https://github.com/o/r/pull/2",
                 experiment={"name": "pilot", "arm": "b", "provider": "provider-b", "model": "model-b",
                             "effort": "high"}),
            idea("017-skipped", stage="pr-open", round=1,
                 rounds=[{"security": "CHANGES", "ux": "PASS", "ui": "PASS", "code": "PASS"}],
                 pr="https://github.com/o/r/pull/3",
                 experiment={"name": "pilot", "arm": "b", "provider": "provider-b", "model": "model-b",
                             "effort": "high"}),
            idea("022-gone", stage="review", escalated=True,
                 experiment={"name": "pilot", "arm": "b", "provider": "provider-b", "model": "model-b",
                             "effort": "high"}),
            idea("023-blank", stage="review", pr="https://github.com/o/r/pull/4",
                 experiment={"name": "pilot", "arm": "b", "provider": "provider-b", "model": "model-b",
                             "effort": "high"}),
            idea("024-rework", stage="idea", started=stamp(hours=-1),
                 refs=[{"id": "016-doc-lint", "kind": "rework-of"}]),
            idea("025-surfaced", stage="idea", started=stamp(hours=-2),
                 refs=[{"id": "016-doc-lint", "kind": "follow-up-of"}]),
            idea("026-old", stage="idea", started=stamp(days=30),
                 refs=[{"id": "016-doc-lint", "kind": "follow-up-of"}]),
            idea("027-unreadable", stage="idea", updated="garbage",
                 refs=[{"id": "016-doc-lint", "kind": "follow-up-of"}]),
            idea("028-unkanchor", stage="idea", started=stamp(hours=-1),
                 refs=[{"id": "021-extra", "kind": "rework-of"}]),
            idea("029-renamed", stage="merged",
                 experiment={"name": "pilot2", "arm": "b", "provider": "provider-z", "model": "model-z",
                             "effort": "low"}),
        ]

    def test_the_report_prints_per_arm_metrics(self):
        code, out, err = self.report()
        self.assertEqual((code, err), (0, ""))
        report = json.loads(out)
        self.assertEqual(report["name"], "pilot")
        self.assertEqual(report["a"], {
            "jobs": 2, "merged": 1, "closed": 0, "needs_human": 0, "escalated": 0,
            "mean_review_rounds": 1.5, "changes_verdicts": 1, "stuck_resumes": 2,
            "follow_ups_7d": {"rework_of": ["024-rework"], "surfaced": ["025-surfaced"], "unknown_window": 2},
            "skipped_steps": {"missing_round_review": [], "missing_decision": [], "pr_before_approve": []},
            "coverage_unknown": 0})
        self.assertEqual(report["b"], {
            "jobs": 4, "merged": 0, "closed": 1, "needs_human": 0, "escalated": 1,
            "mean_review_rounds": 1.0, "changes_verdicts": 2, "stuck_resumes": 0,
            "follow_ups_7d": {"rework_of": [], "surfaced": [], "unknown_window": 0},
            "skipped_steps": {"missing_round_review": ["017-skipped"], "missing_decision": ["014-inline-images"],
                              "pr_before_approve": ["017-skipped"]},
            "coverage_unknown": 2})

    def test_the_report_writes_nothing(self):
        from support import snapshot
        before = snapshot(self.home)
        code, out, err = self.report()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(snapshot(self.home), before)
        self.assertEqual([call for call in self.calls if call[0] != "GET"], [])

    def test_the_report_matches_by_recorded_name(self):
        code, out, err = self.report("pilot2")
        self.assertEqual((code, err), (0, ""))
        report = json.loads(out)
        self.assertEqual((report["a"]["jobs"], report["b"]["jobs"]), (0, 1))
        self.assertEqual(report["b"]["merged"], 1)

    def test_the_report_rejects_bad_usage(self):
        for args in (["pick", "pilot"], ["report", "al pha"], ["report"]):
            self.assertEqual(invoke(factory, ["experiment", *args])[0], 2)


class ReportBoundaryTest(ReportFixture):
    def setUp(self):
        super().setUp()
        arm_a = {"name": "pilot", "arm": "a", "provider": "provider-a", "model": "model-a", "effort": "high"}
        spec = {"spec.md": "# spec\n"}
        planted = self.home / "outside" / "030-up" / "spec.md"
        planted.parent.mkdir(parents=True)
        planted.write_text("# spec\n", encoding="utf-8")
        self.folder("031-reviewer", spec)
        self.folder("032-ok", spec)
        self.board["jobs"] = [
            idea("030-up", project="../outside", stage="review", experiment=arm_a),
            idea("031-reviewer", stage="review", rounds=[{"../../x": "PASS"}], experiment=arm_a),
            idea("032-ok", stage="review", experiment=arm_a),
        ]

    def test_traversal_keys_count_as_coverage_unknown_without_a_probe(self):
        probed = []

        def spy(original):
            def probe(path):
                probed.append(os.path.normpath(path))
                return original(path)
            return probe
        with patch.object(Path, "is_dir", spy(Path.is_dir)), patch.object(Path, "is_file", spy(Path.is_file)):
            code, out, err = self.report()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out)["a"]["coverage_unknown"], 2)
        root = os.path.join(self.home, "jobs", "alpha") + os.sep
        self.assertTrue(probed)
        self.assertEqual([p for p in probed if not p.startswith(root)], [])


if __name__ == "__main__":
    unittest.main()
