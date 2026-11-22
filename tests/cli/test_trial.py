import json
import os
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
RUN = "20261111T090000Z-a1b2c3"
TRIAL = "factory-spec-v8"
CHAMPION = "Name the acceptance check for each criterion."
CHALLENGER = CHAMPION + " Put the test file in the Check column."
EDIT_SUMMARY = "Spec note: name the test file for each criterion"
PIPELINE_GET = "GET /api/pipeline/projects/factory"
HISTORY_GET = "GET /api/pipeline/history/projects/factory/7"
REVERT_ROUTE = "PUT /api/pipeline/retro/projects/factory"
RESUME_ROUTE = "POST /api/trials/resume/factory"
REASON = "Owner asked in chat to resume factory trials"
PAUSE_LINE = ("stage-note trials paused for factory after 2 reverts; "
              "a person runs trial resume factory")


def edit(**over):
    e = {"scope": "projects/factory", "stage": "spec", "note": CHALLENGER, "summary": EDIT_SUMMARY}
    e.update(over)
    return e


def reply(edits):
    finding = {"title": "ls alias crashes in non-interactive shells", "category": "tool-economy", "count": 253,
               "cost": 250, "sessions": ["host-a/3ae1fc86-0000-4000-8000-000000000001"],
               "root_cause": "The colorls alias needs a terminal size.",
               "fix": "Alias ls only in interactive shells.", "project": "sample-tools"}
    return {"schema": "factory.retro/1", "run": RUN, "findings": [finding], "suggestions": [], "edits": edits}


def stamp(**delta):
    return (datetime.now(timezone.utc) + timedelta(**delta)).strftime("%Y-%m-%dT%H:%M:%SZ")


def trial(**over):
    t = {"project": "factory", "stage": "spec", "version": 8, "run": RUN, "summary": EDIT_SUMMARY,
         "champion_version": 7, "champion": CHAMPION, "challenger": CHALLENGER,
         "opened": stamp(days=-3), "status": "open"}
    t.update(over)
    t.setdefault("id", f"{t['project']}-{t['stage']}-v{t['version']}")
    return t


def arm_ids(trial_id, arm, count, start=2100):
    """The first count made-up job ids from start that trial_arm puts in arm."""
    ids, n = [], start
    while len(ids) < count:
        job_id = f"{n}-job"
        if factory.trial_arm(trial_id, job_id) == arm:
            ids.append(job_id)
        n += 1
    return ids


def finished(job_id, rounds=1, tokens=1000, project="factory", started=None):
    return {"id": job_id, "project": project, "stage": "merged", "round": rounds, "started": started or stamp(days=-2),
            "tokens": {"host-a": {"models": {"model-a": {"input": tokens}}}}}


def arm_jobs(a=None, b=None, count=5):
    """count finished jobs per arm of TRIAL; a and b override each arm's rounds and tokens."""
    return ([finished(i, **(a or {})) for i in arm_ids(TRIAL, "a", count)]
            + [finished(i, **(b or {})) for i in arm_ids(TRIAL, "b", count)])


def running(trials=(), jobs=(), paused=False):
    return {"updated": "2026-11-12T09:00:00Z", "jobs": list(jobs), "trials": list(trials),
            "autonomy": {"paused": paused, "projects": ["factory"], "self": []}}


