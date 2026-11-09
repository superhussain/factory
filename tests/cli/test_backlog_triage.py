import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

ITEM = "sample: Add CSV export to reports"
QUEUE = {"decision": "queue", "confidence": 86, "threshold": 80, "reason": "A small export feature.",
         "model": "@cf/meta/llama-3.1-8b-instruct-fast"}
WARN = "factory.py: backlog triage failed ({}); the item waits in the backlog for review\n"


class BacklogTriageTests(IsolatedTestCase):
    def setUp(self):
        self.module = load_runtime()
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.dir, ignore_errors=True))
        self.files = {"/api/backlog": "# Backlog\n",
                      "/api/board": json.dumps({"updated": "", "jobs": [{"id": "003-old-job", "stage": "done"}]})}
        self.triage = (200, json.dumps(QUEUE))
        self.posts = []
        self.after_board = None

        def api(home, method, path, body=None, ok=(200,), headers=None, raw=False):
            if (method, path) == ("GET", "/projects.json"):
                return 200, {}, json.dumps([{"slug": "sample"}])
            self.assertEqual((method, path), ("POST", "/api/backlog/triage"))
            self.assertEqual(headers, {"Content-Type": "application/json"})
            self.posts.append(json.loads(body))
            if isinstance(self.triage, Exception):
                self.module.fail("can't reach https://factory.example.test/api/backlog/triage: timed out", 5)
            status, text = self.triage
            if status not in ok:
                self.module.fail(f"POST returned {status}", 5)
            return status, {}, text

        def cas(home, path, change):
            new, result = change(self.files.get(path))
            if new is not None:
                self.files[path] = new
            if path == "/api/board" and self.after_board:
                self.after_board()
                self.after_board = None
            return result

        for target, fake in (("factory_home", lambda: self.dir), ("api", api), ("cas", cas)):
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

    def test_queue_writes_the_idea_then_checks_the_line_off(self):
        (self.dir / "jobs" / "sample" / "007-local-only").mkdir(parents=True)
        code, out, err = self.add()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"added {ITEM}\nqueued {ITEM} as 008-add-csv-export: confidence 86% (threshold 80%)\n")
        self.assertEqual(self.posts, [{"item": ITEM}])
        job = self.board()[-1]
        at = job["updated"]
        self.assertEqual(job, {
            "id": "008-add-csv-export", "project": "sample", "title": "Add CSV export to reports",
            "stage": "idea", "updated": at,
            "idea": "Add CSV export to reports\n\nQueued by backlog triage: confidence 86% (threshold 80%). "
                    "A small export feature.",
            "backlog": {"line": f"- [ ] {ITEM}", "decision": "approved", "at": at, "by": "triage",
                        "triage": {"model": QUEUE["model"], "confidence": 86, "threshold": 80,
                                   "reason": "A small export feature."}},
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
        self.assertTrue(job["idea"].startswith("Add CSV export to reports\n\nQueued by backlog triage:"))
        self.assertEqual(self.files["/api/backlog"].split("\n")[1], f"- [x] {item} (004-add-csv-export)")

    def test_hold_and_off_change_nothing_after_the_append(self):
        line = f"# Backlog\n- [ ] {ITEM}\n"
        board = self.files["/api/board"]
        for reply, printed, cached in (
            ({"decision": "hold", "confidence": 62, "threshold": 80, "reason": "Touches billing.",
              "model": QUEUE["model"]},
             f"held {ITEM} for review: confidence 62% (threshold 80%): Touches billing.\n", [f"- [ ] {ITEM}"]),
            ({"decision": "hold", "confidence": None, "threshold": 80, "reason": "its author recommends deny",
              "model": None},
             f"held {ITEM} for review: its author recommends deny\n", [f"- [ ] {ITEM}"]),
            ({"decision": "off"}, "", None),
        ):
            with self.subTest(reply=reply):
                self.files["/api/backlog"] = "# Backlog\n"
                (self.dir / "backlog-triage.json").unlink(missing_ok=True)
                self.triage = (200, json.dumps(reply))
                self.assertEqual(self.add(), (0, f"added {ITEM}\n" + printed, ""))
                self.assertEqual(self.files["/api/backlog"], line)
                self.assertEqual(self.files["/api/board"], board)
                self.assertEqual(self.cached(), cached)

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
                self.assertEqual((code, out), (0, f"added {ITEM}\n"))
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
        self.assertEqual(out, f"added {ITEM}\nkept {ITEM} in the backlog: its line changed before triage finished\n")
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
