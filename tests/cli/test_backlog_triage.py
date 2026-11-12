import contextlib
import io
import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

ITEM = "sample: Add CSV export to reports"
QUEUE = {"decision": "queue", "confidence": 86, "threshold": 80, "reason": "Rated by clef-flash against the project rules.",
         "model": "@cf/cloudflare/clef-flash"}
LINK = f"link: https://factory.example/#/backlog/29a3c3690fe4\n"
WARN = "factory.py: backlog triage failed ({}); the item waits in the backlog for review\n"


class TriageFixture(IsolatedTestCase):
    def setUp(self):
        self.module = load_runtime()
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.dir, ignore_errors=True))
        self.files = {"/api/backlog": "# Backlog\n",
                      "/api/board": json.dumps({"updated": "", "jobs": [{"id": "003-old-job", "stage": "done"}]})}
        self.triage = (200, json.dumps(QUEUE))
        self.posts = []
        self.batch = (404, "")
        self.batches = []
        self.after_board = None
        self.cas_down = set()
        self.views = {"sample": (200, {"triage": {"enabled": True, "threshold": 80}})}
        self.gets = []
        self.reads = []
        self.backlog_gets = 0
        self.stored = {}
        self.read_status = 200

        def api(home, method, path, body=None, ok=(200,), headers=None, raw=False):
            if (method, path) == ("GET", "/projects.json"):
                return 200, {}, json.dumps([{"slug": "sample"}, {"slug": "beta"}])
            if (method, path) == ("GET", "/api/backlog"):
                self.assertEqual(ok, (200, 404))
                self.backlog_gets += 1
                text = self.files.get(path)
                return (404, {}, "") if text is None else (200, {}, text)
            if method == "GET" and path.startswith("/api/pipeline/effective/"):
                self.assertEqual(ok, (200, 404, 409))
                self.gets.append(path)
                status, view = self.views[path.rsplit("/", 1)[1]]
                if status not in ok:
                    self.module.fail(f"GET {path} returned {status}", 5)
                return status, {}, view if isinstance(view, str) else json.dumps(view)
            if (method, path) == ("POST", "/api/backlog/triage/verdicts"):
                self.assertEqual(ok, (200, 400, 404))
                items = json.loads(body)["items"]
                self.reads.append(items)
                if any(len(item) > 2000 for item in items):
                    return 400, {}, "items[2]: must be one line of at most 2000 characters"
                if self.read_status == 404:
                    return 404, {}, ""
                return 200, {}, json.dumps({"verdicts": [self.stored.get(item) for item in items]})
            if (method, path) == ("POST", "/api/backlog/triage/batch"):
                self.assertEqual(ok, (200, 400, 404, 409, 502, 503))
                self.assertEqual(headers, {"Content-Type": "application/json"})
                self.batches.append(json.loads(body)["items"])
                batch = self.batch(self.batches[-1]) if callable(self.batch) else self.batch
                if isinstance(batch, Exception):
                    self.module.fail("can't reach https://factory.example.test/api/backlog/triage/batch: timed out", 5)
                status, text = batch
                if status not in ok:
                    self.module.fail(f"POST returned {status}", 5)
                return status, {}, text
            self.assertEqual((method, path), ("POST", "/api/backlog/triage"))
            self.assertEqual(headers, {"Content-Type": "application/json"})
            self.posts.append(json.loads(body))
            triage = self.triage(self.posts[-1]["item"]) if callable(self.triage) else self.triage
            if isinstance(triage, Exception):
                self.module.fail("can't reach https://factory.example.test/api/backlog/triage: timed out", 5)
            status, text = triage
            if status not in ok:
                self.module.fail(f"POST returned {status}", 5)
            return status, {}, text

        def cas(home, path, change):
            if path in self.cas_down:
                self.module.fail(f"can't reach https://factory.example.test{path}: timed out", 5)
            new, result = change(self.files.get(path))
            if new is not None:
                self.files[path] = new
            if path == "/api/board" and self.after_board:
                self.after_board()
                self.after_board = None
            return result

        for target, fake in (("factory_home", lambda: self.dir), ("api_url", lambda home: "https://factory.example"),
                             ("api", api), ("cas", cas)):
            patcher = patch.object(self.module, target, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def board(self):
        return json.loads(self.files["/api/board"])["jobs"]

    def add(self, item=ITEM, *extra):
        return invoke(self.module, ["backlog", "add", item, *extra])

    def cached(self):
        path = self.dir / "backlog-triage.json"
        return json.loads(path.read_text(encoding="utf-8"))["lines"] if path.exists() else None


class BacklogTriageTests(TriageFixture):
    def test_queue_writes_the_idea_then_checks_the_line_off(self):
        (self.dir / "jobs" / "sample" / "007-local-only").mkdir(parents=True)
        code, out, err = self.add()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"added {ITEM}\n{LINK}queued {ITEM} as 008-add-csv-export: confidence 86% (threshold 80%)\n")
        self.assertEqual(self.posts, [{"item": ITEM}])
        job = self.board()[-1]
        at = job["updated"]
        self.assertEqual(job, {
            "id": "008-add-csv-export", "project": "sample", "title": "Add CSV export to reports",
            "stage": "idea", "updated": at,
            "idea": "Add CSV export to reports\n\nQueued by backlog triage: confidence 86% (threshold 80%). "
                    "Rated by clef-flash against the project rules.",
            "backlog": {"line": f"- [ ] {ITEM}", "decision": "approved", "at": at, "by": "triage",
                        "triage": {"model": QUEUE["model"], "confidence": 86, "threshold": 80,
                                   "reason": "Rated by clef-flash against the project rules."}},
        })
        self.assertEqual(self.files["/api/backlog"], f"# Backlog\n- [x] {ITEM} (008-add-csv-export)\n")
        self.assertIsNone(self.cached())

    def test_queue_keeps_refs_and_sends_the_context(self):
        item = ITEM + " (sub-job-of 002-reports)"
        path = self.dir / "context.md"
        path.write_text("Recommend: approve\nSmall.\n", encoding="utf-8")
        code, out, _ = self.add(item, "--context", str(path))
        self.assertEqual(code, 0)
        self.assertEqual(self.posts, [{"item": item, "context": "Recommend: approve\nSmall."}])
        job = self.board()[-1]
        self.assertEqual((job["id"], job["title"]), ("004-add-csv-export", "Add CSV export to reports"))
        self.assertEqual(job["refs"], [{"id": "002-reports", "kind": "sub-job-of"}])
        self.assertTrue(job["idea"].startswith("Add CSV export to reports\n\nContext from the backlog:\n"
                                               "Recommend: approve\nSmall.\n\nQueued by backlog triage:"))
        self.assertEqual(self.files["/api/backlog"].split("\n")[1], f"- [x] {item} (004-add-csv-export)")

    def test_queue_copies_the_context_into_the_idea(self):
        path = self.dir / "context.md"
        path.write_text("Recommend: approve\nUsers ask for CSV weekly.\n", encoding="utf-8")
        code, _, _ = self.add(ITEM, "--context", str(path))
        self.assertEqual(code, 0)
        self.assertEqual(self.board()[-1]["idea"],
                         "Add CSV export to reports\n\nContext from the backlog:\n"
                         "Recommend: approve\nUsers ask for CSV weekly.\n\n"
                         "Queued by backlog triage: confidence 86% (threshold 80%). "
                         "Rated by clef-flash against the project rules.")

    def test_queue_cuts_a_long_context_to_the_idea_limit(self):
        path = self.dir / "context.md"
        path.write_text("Recommend: approve\n" + "y" * 5000 + "\n", encoding="utf-8")
        code, _, _ = self.add(ITEM, "--context", str(path))
        self.assertEqual(code, 0)
        idea = self.board()[-1]["idea"]
        self.assertEqual(len(idea), 4000)
        self.assertTrue(idea.startswith("Add CSV export to reports\n\nContext from the backlog:\nRecommend: approve\nyyy"))
        self.assertTrue(idea.endswith("y\n[context cut to fit the 4000-character idea limit]\n\n"
                                      "Queued by backlog triage: confidence 86% (threshold 80%). "
                                      "Rated by clef-flash against the project rules."))
        self.assertEqual(self.module.idea_with_context("a", "x" * 4000, "b" * 3950), "a" + "b" * 3950)
        self.assertEqual(self.module.idea_with_context("a", None, "b"), "ab")

    def test_queue_skips_a_dotted_suffix_id(self):
        code, _, _ = self.add(ITEM + " (follow-up-of a..b)")
        self.assertEqual(code, 0)
        job = self.board()[-1]
        self.assertEqual(job["title"], "Add CSV export to reports")
        self.assertNotIn("refs", job)

    def test_hold_and_off_change_nothing_after_the_append(self):
        line = f"# Backlog\n- [ ] {ITEM}\n"
        board = self.files["/api/board"]
        for reply, printed, cached in (
            ({"decision": "hold", "confidence": 62, "threshold": 80, "reason": "Rated by clef-flash against the project rules.",
              "model": QUEUE["model"]},
             f"held {ITEM} for review: confidence 62% (threshold 80%): Rated by clef-flash against the project rules.\n", [f"- [ ] {ITEM}"]),
            ({"decision": "hold", "confidence": None, "threshold": 80, "reason": "its author recommends deny",
              "model": None},
             f"held {ITEM} for review: its author recommends deny\n", [f"- [ ] {ITEM}"]),
            ({"decision": "off"}, "", None),
        ):
            with self.subTest(reply=reply):
                self.files["/api/backlog"] = "# Backlog\n"
                (self.dir / "backlog-triage.json").unlink(missing_ok=True)
                self.triage = (200, json.dumps(reply))
                self.assertEqual(self.add(), (0, f"added {ITEM}\n{LINK}" + printed, ""))
                self.assertEqual(self.files["/api/backlog"], line)
                self.assertEqual(self.files["/api/board"], board)
                self.assertEqual(self.cached(), cached)

    def test_add_strips_trailing_spaces(self):
        self.triage = (200, json.dumps(HOLD))
        code, out, err = self.add(ITEM + "  ")
        self.assertEqual((code, err), (0, ""))
        self.assertTrue(out.startswith(f"added {ITEM}\n{LINK}"))
        self.assertEqual(self.files["/api/backlog"], f"# Backlog\n- [ ] {ITEM}\n")
        self.assertEqual(self.posts, [{"item": ITEM}])
        self.assertEqual(self.cached(), [f"- [ ] {ITEM}"])

    def test_failures_warn_and_exit_0_with_the_line_waiting(self):
        board = self.files["/api/board"]
        for triage, reason in (
            ((404, "not found"), None),
            ((400, "item: bad"), "400"),
            ((409, "stored pipeline configuration no longer validates"), "409"),
            ((502, "the triage model failed"), "502"),
            ((503, "backlog triage needs the Workers AI binding"), "503"),
            ((500, "boom"), "POST returned 500"),
            (OSError("timed out"), "can't reach https://factory.example.test/api/backlog/triage: timed out"),
            ((200, "not json"), "malformed reply"),
            ((200, json.dumps({**QUEUE, "confidence": "86"})), "malformed reply"),
            ((200, json.dumps({**QUEUE, "decision": "maybe"})), "malformed reply"),
        ):
            with self.subTest(triage=triage):
                self.files["/api/backlog"] = "# Backlog\n"
                self.triage = triage
                code, out, err = self.add()
                self.assertEqual((code, out), (0, f"added {ITEM}\n{LINK}"))
                self.assertEqual(err, "" if reason is None else WARN.format(reason))
                self.assertEqual(self.files["/api/backlog"], f"# Backlog\n- [ ] {ITEM}\n")
                self.assertEqual(self.files["/api/board"], board)
                self.assertEqual(self.cached(), [f"- [ ] {ITEM}"] if reason == "400" else None)
                (self.dir / "backlog-triage.json").unlink(missing_ok=True)

    def test_a_lost_line_drops_the_unclaimed_row(self):
        def claim_line():
            self.files["/api/backlog"] = f"# Backlog\n- [x] {ITEM} (009-other)\n"
        self.after_board = claim_line
        code, out, err = self.add()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"added {ITEM}\n{LINK}kept {ITEM} in the backlog: its line changed before triage finished\n")
        self.assertEqual([j["id"] for j in self.board()], ["003-old-job"])
        self.assertEqual(self.files["/api/backlog"], f"# Backlog\n- [x] {ITEM} (009-other)\n")

    def test_a_lost_line_keeps_a_row_someone_claimed(self):
        def claim_both():
            data = json.loads(self.files["/api/board"])
            data["jobs"][-1]["claim"] = {"device": "other"}
            self.files["/api/board"] = json.dumps(data)
            self.files["/api/backlog"] = "# Backlog\n"
        self.after_board = claim_both
        code, out, _ = self.add()
        self.assertEqual(code, 0)
        self.assertEqual([j["id"] for j in self.board()], ["003-old-job", "004-add-csv-export"])

    def test_a_failed_board_write_warns_and_exits_0(self):
        board = self.files["/api/board"]
        self.cas_down = {"/api/board"}
        self.assertEqual(self.add(), (0, f"added {ITEM}\n{LINK}",
                                      "factory.py: can't reach https://factory.example.test/api/board: timed out\n"
                                      f"factory.py: backlog triage could not queue {ITEM}; "
                                      "the item waits in the backlog for review\n"))
        self.assertEqual(self.files["/api/backlog"], f"# Backlog\n- [ ] {ITEM}\n")
        self.assertEqual(self.files["/api/board"], board)

    def test_a_failed_check_off_drops_the_row_and_exits_0(self):
        self.after_board = lambda: self.cas_down.add("/api/backlog")
        self.assertEqual(self.add(), (0, f"added {ITEM}\n{LINK}",
                                      "factory.py: can't reach https://factory.example.test/api/backlog: timed out\n"
                                      f"factory.py: backlog triage could not check off {ITEM}; it removed "
                                      "004-add-csv-export and the item waits in the backlog for review\n"))
        self.assertEqual([j["id"] for j in self.board()], ["003-old-job"])
        self.assertEqual(self.files["/api/backlog"], f"# Backlog\n- [ ] {ITEM}\n")

    def test_a_failed_check_off_and_drop_names_the_row(self):
        self.after_board = lambda: self.cas_down.update({"/api/backlog", "/api/board"})
        self.assertEqual(self.add(), (0, f"added {ITEM}\n{LINK}",
                                      "factory.py: can't reach https://factory.example.test/api/backlog: timed out\n"
                                      "factory.py: can't reach https://factory.example.test/api/board: timed out\n"
                                      f"factory.py: backlog triage queued {ITEM} as 004-add-csv-export but could not "
                                      "check off its line; check off the line or remove 004-add-csv-export by hand\n"))
        self.assertEqual([j["id"] for j in self.board()], ["003-old-job", "004-add-csv-export"])
        self.assertEqual(self.files["/api/backlog"], f"# Backlog\n- [ ] {ITEM}\n")
        self.assertEqual(self.cached(), [f"- [ ] {ITEM}"])

    def test_a_failed_drop_on_a_changed_line_names_the_row(self):
        def claim_line():
            self.files["/api/backlog"] = f"# Backlog\n- [x] {ITEM} (009-other)\n"
            self.cas_down.add("/api/board")
        self.after_board = claim_line
        code, out, err = self.add()
        self.assertEqual(code, 0)
        self.assertEqual(out, f"added {ITEM}\n{LINK}kept {ITEM} in the backlog: its line changed before triage finished\n")
        self.assertEqual(err, "factory.py: can't reach https://factory.example.test/api/board: timed out\n"
                              f"factory.py: backlog triage kept {ITEM} in the backlog but could not remove "
                              "004-add-csv-export; remove 004-add-csv-export by hand\n")
        self.assertEqual([j["id"] for j in self.board()], ["003-old-job", "004-add-csv-export"])


