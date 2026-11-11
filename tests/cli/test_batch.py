import io
import json
import os
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
LEAD = "090-sample-export-csv"
TARGET = f"sample/{LEAD}"
REF = {"id": LEAD, "kind": "batched-into"}
NOW = "2026-01-01T12:00:00Z"
LINE = "- [ ] sample: Name the weekly CSV weekly-<date>.csv (follow-up-of 080-sample-report)"
CLAIM = {"device": "test", "session": "tag", "since": NOW, "seen": NOW}
BAD_ID = "a job id is letters, digits, dots, dashes and underscores, like 003-rate-limiting. Nothing was written."


def idea(job_id, **extra):
    return {"id": job_id, "project": "sample", "stage": "idea", "title": f"Title {job_id}", **extra}


def jobs():
    return [
        {"id": LEAD, "project": "sample", "stage": "spec", "title": "Add CSV export", "claim": dict(CLAIM)},
        idea("091-sample-typo", idea="Fix the header typo", claim={**CLAIM, "session": "gone"}),
        idea("092-sample-csv-name"),
        idea("093-sample-login"),
    ]


class BatchTest(IsolatedTestCase):
    def setUp(self):
        self.home = Path(os.environ["XDG_CONFIG_HOME"], "factory")
        self.home.mkdir(parents=True)
        (self.home / "config.md").write_text("---\ndevice: test\n---\n")
        self.puts = 0
        self.backlog = "# Backlog\n"
        self.backlog_puts = 0
        self.on_board_put = None
        self.set_jobs(jobs())
        api = patch.object(factory, "api", side_effect=self.api)
        api.start()
        self.addCleanup(api.stop)

    def set_jobs(self, rows):
        self.text = factory.dump_board({"updated": "", "jobs": rows})

    def api(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if path == "/api/backlog":
            if method == "PUT":
                self.backlog_puts += 1
                self.backlog = body
            return 200, {"ETag": '"synthetic"'}, self.backlog
        if method == "PUT":
            self.puts += 1
            self.text = body
            hook, self.on_board_put = self.on_board_put, None
            if hook:
                hook()
        return 200, {"ETag": '"synthetic"'}, self.text

    def job(self, job_id):
        return next(j for j in json.loads(self.text)["jobs"] if j["id"] == job_id)

    def batch(self, *args):
        with patch.dict(os.environ, {"PASEO_AGENT_ID": ""}):
            return invoke(factory, ["batch", *args, "--session", "tag"])

    def refuse(self, code, message, *args):
        self.assertEqual(self.batch(*args), (code, "", f"factory.py: {message}\n"))
        self.assertEqual(self.puts, 0)

    def ids(self, out, key):
        return [j["id"] for j in json.loads(out)[key]]

    def test_lists_eligible_candidates_in_board_order(self):
        code, out, _ = self.batch(TARGET)
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out)["lead"], LEAD)
        self.assertEqual(json.loads(out)["riders"], [])
        self.assertEqual(json.loads(out)["candidates"][:2], [
            {"id": "091-sample-typo", "title": "Title 091-sample-typo", "idea": "Fix the header typo", "hold": False},
            {"id": "092-sample-csv-name", "title": "Title 092-sample-csv-name",
             "idea": "Title 092-sample-csv-name", "hold": False}])
        self.assertEqual(self.ids(out, "candidates"), ["091-sample-typo", "092-sample-csv-name", "093-sample-login"])
        self.assertEqual(self.puts, 0)

    def test_candidates_mark_hold(self):
        rows = jobs()
        rows[2]["refs"] = [{"id": "080-sample-report", "kind": "follow-up-of"}]
        self.set_jobs(rows)
        code, out, _ = self.batch(TARGET)
        self.assertEqual(code, 0)
        hold = {c["id"]: c["hold"] for c in json.loads(out)["candidates"]}
        self.assertEqual((hold["092-sample-csv-name"], hold["093-sample-login"]), (True, False))

    def test_excludes_each_ineligible_idea(self):
        others = [
            idea("100-sample-other", project="elsewhere"),
            {**idea("101-sample-gated"), "needs": "pick one"},
            idea("102-sample-paused", paused={"at": NOW}),
            idea("103-sample-closing", close={"at": NOW}),
            idea("104-sample-images", idea_images=["a" * 64 + ".png"]),
            idea("105-sample-tracer", tracer={"scope": "approved"}),
            idea("106-sample-rework", backlog={"decision": "rework"}),
            idea("107-sample-held", claim={"device": "other", "session": "x", "since": NOW, "seen": factory.now()}),
            idea("108-sample-batched", refs=[{"id": "050-sample-x", "kind": "batched-into"}]),
            {**idea("109-sample-build"), "stage": "build"},
        ]
        self.set_jobs(jobs()[:1] + others + [idea("110-sample-ok")])
        code, out, _ = self.batch(TARGET)
        self.assertEqual(self.ids(out, "candidates"), ["110-sample-ok"])

    def test_problem_reasons(self):
        data = {"jobs": [idea("050-sample-lead")]}
        cases = [
            (idea("1-a", project="elsewhere"), "another project"),
            ({**idea("1-a"), "stage": "build"}, "not an idea"),
            ({**idea("1-a"), "needs": "pick"}, "waits on a human"),
            (idea("1-a", paused={"at": NOW}), "paused"),
            (idea("1-a", close={"at": NOW}), "closing"),
            (idea("1-a", idea_images=["x"]), "has pasted images"),
            (idea("1-a", tracer={}), "follows Tracer"),
            (idea("1-a", backlog={"decision": "rework"}), "marked for rework"),
            (idea("1-a", claim={"device": "d", "session": "s", "since": NOW, "seen": factory.now()}),
             "held by d session s"),
            (idea("1-a", refs=[{"id": "050-sample-lead", "kind": "batched-into"}]),
             "already batched into 050-sample-lead"),
            (idea("050-sample-lead"), None),
        ]
        for j, reason in cases[:-1]:
            with self.subTest(reason=reason):
                self.assertEqual(factory.batch_problem(data, j, "sample", "test", "tag", 120), reason)
        data["jobs"].append(idea("2-b", refs=[{"id": "050-sample-lead", "kind": "batched-into"}]))
        self.assertEqual(factory.batch_problem(data, cases[-1][0], "sample", "test", "tag", 120), "leads a batch")
        self.assertIsNone(factory.batch_problem({"jobs": []}, idea("3-c"), "sample", "test", "tag", 120))

    def test_candidates_stop_at_forty(self):
        self.set_jobs(jobs()[:1] + [idea(f"{n}-sample-many") for n in range(100, 145)])
        self.assertEqual(len(json.loads(self.batch(TARGET)[1])["candidates"]), 40)

    def test_attach_moves_riders_in_one_write(self):
        code, out, _ = self.batch(TARGET, "091-sample-typo", "092-sample-csv-name")
        self.assertEqual((code, self.puts), (0, 1))
        for rider in ("091-sample-typo", "092-sample-csv-name"):
            j = self.job(rider)
            self.assertEqual((j["stage"], j["refs"], j["note"]), ("spec", [REF], f"batched into {LEAD}"))
            self.assertNotIn("claim", j)
        self.assertEqual(self.job("093-sample-login")["stage"], "idea")
        result = json.loads(out)
        self.assertEqual(self.ids(out, "riders"), ["091-sample-typo", "092-sample-csv-name"])
        self.assertEqual(result["riders"][0]["idea"], "Fix the header typo")
        self.assertEqual(self.ids(out, "candidates"), ["093-sample-login"])
        self.assertEqual(self.backlog_puts, 0)

    def set_backlog(self):
        self.backlog = ("# Backlog\n\n"
                        f"{LINE}\n"
                        "  Use the ISO date.\n"
                        "- [ ] elsewhere: Add SSO login\n")
        return f"backlog:{factory.backlog_key(LINE)}"

    def test_lists_backlog_lines_after_ideas(self):
        key = self.set_backlog()
        code, out, _ = self.batch(TARGET)
        self.assertEqual(code, 0)
        self.assertEqual(self.ids(out, "candidates"),
                         ["091-sample-typo", "092-sample-csv-name", "093-sample-login", key])
        self.assertEqual(json.loads(out)["candidates"][-1],
                         {"id": key, "title": "Name the weekly CSV weekly-<date>.csv",
                          "idea": "Name the weekly CSV weekly-<date>.csv\n\nUse the ISO date."})

    def test_attach_backlog_line_adds_rider_and_checks_it_off(self):
        key = self.set_backlog()
        code, out, _ = self.batch(TARGET, key)
        self.assertEqual((code, self.puts, self.backlog_puts), (0, 1, 1))
        j = self.job("094-name-the-weekly")
        self.assertEqual((j["project"], j["stage"], j["note"], j["refs"]),
                         ("sample", "spec", f"batched into {LEAD}",
                          [{"id": "080-sample-report", "kind": "follow-up-of"}, REF]))
        self.assertEqual((j["title"], j["idea"]), ("Name the weekly CSV weekly-<date>.csv",
                                                   "Name the weekly CSV weekly-<date>.csv\n\nUse the ISO date."))
        self.assertIn("- [x] sample: Name the weekly CSV weekly-<date>.csv (follow-up-of 080-sample-report) "
                      "(094-name-the-weekly)\n  Use the ISO date.\n", self.backlog)
        self.assertIn("- [ ] elsewhere: Add SSO login\n", self.backlog)
        self.assertEqual(self.ids(out, "riders"), ["094-name-the-weekly"])
        self.assertNotIn(key, self.ids(out, "candidates"))

    def test_unknown_backlog_key_writes_nothing(self):
        self.set_backlog()
        self.refuse(4, "no unchecked sample backlog line backlog:000000000000. Nothing was written.",
                    TARGET, "backlog:000000000000")
        self.assertEqual(self.backlog_puts, 0)

    def test_changed_line_drops_its_rider(self):
        key = self.set_backlog()

        def take():
            self.backlog = self.backlog.replace(LINE, LINE + " (095-sample-taken)")
        self.on_board_put = take
        code, out, err = self.batch(TARGET, key)
        self.assertEqual(code, 0)
        self.assertEqual(err, f"factory.py: {key} changed before batch could check it off; "
                              "removed 094-name-the-weekly\n")
        self.assertNotIn("094-name-the-weekly", [j["id"] for j in json.loads(self.text)["jobs"]])
        self.assertEqual((json.loads(out)["riders"], self.backlog_puts), ([], 0))

    def test_attach_refusals_write_nothing(self):
        self.refuse(4, "no job 999-sample-none on the board", "sample/999-sample-none")
        self.refuse(4, "no job 999-sample-none on the board", TARGET, "999-sample-none")
        self.set_jobs(jobs() + [idea("100-sample-other", project="elsewhere")])
        self.refuse(2, "cannot batch 100-sample-other: another project. Nothing was written.", TARGET,
                    "091-sample-typo", "100-sample-other")
        self.refuse(2, "cannot batch 090-sample-export-csv: not an idea. Nothing was written.", TARGET, LEAD)
        rows = jobs()
        rows[0]["claim"]["session"] = "other"
        self.set_jobs(rows)
        self.refuse(6, f"{LEAD} is not held by this session. Nothing was written.", TARGET, "091-sample-typo")
        rows[0].update(claim=dict(CLAIM), stage="build")
        self.set_jobs(rows)
        self.refuse(2, f"batch runs while {LEAD} is at spec, not build", TARGET, "091-sample-typo")
        rows[0].update(stage="spec", tracer={"scope": "approved"})
        self.set_jobs(rows)
        self.refuse(2, f"cannot batch into {LEAD}: follows Tracer", TARGET, "091-sample-typo")

    def test_attach_refuses_bad_picked_ids(self):
        self.set_jobs(jobs() + [idea("bad id")])
        self.refuse(2, f"bad job id 'bad id': {BAD_ID}", TARGET, "bad id")
        self.refuse(2, f"bad job id '../x': {BAD_ID}", TARGET, "../x")

    def test_attaches_twenty_riders(self):
        more = [f"{n}-sample-more" for n in range(100, 120)]
        self.set_jobs(jobs() + [idea(i) for i in more])
        code, out, _ = self.batch(TARGET, *more)
        self.assertEqual((code, self.ids(out, "riders")), (0, more))

    def test_refuses_more_than_twenty_riders(self):
        more = [f"{n}-sample-more" for n in range(100, 118)]
        self.set_jobs(jobs() + [idea(i) for i in more])
        with patch.dict(os.environ, {"FACTORY_SESSION": "tag"}):
            with patch("sys.stderr", new_callable=io.StringIO) as err, self.assertRaises(SystemExit) as raised:
                factory.batch_cmd(TARGET, "091-sample-typo", "092-sample-csv-name", "093-sample-login", *more)
        self.assertEqual((raised.exception.code, self.puts), (2, 0))
        self.assertEqual(err.getvalue(), "factory.py: a batch holds at most 20 riders\n")

    def test_cli_accepts_twenty_idea_ids(self):
        self.assertEqual(factory.COMMANDS["batch"][1:], (1, 21))

    def patch_lead(self, patch_body):
        factory.cas(self.home, "/api/board", lambda text: self.apply(text, patch_body))

    def apply(self, text, body):
        data = factory.load_board(text)
        factory.patch_job(data, LEAD, body)
        return factory.dump_board(data), None

    def test_lead_stage_and_pr_mirror_onto_riders(self):
        self.batch(TARGET, "091-sample-typo", "092-sample-csv-name")
        self.patch_lead({"stage": "build"})
        self.assertEqual(self.job("091-sample-typo")["stage"], "build")
        self.patch_lead({"stage": "pr-open", "pr": "https://example.test/pr/1"})
        for rider in ("091-sample-typo", "092-sample-csv-name"):
            j = self.job(rider)
            self.assertEqual((j["stage"], j["pr"]), ("pr-open", "https://example.test/pr/1"))
            self.assertNotIn("claim", j)
            self.assertNotIn("needs", j)
        self.patch_lead({"stage": "merged"})
        j = self.job("091-sample-typo")
        self.assertEqual((j["stage"], j["done"]), ("merged", self.job(LEAD)["done"]))
        self.assertEqual(self.job("093-sample-login")["stage"], "idea")

    def test_lead_needs_mirror_onto_riders_at_needs_human(self):
        self.batch(TARGET, "091-sample-typo")
        self.patch_lead({"stage": "needs-human", "needs": "Pick the CSV date format in needs.md"})
        j = self.job("091-sample-typo")
        self.assertEqual((j["stage"], j["needs"]), ("needs-human", "Pick the CSV date format in needs.md"))
        self.patch_lead({"needs": "Pick the CSV file name in needs.md"})
        self.assertEqual(self.job("091-sample-typo")["needs"], "Pick the CSV file name in needs.md")
        self.patch_lead({"stage": "build"})
        self.assertNotIn("needs", self.job("091-sample-typo"))

    def test_needs_only_patch_skips_unchanged_riders(self):
        self.batch(TARGET, "091-sample-typo")
        self.patch_lead({"stage": "build"})
        before = self.job("091-sample-typo")["updated"]
        with patch.object(factory, "now", return_value="2026-01-01T13:00:00Z"):
            self.patch_lead({"needs": None})
        self.assertEqual(self.job("091-sample-typo")["updated"], before)

    def test_closed_lead_clears_rider_needs(self):
        self.batch(TARGET, "091-sample-typo")
        self.patch_lead({"stage": "needs-human", "needs": "Pick the CSV date format in needs.md"})
        self.patch_lead({"stage": "closed"})
        j = self.job("091-sample-typo")
        self.assertEqual(j["stage"], "idea")
        self.assertNotIn("needs", j)

    def test_closed_lead_returns_riders_to_idea(self):
        self.batch(TARGET, "091-sample-typo")
        self.patch_lead({"stage": "closed"})
        j = self.job("091-sample-typo")
        self.assertEqual((j["stage"], j["note"]), ("idea", f"unbatched: {LEAD} closed"))
        self.assertNotIn("refs", j)

    def test_closed_lead_keeps_other_refs(self):
        rows = jobs()
        rows[1]["refs"] = [{"id": "001-sample-old", "kind": "follow-up-of"}]
        self.set_jobs(rows)
        self.batch(TARGET, "091-sample-typo")
        self.patch_lead({"stage": "closed"})
        self.assertEqual(self.job("091-sample-typo")["refs"], [{"id": "001-sample-old", "kind": "follow-up-of"}])

    def test_closed_lead_drops_non_dict_refs(self):
        rows = jobs()
        rows[1]["refs"] = ["junk"]
        self.set_jobs(rows)
        self.batch(TARGET, "091-sample-typo")
        self.patch_lead({"stage": "closed"})
        j = self.job("091-sample-typo")
        self.assertEqual(j["stage"], "idea")
        self.assertNotIn("refs", j)
        self.assertEqual(self.job(LEAD)["stage"], "closed")

    def test_finished_riders_are_left_alone(self):
        self.batch(TARGET, "091-sample-typo", "092-sample-csv-name")
        rows = json.loads(self.text)["jobs"]
        rows[1]["stage"] = "merged"
        rows[2]["stage"] = "closed"
        self.set_jobs(rows)
        self.patch_lead({"stage": "build"})
        self.assertEqual([self.job("091-sample-typo")["stage"], self.job("092-sample-csv-name")["stage"]],
                         ["merged", "closed"])

    def test_stuck_and_inflight_skip_riders(self):
        self.batch(TARGET, "091-sample-typo")
        data = json.loads(self.text)
        data["jobs"][0]["updated"] = data["jobs"][1]["updated"] = factory.now()
        self.assertEqual(factory.inflight_count(data, "sample", 120), 1)
        for j in data["jobs"][:2]:
            j["updated"] = "2020-01-01T00:00:00Z"
        data["jobs"][0]["claim"]["seen"] = "2020-01-01T00:00:00Z"
        self.text = json.dumps(data)
        with patch.object(factory, "read_paths", return_value={"sample": str(self.home)}), \
                patch.object(factory, "paseo_server", return_value=None):
            code, out, _ = invoke(factory, ["stuck", "--limit", "5"])
        self.assertEqual(code, 0)
        self.assertEqual([j["id"] for j in json.loads(out)], [LEAD])

    def stack(self, *args):
        with patch.dict(os.environ, {"PASEO_AGENT_ID": ""}):
            return invoke(factory, ["stack", *args, "--session", "tag"])

    def refuse_stack(self, code, message, *args):
        self.assertEqual(self.stack(*args), (code, "", f"factory.py: {message}\n"))
        self.assertEqual(self.puts, 0)

    def test_stack_marks_one_rider(self):
        self.batch(TARGET, "091-sample-typo", "092-sample-csv-name")
        self.puts = 0
        code, out, _ = self.stack(TARGET, "092-sample-csv-name")
        self.assertEqual((code, self.puts), (0, 1))
        self.assertEqual(json.loads(out), {"lead": LEAD, "stacked": "092-sample-csv-name"})
        j = self.job("092-sample-csv-name")
        self.assertEqual((j["stacked"], j["note"], j["stage"]), (True, f"stacked PR on {LEAD}", "spec"))
        self.assertNotIn("stacked", self.job("091-sample-typo"))
        code, out, _ = self.stack(TARGET, "092-sample-csv-name")
        self.assertEqual((code, self.puts), (0, 1))
        self.assertEqual(json.loads(out), {"lead": LEAD, "stacked": "092-sample-csv-name"})

    def test_stack_refusals_write_nothing(self):
        self.batch(TARGET, "091-sample-typo", "092-sample-csv-name")
        batched = json.loads(self.text)["jobs"]
        self.puts = 0
        self.refuse_stack(2, "bad job id '../x'. Nothing was written.", TARGET, "../x")
        self.refuse_stack(2, "bad job id 'bad id'. Nothing was written.", TARGET, "bad id")
        self.refuse_stack(4, "no job 999-sample-none on the board", "sample/999-sample-none", "092-sample-csv-name")
        self.refuse_stack(4, "no job 999-sample-none on the board", TARGET, "999-sample-none")
        rows = json.loads(json.dumps(batched))
        rows[0]["claim"]["session"] = "other"
        self.set_jobs(rows)
        self.refuse_stack(6, f"{LEAD} is not held by this session. Nothing was written.", TARGET,
                          "092-sample-csv-name")
        rows[0].update(claim=dict(CLAIM), stage="build")
        self.set_jobs(rows)
        self.refuse_stack(2, f"stack runs while {LEAD} is at spec, not build", TARGET, "092-sample-csv-name")
        rows[0]["stage"] = "spec"
        self.set_jobs(rows)
        self.refuse_stack(2, f"cannot stack 093-sample-login: not batched into {LEAD}. Nothing was written.",
                          TARGET, "093-sample-login")
        rows[1]["stacked"] = True
        self.set_jobs(rows)
        self.refuse_stack(2, f"cannot stack 092-sample-csv-name: 091-sample-typo is already stacked on {LEAD}. "
                          "Nothing was written.", TARGET, "092-sample-csv-name")

    def test_cli_registers_stack(self):
        self.assertEqual(factory.COMMANDS["stack"], (factory.stack_cmd, 2, 2))

    def stacked_batch(self):
        self.batch(TARGET, "091-sample-typo", "092-sample-csv-name")
        self.stack(TARGET, "092-sample-csv-name")

    def test_stacked_rider_keeps_its_own_pr(self):
        self.stacked_batch()
        self.patch_lead({"stage": "build"})
        self.assertEqual(self.job("092-sample-csv-name")["stage"], "build")
        self.patch_lead({"stage": "pr-open", "pr": "https://example.test/pr/41"})
        self.assertEqual(self.job("091-sample-typo")["pr"], "https://example.test/pr/41")
        j = self.job("092-sample-csv-name")
        self.assertEqual(j["stage"], "pr-open")
        self.assertNotIn("pr", j)
        rows = json.loads(self.text)["jobs"]
        rows[2]["pr"] = "https://example.test/pr/42"
        self.set_jobs(rows)
        self.patch_lead({"stage": "needs-human"})
        j = self.job("092-sample-csv-name")
        self.assertEqual((j["stage"], j["pr"]), ("needs-human", "https://example.test/pr/42"))

    def test_stacked_rider_takes_lead_needs_at_needs_human(self):
        self.stacked_batch()
        self.patch_lead({"stage": "needs-human", "needs": "Pick the CSV date format in needs.md"})
        j = self.job("092-sample-csv-name")
        self.assertEqual((j["stage"], j["needs"]), ("needs-human", "Pick the CSV date format in needs.md"))
        self.patch_lead({"stage": "build"})
        j = self.job("092-sample-csv-name")
        self.assertEqual(j["stage"], "build")
        self.assertNotIn("needs", j)

    def test_merged_lead_releases_stacked_rider(self):
        rows = jobs()
        rows[2]["refs"] = ["junk", {"id": "001-sample-old", "kind": "follow-up-of"}]
        self.set_jobs(rows)
        self.stacked_batch()
        self.patch_lead({"stage": "pr-open", "pr": "https://example.test/pr/41", "automerge": True})
        rows = json.loads(self.text)["jobs"]
        rows[2]["pr"] = "https://example.test/pr/42"
        self.set_jobs(rows)
        self.patch_lead({"stage": "merged"})
        self.assertEqual(self.job("091-sample-typo")["stage"], "merged")
        j = self.job("092-sample-csv-name")
        self.assertEqual((j["stage"], j["pr"], j["automerge"], j["note"]),
                         ("pr-open", "https://example.test/pr/42", True, f"stacked on {LEAD}, which merged"))
        self.assertEqual(j["refs"], [{"id": "001-sample-old", "kind": "follow-up-of"},
                                     {"id": LEAD, "kind": "follow-up-of"}])
        self.assertNotIn("stacked", j)
        self.assertNotIn("escalated", j)
        self.assertIsNone(factory.lead_of(j))

    def test_stacked_rider_needs_cleared_on_merge_from_needs_human(self):
        self.stacked_batch()
        rows = json.loads(self.text)["jobs"]
        rows[2]["pr"] = "https://example.test/pr/42"
        self.set_jobs(rows)
        self.patch_lead({"stage": "needs-human", "needs": "Pick the CSV date format in needs.md"})
        self.assertEqual(self.job("092-sample-csv-name")["needs"], "Pick the CSV date format in needs.md")
        self.patch_lead({"stage": "merged"})
        self.assertNotIn("needs", self.job("092-sample-csv-name"))

    def test_merged_lead_unbatches_stacked_rider_without_pr(self):
        self.stacked_batch()
        self.patch_lead({"stage": "pr-open", "pr": "https://example.test/pr/41", "escalated": True})
        self.patch_lead({"stage": "merged"})
        j = self.job("092-sample-csv-name")
        self.assertEqual((j["stage"], j["note"]), ("idea", f"unbatched: {LEAD} merged before its stacked PR opened"))
        for key in ("refs", "stacked", "pr", "escalated"):
            self.assertNotIn(key, j)

    def test_closed_lead_unbatches_stacked_rider(self):
        self.stacked_batch()
        self.patch_lead({"stage": "pr-open", "pr": "https://example.test/pr/41"})
        rows = json.loads(self.text)["jobs"]
        rows[2]["pr"] = "https://example.test/pr/42"
        self.set_jobs(rows)
        self.patch_lead({"stage": "closed"})
        j = self.job("092-sample-csv-name")
        self.assertEqual((j["stage"], j["note"]),
                         ("idea", f"unbatched: {LEAD} closed; stacked PR https://example.test/pr/42 is still open"))
        for key in ("refs", "stacked", "pr"):
            self.assertNotIn(key, j)
        self.assertEqual(self.job("091-sample-typo")["note"], f"unbatched: {LEAD} closed")
