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
CLAIM = {"device": "test", "session": "tag", "since": NOW, "seen": NOW}


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
        self.set_jobs(jobs())
        api = patch.object(factory, "api", side_effect=self.api)
        api.start()
        self.addCleanup(api.stop)

    def set_jobs(self, rows):
        self.text = factory.dump_board({"updated": "", "jobs": rows})

    def api(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if method == "PUT":
            self.puts += 1
            self.text = body
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
            {"id": "091-sample-typo", "title": "Title 091-sample-typo", "idea": "Fix the header typo"},
            {"id": "092-sample-csv-name", "title": "Title 092-sample-csv-name",
             "idea": "Title 092-sample-csv-name"}])
        self.assertEqual(self.ids(out, "candidates"), ["091-sample-typo", "092-sample-csv-name", "093-sample-login"])
        self.assertEqual(self.puts, 0)

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

    def test_candidates_stop_at_twenty(self):
        self.set_jobs(jobs()[:1] + [idea(f"{n}-sample-many") for n in range(100, 125)])
        self.assertEqual(len(json.loads(self.batch(TARGET)[1])["candidates"]), 20)

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

    def test_refuses_more_than_four_riders(self):
        rows = jobs() + [idea(f"{n}-sample-more") for n in range(100, 104)]
        self.set_jobs(rows)
        with patch.dict(os.environ, {"FACTORY_SESSION": "tag"}):
            with self.assertRaises(SystemExit) as raised:
                factory.batch_cmd(TARGET, "091-sample-typo", "092-sample-csv-name", "093-sample-login",
                                  "100-sample-more", "101-sample-more")
        self.assertEqual((raised.exception.code, self.puts), (2, 0))

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