class TrialHelpersTest(unittest.TestCase):
    def test_arm_is_a_stable_half_split_per_trial(self):
        self.assertEqual(factory.trial_arm(TRIAL, "2100-x"), factory.experiment_arm(f"{TRIAL}/2100-x", 50))
        arms = [factory.trial_arm(TRIAL, f"{n}-x") for n in range(2100, 2300)]
        self.assertTrue(70 < arms.count("a") < 130)
        self.assertEqual(arms, [factory.trial_arm(TRIAL, f"{n}-x") for n in range(2100, 2300)])

    def test_open_trial_for_and_pause(self):
        data = running([trial(status="kept"), trial(stage="build"),
                        trial(project="sample", version=2)])
        self.assertIsNone(factory.open_trial_for(data, "factory", "spec"))
        self.assertEqual(factory.open_trial_for(data, "factory", "build")["id"], "factory-build-v8")
        self.assertIsNone(factory.open_trial_for({"jobs": []}, "factory", "spec"))
        reverted = [trial(version=5, status="reverted"), trial(version=6, status="reverted")]
        self.assertTrue(factory.trials_paused(running(reverted), "factory"))
        self.assertFalse(factory.trials_paused(running(reverted), "sample"))
        self.assertFalse(factory.trials_paused(running(reverted[:1]), "factory"))
        reverted[0]["resumed"] = stamp()
        self.assertFalse(factory.trials_paused(running(reverted), "factory"))

    def test_revert_failed_neither_blocks_nor_pauses(self):
        failed = running([trial(version=5, status="revert-failed"), trial(version=6, status="revert-failed")])
        self.assertIsNone(factory.open_trial_for(failed, "factory", "spec"))
        self.assertFalse(factory.trials_paused(failed, "factory"))

    def test_trial_jobs_need_the_project_and_a_start_at_or_after_opened(self):
        t = trial(opened="2026-11-12T09:00:00Z")
        a, b = arm_ids(TRIAL, "a", 1)[0], arm_ids(TRIAL, "b", 1)[0]
        jobs = [{"id": a, "project": "factory", "started": "2026-11-12T09:00:00Z"},
                {"id": b, "project": "factory", "started": "2026-11-13T09:00:00Z"},
                {"id": "2001-early", "project": "factory", "started": "2026-11-12T08:59:59Z"},
                {"id": "2002-none", "project": "factory"},
                {"id": "2003-other", "project": "sample", "started": "2026-11-13T09:00:00Z"}]
        arms = factory.trial_jobs({"jobs": jobs}, t)
        self.assertEqual({arm: list(ids) for arm, ids in arms.items()}, {"a": [a], "b": [b]})


class TrialOpenTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        os.environ["FACTORY_HOME"] = str(self.home)
        patcher = patch.object(factory, "retro_upload")
        patcher.start()
        self.addCleanup(patcher.stop)
        self.folder = self.home / "retro" / RUN
        self.folder.mkdir(parents=True)
        agg = {"schema": "factory.retro-aggregate/1", "run": RUN, "from": "2026-11-05", "to": "2026-11-11",
               "host": "host-a", "projects": ["factory", "sample-tools"], "hosts": {"host-a": {"sessions": 4, "jobs": 2, "ad_hoc": 0}},
               "pipeline": {"global": {"version": 0, "stage_notes": {}},
                            "projects/factory": {"version": 7, "stage_notes": {"spec": CHAMPION}}}}
        (self.folder / "aggregate.json").write_text(json.dumps(agg), encoding="utf-8")

    def admit(self, edits, trials=()):
        (self.folder / "reply.json").write_text(json.dumps(reply(edits=edits)), encoding="utf-8")
        routes = {"GET /api/board": (200, running(trials)),
                  REVERT_ROUTE: (200, b"ok", {"X-Pipeline-Version": "8"}), "PUT /api/board": (200, b"ok")}
        with http_fixture(routes) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\ndevice: mac\n---\n")
            code, out, err = invoke(factory, ["retro", "admit", RUN])
        return code, out, err, server.requests, (self.folder / "findings.md").read_text(encoding="utf-8")

    def test_saved_edit_opens_a_trial_with_champion_and_challenger(self):
        before = stamp()
        code, out, err, requests, _ = self.admit([edit()])
        self.assertEqual((code, err), (0, ""))
        self.assertIn(f"trial {TRIAL} opened: alternate jobs keep the old note until trial settle decides\n", out)
        board = json.loads(next(body for m, p, _, body in requests if f"{m} {p}" == "PUT /api/board"))
        record = board["trials"][0]
        self.assertTrue(before <= record.pop("opened") <= stamp())
        self.assertEqual(record, {"id": TRIAL, "project": "factory", "stage": "spec", "version": 8, "run": RUN,
                                  "summary": EDIT_SUMMARY, "champion_version": 7, "champion": CHAMPION,
                                  "challenger": CHALLENGER, "status": "open"})

    def test_new_stage_opens_with_an_empty_champion(self):
        build = edit(stage="build", note="Run the tests.", summary="Build note: run the tests")
        _, out, _, requests, _ = self.admit([build])
        self.assertIn("trial factory-build-v8 opened", out)
        board = json.loads(next(body for m, p, _, body in requests if f"{m} {p}" == "PUT /api/board"))
        self.assertEqual(board["trials"][0]["champion"], "")

    def test_open_trial_skips_the_edit_before_the_daily_cap(self):
        code, out, err, requests, table = self.admit([edit()], [trial()])
        line = "a trial is open for projects/factory stage_notes.spec"
        self.assertEqual((code, err), (0, ""))
        self.assertIn(f"skipped edits[0]: {line}\n", out)
        self.assertEqual([f"{m} {p}" for m, p, *_ in requests], ["GET /api/board"])
        self.assertIn(f"| projects/factory | spec | skipped: {line} | {EDIT_SUMMARY} |", table)
        # Another stage of the same project is still trialled.
        build = edit(stage="build", note="Run the tests.", summary="Build note: run the tests")
        _, out, _, requests, _ = self.admit([build], [trial()])
        self.assertIn("trial factory-build-v8 opened", out)

    def test_paused_project_skips_stage_note_edits(self):
        reverts = [trial(version=5, status="reverted"), trial(version=6, status="reverted")]
        line = "stage-note trials are paused after 2 reverts; a person runs trial resume factory"
        code, out, err, requests, table = self.admit([edit()], reverts)
        self.assertEqual((code, err), (0, ""))
        self.assertIn(f"skipped edits[0]: {line}\n", out)
        self.assertEqual([f"{m} {p}" for m, p, *_ in requests], ["GET /api/board"])
        self.assertIn(f"| projects/factory | spec | skipped: {line} |", table)
        reverts[0]["resumed"] = stamp()
        _, out, _, _, _ = self.admit([edit()], reverts)
        self.assertIn(f"trial {TRIAL} opened", out)


