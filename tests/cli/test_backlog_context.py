import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

ITEM = "factory: Add CSV export to the board (sub-job-of 204-factory-backlog-cards)"
CONTEXT = """Recommend: approve

The operator exports the board by hand today. Job 204 left CSV export out of scope.

## Question: Export scope
Which jobs go in the file?
Pick: one
- Visible jobs (recommended): Only the cards the current filter shows.
- All jobs: Every job on the board.
"""
LINK = "link: https://factory.example/#/backlog/85abcd967ecb\n"
STORED = """- [ ] factory: Add CSV export to the board (sub-job-of 204-factory-backlog-cards)
  Recommend: approve

  The operator exports the board by hand today. Job 204 left CSV export out of scope.

  ## Question: Export scope
  Which jobs go in the file?
  Pick: one
  - Visible jobs (recommended): Only the cards the current filter shows.
  - All jobs: Every job on the board.
"""
QUESTION = "## Question: Q\nPrompt?\nPick: one\n- A: a\n- B: b\n"


class BacklogContextTests(IsolatedTestCase):
    def setUp(self):
        self.module = load_runtime()
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.dir, ignore_errors=True))
        self.backlog = "# Backlog\n"
        self.writes = []
        self.triaged = []

        def api(home, method, path, *args, **kwargs):
            if (method, path) == ("POST", "/api/backlog/triage"):
                self.triaged.append(path)
                return 404, {}, "not found"
            self.assertEqual((method, path), ("GET", "/projects.json"))
            return 200, {}, json.dumps([{"slug": "factory"}])

        def cas(home, path, change):
            new, result = change(self.backlog)
            if new is not None:
                self.backlog = new
                self.writes.append(new)
            return result

        for target, fake in (("factory_home", lambda: self.dir), ("api_url", lambda home: "https://factory.example"),
                             ("api", api), ("cas", cas)):
            patcher = patch.object(self.module, target, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def add(self, text, raw=None):
        path = self.dir / "context.md"
        if raw is None:
            path.write_text(text, encoding="utf-8")
        else:
            path.write_bytes(raw)
        return invoke(self.module, ["backlog", "add", ITEM, "--context", str(path)]), str(path)

    def test_writes_the_indented_block_and_next_still_finds_the_item(self):
        (code, out, err), _ = self.add(CONTEXT)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"added {ITEM} with context\n{LINK}")
        self.assertEqual(self.backlog, "# Backlog\n" + STORED)
        line = "- [ ] " + ITEM
        self.assertEqual(
            self.module.first_item(self.backlog, {"factory"}, {"factory": "/x"}, set()),
            (line, "factory", ITEM.split(": ", 1)[1], [{"id": "204-factory-backlog-cards", "kind": "sub-job-of"}]),
        )

    def test_backlog_key_hashes_the_line(self):
        self.assertEqual(self.module.backlog_key("- [ ] " + ITEM), "85abcd967ecb")
        self.assertEqual(self.module.backlog_key("- [ ] factory: Café menu on the board"), "f3b20e9cae86")

    def test_without_a_file_the_line_is_added_alone(self):
        code, out, _ = invoke(self.module, ["backlog", "add", ITEM])
        self.assertEqual((code, out), (0, f"added {ITEM}\n{LINK}"))
        self.assertEqual(self.backlog, "# Backlog\n- [ ] " + ITEM + "\n")

    def test_skips_a_line_already_in_the_backlog(self):
        for line, link in (("- [ ] " + ITEM, LINK), ("- [x] " + ITEM + " (205-csv-export)",
                                                       "link: https://factory.example/#205-csv-export\n"),
                           ("- [x] " + ITEM + " (205-csv#export)", "link: https://factory.example/#205-csv%23export\n")):
            with self.subTest(line=line):
                self.backlog = "# Backlog\r\n" + line + "\r\n"
                self.writes, self.triaged = [], []
                code, out, _ = invoke(self.module, ["backlog", "add", ITEM])
                self.assertEqual((code, out), (0, f"already in backlog: {ITEM}\n{link}"))
                self.assertEqual((self.writes, self.triaged), ([], []))
                self.assertEqual(self.backlog, "# Backlog\r\n" + line + "\r\n")

    def test_already_queued_says_the_context_was_not_added(self):
        self.backlog = "# Backlog\n- [ ] " + ITEM + "\n"
        (code, out, _), _ = self.add(CONTEXT)
        self.assertEqual((code, out), (0, f"already in backlog: {ITEM} (the --context file was not added)\n{LINK}"))
        self.assertEqual(self.writes, [])

    def test_skips_a_stored_line_with_trailing_whitespace(self):
        for line in ("- [ ] " + ITEM + "  \t", "- [x] " + ITEM + " (205-csv-export) "):
            with self.subTest(line=line):
                self.backlog = "# Backlog\n" + line + "\n"
                self.writes = []
                code, out, _ = invoke(self.module, ["backlog", "add", ITEM])
                self.assertEqual(code, 0)
                self.assertTrue(out.startswith(f"already in backlog: {ITEM}\n"), out)
                self.assertEqual(self.writes, [])

    def test_normalizes_line_endings_and_edge_blank_lines(self):
        (code, _, _), _ = self.add("", raw=b"\r\n\r\n" + CONTEXT.replace("\n", "\r\n").encode() + b"\r\n\r\n")
        self.assertEqual(code, 0)
        self.assertEqual(self.backlog, "# Backlog\n" + STORED)

    def test_bad_files_exit_2_with_their_message_and_write_nothing(self):
        two = QUESTION + QUESTION.replace("Q\n", "R\n")
        cases = [
            ("raw", b"\xff\xfe", "cannot read it as UTF-8 text"),
            ("text", "Recommend: approve\n" + "x" * 6000, "longer than 6000 characters"),
            ("text", "Recommend: approve\nreasons​", "holds a control or invisible character"),
            ("text", "Recommend: maybe\nreasons", 'the first line must be "Recommend: approve" or "Recommend: deny"'),
            ("text", "Recommend: deny\n\n" + QUESTION, "write the reasons first, then only ## Question: sections"),
            ("text", "Recommend: deny\nreasons\n" + QUESTION * 4, "at most 3 questions"),
            ("text", "Recommend: deny\nreasons\n" + QUESTION.replace("Pick: one", "Pick: some"),
             "question 1 is not in the needs.md question format"),
            ("text", "Recommend: deny\nreasons\n" + two.replace("- B: b", "B"),
             "question 1 is not in the needs.md question format"),
            ("text", "Recommend: deny\nreasons\n" + QUESTION + QUESTION.replace("- A: a", "- A (recommended): a")
             .replace("- B: b", "- B (recommended): b"), "question 2 is not in the needs.md question format"),
        ]
        for kind, content, message in cases:
            with self.subTest(message=message):
                result, path = self.add(content if kind == "text" else "", raw=content if kind == "raw" else None)
                self.assertEqual(result, (2, "", f"factory.py: context file {path}: {message}\n"
                                                 f"{self.module.BACKLOG_CONTEXT_EXAMPLE}\n"))
                self.assertEqual(self.writes, [])

    def test_error_example_is_a_valid_context(self):
        example = self.module.BACKLOG_CONTEXT_EXAMPLE
        self.assertIsNone(self.module.backlog_context_error(example.split("\n", 1)[1]))

    def test_bad_usage_exits_2(self):
        usage = 'factory.py: usage: backlog add "<slug>: <feature>" [--context <file>] | backlog rework <job-id> (feature on stdin)\n'
        for args in (["backlog", "add", ITEM, "--context"], ["backlog", "add", ITEM, "--file", "x"],
                     ["backlog", "add", ITEM, "x"], ["backlog", "add", ITEM, "--context", ""]):
            with self.subTest(args=args):
                self.assertEqual(invoke(self.module, args), (2, "", usage))
        self.assertEqual(self.writes, [])