PDF = "- [ ] sample: Add PDF export to reports"
RENAME = "- [ ] sample: Rename report filters (rework-of 006-filter-names)"
EXAMPLE = f"# Backlog\n{PDF}\n{RENAME}\n  Recommend: approve\n  Clearer names.\n\n"
HOLD = {"decision": "hold", "confidence": 62, "threshold": 80, "reason": "Rated by clef-flash against the project rules.",
        "model": QUEUE["model"]}


class BacklogTriagePassTests(TriageFixture):
    def setUp(self):
        super().setUp()
        self.files["/api/backlog"] = EXAMPLE
        self.files["/api/board"] = json.dumps({"updated": "", "jobs": [{"id": "008-old-job", "stage": "done"}]})
        self.triage = lambda item: (200, json.dumps(QUEUE if "PDF" in item else HOLD))

    def sweep(self):
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            self.module.backlog_triage_pass(self.dir)
        return stdout.getvalue(), stderr.getvalue()

    def test_queues_the_pdf_line_and_holds_the_rework_line_with_its_context(self):
        out, err = self.sweep()
        self.assertEqual(err, "")
        self.assertEqual(out, "queued sample: Add PDF export to reports as 009-add-pdf-export: "
                              "confidence 86% (threshold 80%)\n"
                              "held sample: Rename report filters (rework-of 006-filter-names) for review: "
                              "confidence 62% (threshold 80%): Rated by clef-flash against the project rules.\n")
        self.assertEqual(self.posts, [{"item": "sample: Add PDF export to reports"},
                                      {"item": "sample: Rename report filters (rework-of 006-filter-names)",
                                       "context": "Recommend: approve\nClearer names."}])
        self.assertEqual(self.board()[-1]["id"], "009-add-pdf-export")
        self.assertEqual(self.files["/api/backlog"].split("\n")[1],
                         "- [x] sample: Add PDF export to reports (009-add-pdf-export)")
        self.assertEqual(self.cached(), [RENAME])
        self.assertEqual(self.gets, ["/api/pipeline/effective/sample"])

    def test_a_queued_sweep_line_carries_its_context(self):
        self.triage = (200, json.dumps(QUEUE))
        out, err = self.sweep()
        self.assertEqual(err, "")
        self.assertEqual([j["idea"].split("\n\nQueued by")[0] for j in self.board()[-2:]],
                         ["Add PDF export to reports",
                          "Rename report filters\n\nContext from the backlog:\nRecommend: approve\nClearer names."])

    def test_skips_retro_lines(self):
        retro = "- [ ] sample: Guard the ls alias"
        self.files["/api/backlog"] = (f"# Backlog\n{retro}\n  Recommend: approve\n\n  Reasons.\n\n"
                                      "  Retro: run r1, fingerprint 0123456789abcdef, created 2026-11-12T05:40:00Z\n"
                                      f"{PDF}\n")
        out, err = self.sweep()
        self.assertEqual(err, "")
        self.assertEqual(self.reads, [["sample: Add PDF export to reports"]])
        self.assertEqual(self.posts, [{"item": "sample: Add PDF export to reports"}])
        self.assertEqual(self.cached(), None)
        self.assertIn(retro + "\n", self.files["/api/backlog"])

    def test_one_verdict_read_per_sweep(self):
        self.sweep()
        self.assertEqual(self.backlog_gets, 1)
        self.assertEqual(self.reads, [["sample: Add PDF export to reports",
                                       "sample: Rename report filters (rework-of 006-filter-names)"]])
        self.assertEqual(len(self.posts), 2)
        self.reads.clear()
        self.sweep()
        self.assertEqual(self.reads, [])

    def test_a_hold_another_host_stored_is_cached_without_a_post(self):
        self.stored["sample: Rename report filters (rework-of 006-filter-names)"] = HOLD
        out, err = self.sweep()
        self.assertEqual(err, "")
        self.assertNotIn("held", out)
        self.assertEqual([p["item"] for p in self.posts], ["sample: Add PDF export to reports"])
        self.assertEqual(self.cached(), [RENAME])

    def test_a_line_the_verdict_read_rejects_does_not_stall_the_sweep(self):
        long = "sample: " + "x" * 2000
        self.files["/api/backlog"] = EXAMPLE + f"- [ ] {long}\n"
        self.triage = lambda item: (400, "item: bad") if len(item) > 2000 else (200, json.dumps(HOLD))
        out, err = self.sweep()
        self.assertEqual(len(self.posts), 3)
        self.assertEqual(self.cached(), [PDF, RENAME, f"- [ ] {long}"])

    def test_an_older_worker_404_on_the_verdict_read_rates_every_line(self):
        self.read_status = 404
        self.stored["sample: Rename report filters (rework-of 006-filter-names)"] = HOLD
        self.sweep()
        self.assertEqual(len(self.posts), 2)

    def test_a_line_with_trailing_spaces_posts_the_stripped_item(self):
        self.files["/api/backlog"] = f"# Backlog\n{PDF}   \n"
        self.triage = (200, json.dumps(HOLD))
        self.sweep()
        self.assertEqual(self.posts, [{"item": "sample: Add PDF export to reports"}])
        self.assertEqual(self.cached(), [PDF + "   "])

    def test_a_worker_error_while_picking_lines_sends_nothing(self):
        self.files["/api/backlog"] = EXAMPLE + "- [ ] beta: Add dark mode\n"
        self.views["beta"] = (500, "boom")
        with self.assertRaises(SystemExit) as raised:
            self.sweep()
        self.assertEqual(raised.exception.code, 5)
        self.assertEqual((self.reads, self.posts, self.cached()), ([], [], None))

    def test_a_row_left_on_the_board_is_not_queued_twice(self):
        self.after_board = lambda: self.cas_down.update({"/api/backlog", "/api/board"})
        self.sweep()
        self.assertEqual(self.cached(), [PDF, RENAME])
        self.cas_down.clear()
        self.posts.clear()
        self.assertEqual(self.sweep(), ("", ""))
        self.assertEqual(self.posts, [])
        self.assertEqual([j["id"] for j in self.board()], ["008-old-job", "009-add-pdf-export"])

    def test_a_second_run_sends_nothing_and_the_cache_drops_gone_lines(self):
        self.sweep()
        self.posts.clear()
        with patch.object(self.module, "save_triaged") as save:
            self.assertEqual(self.sweep(), ("", ""))
        save.assert_not_called()
        self.assertEqual(self.posts, [])
        after = self.files["/api/backlog"]
        for backlog in (after.replace("- [ ] sample: Rename", "- [x] sample: Rename"), "# Backlog\n", None):
            with self.subTest(backlog=backlog):
                self.posts.clear()
                self.files["/api/backlog"] = backlog
                self.module.save_triaged(self.dir, [RENAME])
                self.assertEqual(self.sweep(), ("", ""))
                self.assertEqual(self.cached(), [])
                self.assertEqual(self.posts, [])

    def test_skipped_lines_are_never_sent_and_a_run_sends_at_most_twenty(self):
        extra = "".join(f"- [ ] sample: Idea number {n}\n" for n in range(22))
        self.triage = (200, json.dumps(HOLD))
        for view in ((200, {"triage": {"enabled": False, "threshold": 80}}), (200, {}), (200, {"triage": True}),
                     (200, "not json"), (404, ""), (409, "")):
            with self.subTest(view=view):
                self.views["beta"] = view
                self.files["/api/backlog"] = ("# Backlog\n- [ ] beta: Off project\n- [ ] ghost: Unknown project\n"
                                              "- [ ] not a backlog line\n- [ ] beta: Off again\n" + extra)
                (self.dir / "backlog-triage.json").unlink(missing_ok=True)
                self.posts.clear()
                self.gets.clear()
                out, err = self.sweep()
                self.assertEqual(err, "")
                self.assertEqual([p["item"] for p in self.posts], [f"sample: Idea number {n}" for n in range(20)])
                self.assertEqual(self.gets, ["/api/pipeline/effective/beta", "/api/pipeline/effective/sample"])
                self.assertEqual(self.cached(), [f"- [ ] sample: Idea number {n}" for n in range(20)])

    def test_server_and_network_failures_retry_and_a_400_is_cached(self):
        for triage, reason, cached in (
            ((502, "the triage model failed"), "502", []),
            ((503, "backlog triage needs the Workers AI binding"), "503", []),
            (OSError("timed out"), "can't reach https://factory.example.test/api/backlog/triage: timed out", []),
            ((409, "stored pipeline configuration no longer validates"), "409", []),
            ((400, "item: bad"), "400", [PDF, RENAME]),
        ):
            with self.subTest(triage=triage):
                (self.dir / "backlog-triage.json").unlink(missing_ok=True)
                self.triage = triage
                for run in range(2):
                    self.posts.clear()
                    out, err = self.sweep()
                    self.assertEqual(out, "")
                    self.assertEqual(len(self.posts), (2 if run == 0 else 0) if cached else 1)
                    self.assertEqual(err, WARN.format(reason) * len(self.posts))
                    self.assertEqual(self.cached() or [], cached)
                self.assertEqual(self.files["/api/backlog"], EXAMPLE)