PROMPT_VIEW = {"project": "sample", "merge": "human", "review_rounds": 2, "reviewers": ["security", "ux", "ui", "code"],
               "gates": [], "versions": {"global": 1, "project": 8}}
SAMPLE_TRIAL = "sample-spec-v8"


class TrialPromptTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        (self.home / "projects" / "sample.documents.md").write_text("No documents.\n")
        tempdir = patch.object(tempfile, "tempdir", str(self.home))
        tempdir.start()
        self.addCleanup(tempdir.stop)

    def prompt(self, job_id, stored=CHALLENGER, champion=CHAMPION, started=None, role="spec", shown=CHAMPION):
        """champion is version 7's spec note in pipeline history; shown is the board record's champion text."""
        t = trial(project="sample", champion=shown)
        board = {"updated": "", "trials": [t], "jobs": [
            {"id": job_id, "project": "sample", "title": "Job", "idea": "Do it.", "stage": "spec",
             "started": started or stamp(days=-1)}]}
        view = dict(PROMPT_VIEW, stage_notes={stage: [{"source": "pipeline:global@1", "text": f"Global {stage}."},
                                                      {"source": "pipeline:sample@8", "text": stored}]
                                              for stage in ("spec", "build")})
        history = {"stage_notes": {"spec": champion} if champion else {}}
        routes = {"/api/board": (200, board), "/api/pipeline/effective/sample": (200, view),
                  "/api/pipeline/history/projects/sample/7": (200, history)}
        with http_fixture(routes) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            code, out, err = invoke(factory, ["--home", str(self.home), "prompt", f"sample/{job_id}", role])
        self.assertEqual((code, err), (0, ""))
        return out

    def test_arm_a_reads_the_champion_and_arm_b_the_challenger(self):
        a, b = arm_ids(SAMPLE_TRIAL, "a", 1)[0], arm_ids(SAMPLE_TRIAL, "b", 1)[0]
        out = self.prompt(a)
        self.assertIn(f'source="pipeline:sample@8" stage="spec" trust="data">\n{CHAMPION}\n', out)
        self.assertNotIn(CHALLENGER, out)
        self.assertIn("Global spec.", out)
        out = self.prompt(b)
        self.assertIn(f'source="pipeline:sample@8" stage="spec" trust="data">\n{CHALLENGER}\n', out)

    def test_other_stage_and_earlier_job_read_the_stored_note(self):
        a = arm_ids(SAMPLE_TRIAL, "a", 1)[0]
        self.assertIn(CHALLENGER, self.prompt(a, started=stamp(days=-5)))
        build = self.prompt(a, role="build")
        self.assertIn(f'stage="build" trust="data">\n{CHALLENGER}\n', build)

    def test_superseded_note_is_never_swapped(self):
        a = arm_ids(SAMPLE_TRIAL, "a", 1)[0]
        out = self.prompt(a, stored="A person rewrote this note.")
        self.assertIn("A person rewrote this note.", out)
        self.assertNotIn(CHAMPION, out)

    def test_empty_champion_drops_the_project_note(self):
        a = arm_ids(SAMPLE_TRIAL, "a", 1)[0]
        out = self.prompt(a, champion="")
        self.assertNotIn("pipeline:sample@8", out)
        self.assertIn("Global spec.", out)

    def test_board_champion_text_is_never_used(self):
        a = arm_ids(SAMPLE_TRIAL, "a", 1)[0]
        out = self.prompt(a, shown="Text a board writer put on the record.")
        self.assertIn(f'source="pipeline:sample@8" stage="spec" trust="data">\n{CHAMPION}\n', out)
        self.assertNotIn("Text a board writer put on the record.", out)
        out = self.prompt(a, champion="", shown="Text a board writer put on the record.")
        self.assertNotIn("Text a board writer put on the record.", out)
        self.assertNotIn("pipeline:sample@8", out)

    def test_malformed_trial_record_is_ignored(self):
        a = arm_ids(SAMPLE_TRIAL, "a", 1)[0]
        data = {"trials": [trial(project="sample", id="sample-spec-v9"), trial(project="sample", stage="../x"),
                           trial(project="../sample"), trial(project="sample", champion_version="7")]}
        self.assertIsNone(factory.open_trial_for(data, "sample", "spec"))
        self.assertIsNotNone(factory.open_trial_for({"trials": [trial(project="sample")]}, "sample", "spec"))