PARSE = "sample: Parse recipe URLs (sub-job-of 041-recipe-import)"
STORE = "sample: Store imported recipes (sub-job-of 041-recipe-import)"


class NextDependsTests(IsolatedTestCase):
    """next skips a line until its Depends on: prerequisite is checked off and its job merged, closed or gone."""

    def setUp(self):
        self.module = load_runtime()
        self.claims, self.board_reads = [], 0

        def api(home, method, path, *args, **kwargs):
            if path == "/projects.json":
                return 200, {}, json.dumps([{"slug": "sample"}])
            return 200, {}, self.backlog

        def read_board(home):
            self.board_reads += 1
            return {"jobs": self.jobs}

        def claim_id(home, feature_slug, project_slug, refs=None):
            self.claims.append((feature_slug, refs))
            return "053-store-imported-recipes"

        for target, fake in (("factory_home", lambda: Path("/x")), ("holder", lambda home: ("device", "tag", 120)),
                             ("read_paths", lambda home: {"sample": "/x"}), ("api", api), ("read_board", read_board),
                             ("claim_id", claim_id), ("cas", lambda home, path, change: True)):
            patcher = patch.object(self.module, target, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def next(self, parse_line, stage=None):
        self.backlog = f"{parse_line}\n- [ ] {STORE}\n  Depends on: {PARSE}\n"
        self.jobs = [] if stage is None else [{"id": "052-parse-recipe-urls", "stage": stage}]
        return invoke(self.module, ["next"])

    def test_waits_while_the_prerequisite_line_is_unchecked(self):
        self.backlog = f"- [ ] {STORE}\n  Depends on: {PARSE}\n- [ ] {PARSE}\n"
        self.jobs = []
        code, out, err = invoke(self.module, ["next"])
        self.assertEqual((code, json.loads(out)["feature"]), (0, PARSE.split(": ", 1)[1]))
        self.assertEqual(err, f'skipping "{STORE}": it depends on "{PARSE}", which is still unchecked\n')
        self.assertEqual((self.claims[0][0], self.board_reads), ("parse-recipe-urls", 0))

    def test_waits_while_the_prerequisite_job_is_open(self):
        code, out, err = self.next(f"- [x] {PARSE} (052-parse-recipe-urls)", "build")
        self.assertEqual((code, out), (3, ""))
        self.assertEqual(err, f'skipping "{STORE}": it depends on 052-parse-recipe-urls, which is still open\n'
                              "factory.py: nothing this device can run in the backlog\n")
        self.assertEqual((self.claims, self.board_reads), ([], 1))

    def test_runs_with_refs_once_the_prerequisite_job_is_merged_closed_or_gone(self):
        for stage in ("merged", "closed", None):
            with self.subTest(stage=stage):
                self.claims = []
                code, out, err = self.next(f"- [x] {PARSE} (052-parse-recipe-urls)", stage)
                self.assertEqual((code, err), (0, ""))
                self.assertEqual(json.loads(out), {"id": "053-store-imported-recipes", "project": "sample",
                                                   "feature": STORE.split(": ", 1)[1]})
                self.assertEqual(self.claims, [("store-imported-recipes", [
                    {"id": "041-recipe-import", "kind": "sub-job-of"},
                    {"id": "052-parse-recipe-urls", "kind": "depends-on"}])])