DROP = "- [ ] sample: Drop the legacy CSV header"
ASK = "- [ ] sample: Add CSV export to the report page"
ASK_CONTEXT = "Recommend: approve\n## Question: Delimiter\nPick: one\n- Comma\n- Tab"
SWEEP = EXAMPLE + f"{DROP}\n  Recommend: deny\n" + f"{ASK}\n" + "".join(f"  {row}\n" for row in ASK_CONTEXT.split("\n"))
DENY = {"decision": "hold", "confidence": None, "threshold": 80, "reason": "its author recommends deny", "model": None}
ASKS = {**DENY, "reason": "it asks questions a person must answer"}
UNUSABLE = {"error": "the triage model gave an unusable reply", "status": 502}
BATCH_WARN = "factory.py: backlog triage failed for {} ({}); the item waits in the backlog for review\n"
HELD_RENAME = ("held sample: Rename report filters (rework-of 006-filter-names) for review: "
               "confidence 62% (threshold 80%): Rated by clef-flash against the project rules.\n")


class BacklogTriageBatchTests(TriageFixture):
    def setUp(self):
        super().setUp()
        self.files["/api/backlog"] = SWEEP
        self.files["/api/board"] = json.dumps({"updated": "", "jobs": [{"id": "008-old-job", "stage": "done"}]})
        self.triage = lambda item: (200, json.dumps(ASKS if "report page" in item else HOLD))

    sweep = BacklogTriagePassTests.sweep

    def verdicts(self, *verdicts):
        return 200, json.dumps({"verdicts": list(verdicts)})

    def test_the_sweep_batches_plain_lines_and_posts_question_lines_alone(self):
        self.batch = self.verdicts(QUEUE, HOLD, DENY)
        out, err = self.sweep()
        self.assertEqual(err, "")
        self.assertEqual(out, "queued sample: Add PDF export to reports as 009-add-pdf-export: "
                              "confidence 86% (threshold 80%)\n" + HELD_RENAME +
                              "held sample: Drop the legacy CSV header for review: its author recommends deny\n"
                              "held sample: Add CSV export to the report page for review: "
                              "it asks questions a person must answer\n")
        self.assertEqual(self.batches, [[{"item": "sample: Add PDF export to reports"},
                                         {"item": "sample: Rename report filters (rework-of 006-filter-names)",
                                          "context": "Recommend: approve\nClearer names."},
                                         {"item": "sample: Drop the legacy CSV header", "context": "Recommend: deny"}]])
        self.assertEqual(self.posts, [{"item": "sample: Add CSV export to the report page", "context": ASK_CONTEXT}])
        self.assertEqual(self.cached(), [RENAME, DROP, ASK])
        self.assertEqual(self.board()[-1]["id"], "009-add-pdf-export")
        self.assertEqual(self.gets, ["/api/pipeline/effective/sample"])

    def test_a_sweep_sends_at_most_eight_lines_per_batch_and_one_project_each(self):
        self.batch = lambda items: self.verdicts(*[HOLD] * len(items))
        self.files["/api/backlog"] = "# Backlog\n" + "".join(f"- [ ] sample: Idea number {n}\n" for n in range(22))
        self.sweep()
        self.assertEqual([len(batch) for batch in self.batches], [8, 8, 4])
        self.assertEqual(self.posts, [])
        self.assertEqual(self.cached(), [f"- [ ] sample: Idea number {n}" for n in range(20)])

        self.batches.clear()
        (self.dir / "backlog-triage.json").unlink()
        self.files["/api/backlog"] = "# Backlog\n" + "".join(
            f"- [ ] sample: Idea number {n}\n- [ ] beta: Dark mode {n}\n" if n < 3 else f"- [ ] sample: Idea number {n}\n"
            for n in range(10))
        self.views["beta"] = (200, {"triage": {"enabled": True, "threshold": 80}})
        self.sweep()
        self.assertEqual([[entry["item"].split(":")[0] for entry in batch] for batch in self.batches],
                         [["sample"] * 8, ["sample"] * 2, ["beta"] * 3])
        self.assertEqual(self.batches[2], [{"item": f"beta: Dark mode {n}"} for n in range(3)])

    def test_one_unusable_answer_holds_only_that_line(self):
        self.batch = self.verdicts(QUEUE, UNUSABLE, {"error": "context: over 6000 characters", "status": 400})
        out, err = self.sweep()
        self.assertEqual(out, "queued sample: Add PDF export to reports as 009-add-pdf-export: "
                              "confidence 86% (threshold 80%)\n"
                              "held sample: Add CSV export to the report page for review: "
                              "it asks questions a person must answer\n")
        self.assertEqual(err, BATCH_WARN.format("sample: Rename report filters (rework-of 006-filter-names)", 502)
                         + BATCH_WARN.format("sample: Drop the legacy CSV header", 400))
        self.assertEqual(len(self.batches), 1)
        self.assertEqual([p["item"] for p in self.posts], ["sample: Add CSV export to the report page"])
        self.assertEqual(self.cached(), [DROP, ASK])

        self.batches.clear()
        self.posts.clear()
        self.batch = self.verdicts({**HOLD, "confidence": "62"})
        out, err = self.sweep()
        self.assertEqual(out, "")
        self.assertEqual(err, BATCH_WARN.format("sample: Rename report filters (rework-of 006-filter-names)",
                                                "malformed reply"))
        self.assertEqual(self.posts, [])
        self.assertEqual(self.cached(), [DROP, ASK])

    def test_batch_failures_end_the_sweep_or_fall_back(self):
        whole = "factory.py: backlog triage failed ({}); 3 items wait in the backlog for review\n"
        for batch, reason in (
            ((409, "stored pipeline configuration no longer validates"), "409"),
            ((502, "the triage model failed"), "502"),
            ((503, "backlog triage needs the Workers AI binding"), "503"),
            ((500, "boom"), "POST returned 500"),
            (OSError("timed out"), "can't reach https://factory.example.test/api/backlog/triage/batch: timed out"),
            ((200, "not json"), "malformed reply"),
            (self.verdicts(QUEUE), "malformed reply"),
            ((200, json.dumps({"decision": "off"})), None),
        ):
            with self.subTest(batch=batch):
                (self.dir / "backlog-triage.json").unlink(missing_ok=True)
                self.batches.clear()
                self.posts.clear()
                self.batch = batch
                out, err = self.sweep()
                self.assertEqual(out, "")
                self.assertEqual(err, "" if reason is None else whole.format(reason))
                self.assertEqual(len(self.batches), 1)
                self.assertEqual(self.posts, [])
                self.assertIsNone(self.cached())
                self.assertEqual(self.files["/api/backlog"], SWEEP)

        for batch in ((404, ""), (400, "items: must be a list of 1 to 8 entries")):
            with self.subTest(batch=batch):
                (self.dir / "backlog-triage.json").unlink(missing_ok=True)
                self.batches.clear()
                self.posts.clear()
                self.batch = batch
                out, err = self.sweep()
                self.assertEqual(err, "")
                self.assertEqual(len(self.batches), 1)
                self.assertEqual([p["item"] for p in self.posts],
                                 ["sample: Add PDF export to reports",
                                  "sample: Rename report filters (rework-of 006-filter-names)",
                                  "sample: Drop the legacy CSV header",
                                  "sample: Add CSV export to the report page"])
                self.assertEqual(self.cached(), [PDF, RENAME, DROP, ASK])