class TrialWorkerCase(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        os.environ["FACTORY_HOME"] = str(self.home)

    def run_trial(self, board, *args, routes=None):
        routes = {"GET /api/board": (200, board), "PUT /api/board": (200, b"ok"),
                  PIPELINE_GET: (200, {"stage_notes": {"spec": CHALLENGER}}, {"X-Pipeline-Version": "8"}),
                  HISTORY_GET: (200, {"stage_notes": {"spec": CHAMPION}}),
                  REVERT_ROUTE: (200, b"ok", {"X-Pipeline-Version": "9"}), **(routes or {})}
        with http_fixture(routes) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\ndevice: host-a\n---\n")
            code, out, err = invoke(factory, ["trial", *args])
        written = [json.loads(body) for m, p, _, body in server.requests if f"{m} {p}" == "PUT /api/board"]
        return code, out, err, server.requests, written


class TrialSettleTest(TrialWorkerCase):
    def settle(self, board, routes=None):
        return self.run_trial(board, "settle", routes=routes)

    def test_b_higher_reverts_to_the_champion(self):
        code, out, err, requests, written = self.settle(running([trial()], arm_jobs(b={"rounds": 3})))
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"trial {TRIAL} reverted to version 9: rounds b higher, tokens no difference\n")
        self.assertEqual([f"{m} {p}" for m, p, *_ in requests],
                         ["GET /api/board", PIPELINE_GET, HISTORY_GET, REVERT_ROUTE, "GET /api/board",
                          "PUT /api/board"])
        self.assertEqual(json.loads(requests[3][3]), {
            "schema": "factory.retro-edit/1", "run": RUN, "base_version": 8, "stage": "spec",
            "note": CHAMPION, "summary": f"Revert trial {TRIAL}"})
        record = written[0]["trials"][0]
        self.assertEqual((record["status"], record["revert_version"]), ("reverted", 9))
        self.assertEqual(record["reads"], {"rounds": "b higher", "tokens": "no difference"})
        self.assertEqual(record["arms"], {"a": arm_ids(TRIAL, "a", 5), "b": arm_ids(TRIAL, "b", 5)})
        self.assertIsNotNone(factory.stamp_of(record["settled"]))

    def test_b_higher_on_tokens_alone_reverts(self):
        _, out, _, _, _ = self.settle(running([trial()], arm_jobs(b={"tokens": 5000})))
        self.assertEqual(out, f"trial {TRIAL} reverted to version 9: rounds no difference, tokens b higher\n")

    def test_no_difference_or_b_lower_keeps_the_edit(self):
        for jobs, reads in ((arm_jobs(), "rounds no difference, tokens no difference"),
                            (arm_jobs(a={"rounds": 3}), "rounds b lower, tokens no difference")):
            with self.subTest(reads=reads):
                code, out, _, requests, written = self.settle(running([trial()], jobs))
                self.assertEqual((code, out), (0, f"trial {TRIAL} kept: {reads}\n"))
                self.assertNotIn(REVERT_ROUTE, [f"{m} {p}" for m, p, *_ in requests])
                self.assertEqual(written[0]["trials"][0]["status"], "kept")

    def test_changed_note_is_superseded(self):
        routes = {PIPELINE_GET: (200, {"stage_notes": {"spec": "A person rewrote this note."}},
                                 {"X-Pipeline-Version": "9"})}
        code, out, _, _, written = self.settle(running([trial()], arm_jobs(b={"rounds": 3})), routes)
        self.assertEqual((code, out), (0, f"trial {TRIAL} superseded: the spec note changed after the edit\n"))
        self.assertEqual(written[0]["trials"][0]["status"], "superseded")

    def test_too_few_jobs_stays_open_then_inconclusive_after_28_days(self):
        jobs = arm_jobs(count=4)
        code, out, _, _, written = self.settle(running([trial()], jobs))
        self.assertEqual((code, out), (0, f"trial {TRIAL} open: a 4 finished, b 4 finished\n"))
        self.assertEqual(written[0]["trials"][0]["status"], "open")
        self.assertEqual(written[0]["trials"][0]["arms"]["a"], arm_ids(TRIAL, "a", 4))
        old = trial(opened=stamp(days=-28))
        jobs = arm_jobs(count=4)
        code, out, _, _, written = self.settle(running([old], jobs))
        self.assertEqual((code, out), (0, f"trial {TRIAL} inconclusive after 28 days: kept the edit\n"))
        self.assertEqual(written[0]["trials"][0]["status"], "inconclusive")

    def test_failed_revert_leaves_the_trial_open(self):
        routes = {REVERT_ROUTE: (429, b"retro edit cap: 2 per scope per day")}
        code, out, _, _, written = self.settle(running([trial()], arm_jobs(b={"rounds": 3})), routes)
        self.assertEqual((code, out), (0, f"trial {TRIAL} revert failed: 429 retro edit cap: 2 per scope per day\n"))
        self.assertEqual(written[0]["trials"][0]["status"], "open")

    def test_failed_revert_after_28_days_is_revert_failed(self):
        routes = {REVERT_ROUTE: (400, b"note fails operator text check")}
        old = trial(opened=stamp(days=-28))
        code, out, _, _, written = self.settle(running([old], arm_jobs(b={"rounds": 3})), routes)
        self.assertEqual((code, out), (0, f"trial {TRIAL} revert failed after 28 days: kept the edit: "
                                          "400 note fails operator text check\n"))
        record = written[0]["trials"][0]
        self.assertEqual(record["status"], "revert-failed")
        self.assertIsNotNone(factory.stamp_of(record["settled"]))

    def test_paused_autonomy_settles_nothing(self):
        code, out, err, requests, _ = self.settle(running([trial()], arm_jobs(b={"rounds": 3}), paused=True))
        self.assertEqual((code, out), (3, ""))
        self.assertIn("autonomy is paused; no trial settled", err)
        self.assertEqual([f"{m} {p}" for m, p, *_ in requests], ["GET /api/board"])

    def test_revert_reads_the_champion_from_history_not_the_board(self):
        board = running([trial(champion="Text a board writer put on the record.")], arm_jobs(b={"rounds": 3}))
        _, _, _, requests, _ = self.settle(board)
        self.assertEqual(json.loads(next(body for m, p, _, body in requests if f"{m} {p}" == REVERT_ROUTE))["note"],
                         CHAMPION)

    def test_malformed_records_are_skipped(self):
        bad = [trial(id="other-spec-v8"), trial(project="../x", id="../x-spec-v8"),
               trial(stage="nope", id="factory-nope-v8"), trial(champion_version=None)]
        code, out, _, requests, _ = self.settle(running(bad, arm_jobs(b={"rounds": 3})))
        self.assertEqual((code, out), (0, "no open trials\n"))
        self.assertEqual(len(requests), 1)

    def test_malformed_records_do_not_crash_settle(self):
        board = running(["junk", trial(version=9, opened=5), trial(version=10, opened="garbage"), trial()],
                        arm_jobs(b={"rounds": 3}))
        code, out, err, _, written = self.settle(board)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"trial {TRIAL} reverted to version 9: rounds b higher, tokens no difference\n")
        self.assertNotIn("junk", written[0]["trials"])
        self.assertEqual({t["id"]: t["status"] for t in written[0]["trials"]},
                         {"factory-spec-v9": "open", "factory-spec-v10": "open", TRIAL: "reverted"})

    def test_no_open_trials(self):
        code, out, _, requests, _ = self.settle(running([trial(status="kept")]))
        self.assertEqual((code, out), (0, "no open trials\n"))
        self.assertEqual(len(requests), 1)

    def test_board_keeps_open_records_and_the_newest_settled_ones(self):
        old = [trial(version=n, status="kept", settled=f"2026-10-{n:02d}T09:00:00Z")
               for n in range(1, 22)]
        _, _, _, _, written = self.settle(running([*old, trial()], arm_jobs()))
        ids = [t["id"] for t in written[0]["trials"]]
        self.assertEqual(len(ids), factory.TRIAL_KEEP)
        self.assertNotIn("factory-spec-v1", ids)
        self.assertNotIn("factory-spec-v2", ids)
        self.assertIn(TRIAL, ids)

    def test_usage(self):
        code, out, err, _, _ = self.run_trial(running(), "resume")
        self.assertEqual((code, out), (2, ""))
        self.assertIn("usage: trial settle | trial resume <slug> <reason>", err)

    def test_board_keeps_unresumed_reverts(self):
        old = [trial(version=n, status="kept", settled=f"2026-10-{n:02d}T09:00:00Z")
               for n in range(2, 23)]
        locked = trial(version=1, status="reverted", settled="2026-09-01T09:00:00Z")
        resumed = dict(locked, version=0, id="factory-spec-v0", resumed="2026-09-02T09:00:00Z")
        _, _, _, _, written = self.settle(running([locked, resumed, *old, trial()], arm_jobs()))
        ids = [t["id"] for t in written[0]["trials"]]
        self.assertEqual(len(ids), factory.TRIAL_KEEP + 1)
        self.assertIn("factory-spec-v1", ids)
        self.assertNotIn("factory-spec-v0", ids)
        self.assertNotIn("factory-spec-v2", ids)
        self.assertIn(TRIAL, ids)


