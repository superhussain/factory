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
            (line, "factory", ITEM.split(": ", 1)[1]),
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
                                                       "link: https://factory.example/#205-csv-export\n")):
            with self.subTest(line=line):
                self.backlog = "# Backlog\r\n" + line + "\r\n"
                self.writes, self.triaged = [], []
                code, out, _ = invoke(self.module, ["backlog", "add", ITEM])
                self.assertEqual((code, out), (0, f"already in backlog: {ITEM}\n{link}"))
                self.assertEqual((self.writes, self.triaged), ([], []))
                self.assertEqual(self.backlog, "# Backlog\r\n" + line + "\r\n")

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
                self.assertEqual(result, (2, "", f"factory.py: context file {path}: {message}\n"))
                self.assertEqual(self.writes, [])

    def test_bad_usage_exits_2(self):
        usage = 'factory.py: usage: backlog add "<slug>: <feature>" [--context <file>] | backlog rework <job-id> (feature on stdin)\n'
        for args in (["backlog", "add", ITEM, "--context"], ["backlog", "add", ITEM, "--file", "x"],
                     ["backlog", "add", ITEM, "x"], ["backlog", "add", ITEM, "--context", ""]):
            with self.subTest(args=args):
                self.assertEqual(invoke(self.module, args), (2, "", usage))
        self.assertEqual(self.writes, [])