class TrialResumeTest(TrialWorkerCase):
    def test_second_revert_prints_the_pause_line(self):
        earlier = trial(version=5, status="reverted", settled=stamp(days=-10))
        code, out, _, _, _ = self.run_trial(running([earlier, trial()], arm_jobs(b={"rounds": 3})), "settle")
        self.assertEqual(code, 0)
        self.assertEqual(out, f"trial {TRIAL} reverted to version 9: rounds b higher, tokens no difference\n"
                              f"{PAUSE_LINE}\n")
        resumed = dict(earlier, resumed=stamp(days=-1))
        _, out, _, _, _ = self.run_trial(running([resumed, trial()], arm_jobs(b={"rounds": 3})), "settle")
        self.assertNotIn(PAUSE_LINE, out)

    def resume(self, *args, reply=(200, {"resumed": 2})):
        return self.run_trial(running(), "resume", *args, routes={RESUME_ROUTE: reply})

    def test_resume_posts_the_reason_to_the_worker(self):
        code, out, err, requests, written = self.resume("factory", f"  {REASON} ")
        self.assertEqual((code, out, err), (0, "stage-note trials resumed for factory: 2 reverts cleared\n", ""))
        self.assertEqual([(f"{m} {p}", json.loads(body)) for m, p, _, body in requests],
                         [(RESUME_ROUTE, {"instruction": REASON})])
        self.assertEqual(written, [])

    def test_resume_with_nothing_to_clear(self):
        code, out, _, _, _ = self.resume("factory", REASON, reply=(200, {"resumed": 0}))
        self.assertEqual((code, out), (0, "no unresumed reverts for factory; nothing changed\n"))

    def test_resume_needs_a_reason(self):
        code, out, err, requests, _ = self.resume("factory")
        self.assertEqual((code, out, requests), (2, "", []))
        self.assertIn("usage: trial settle | trial resume <slug> <reason>", err)

    def test_worker_refusal_exits_5(self):
        code, _, err, _, _ = self.resume("factory", REASON, reply=(400, b"instruction: links are not allowed"))
        self.assertEqual(code, 5)
        self.assertIn("instruction: links are not allowed", err)

    def test_unbound_host_gets_the_worker_reason(self):
        code, out, err, _, _ = self.resume("factory", REASON, reply=(403, factory.TRIAL_RESUME_ONLY.encode()))
        self.assertEqual((code, out), (5, ""))
        self.assertIn(factory.TRIAL_RESUME_ONLY, err)
        self.assertNotIn("Access rejected", err)

    def test_generic_403_is_access_rejected(self):
        code, out, err, _, _ = self.resume("factory", REASON, reply=(403, b"<html><body>Forbidden</body></html>"))
        self.assertEqual((code, out), (5, ""))
        self.assertIn("Access rejected", err)
        self.assertNotIn(factory.TRIAL_RESUME_ONLY, err)


if __name__ == "__main__":
    unittest.main()
