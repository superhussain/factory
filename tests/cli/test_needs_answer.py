import contextlib
import hashlib
import io
import json
import os
from unittest.mock import patch

from test_suggest import SuggestCase, factory

JOB = "042-sample-export"
BASE = f"/api/jobs/sample/{JOB}/"
NEEDS = ("# Needs\n\nThe export needs a format before the build goes on.\n\n"
         "## Question: Export format\nWhich format should the export write?\nPick: one\n"
         "- CSV (recommended): Opens in any spreadsheet\n- JSON: Keeps nested fields\n")
SHA = hashlib.sha256(NEEDS.encode()).hexdigest()[:12]
MESSAGE = "042: CSV, and include a header row"
ANSWER = "- Export format: CSV. Other: include a header row\n"
STAMP = "2026-11-12T10:00:00Z"
EXPECTED = (f"# Answers\n\nFor needs.md sha256 {SHA}, submitted {STAMP} from chat by device.\n\n"
            f'Chat message: "{MESSAGE}"\n\n'
            "## Question 1: Export format\n\nWhich format should the export write?\n\nPick: one\n\n"
            "- [x] CSV (recommended): Opens in any spreadsheet\n- [ ] JSON: Keeps nested fields\n\n"
            "Other:\n> include a header row\n")


class NeedsAnswerTest(SuggestCase):
    def setUp(self):
        super().setUp()
        self.board["jobs"].append({"id": JOB, "project": "sample", "stage": "build",
                                   "needs": f"Pick the export format in [needs.md](#{JOB}/needs)."})
        self.files = {"needs.md": NEEDS}
        patch.object(factory, "now", return_value=STAMP).start()

    def request(self, home, method, path, body=None, ok=(200,), headers=None):
        if path == "/api/board":
            return super().request(home, method, path, body, ok, headers)
        self.assertTrue(path.startswith(BASE), path)
        name = path[len(BASE):]
        if method == "GET":
            self.assertEqual(ok, (200, 404))
            text = self.files.get(name)
            return (404, {}, "") if text is None else (200, {"ETag": f'W/"{name}-v1"'}, text)
        self.assertEqual(name, "answers.md")
        want = {"If-Match": '"answers.md-v1"'} if "answers.md" in self.files else {"If-None-Match": "*"}
        self.assertEqual((method, headers), ("PUT", want))
        self.files[name] = body
        return 200, {}, ""

    def write(self, name, text):
        path = self.home / name
        path.write_text(text, encoding="utf-8")
        return str(path)

    def answer(self, code=0, answers=ANSWER, chat=MESSAGE, job=JOB):
        self.output.seek(0)
        self.output.truncate()
        chat_file = chat if chat is None or os.path.islink(chat) else self.write("chat.txt", chat)
        args = ("answer", job, "--answers", self.write("a.txt", answers), "--chat-file", chat_file)
        with contextlib.redirect_stderr(io.StringIO()) as err:
            if code:
                with self.assertRaises(SystemExit) as raised:
                    factory.needs_cmd(*args)
                self.assertEqual(raised.exception.code, code, err.getvalue())
            else:
                factory.needs_cmd(*args)
        return err.getvalue()

    def job(self):
        return next(j for j in self.board["jobs"] if j["id"] == JOB)

    def test_writes_answers_then_clears_needs(self):
        self.answer()
        self.assertEqual(self.files["answers.md"], EXPECTED)
        self.assertNotIn("needs", self.job())
        self.assertEqual(self.job()["updated"], STAMP)
        self.assertEqual(json.loads(self.output.getvalue()), {"id": JOB, "answers": "saved", "needs": "cleared"})
        puts = [c.args[2] for c in self.api.call_args_list if c.args[1] == "PUT"]
        self.assertEqual(puts, [BASE + "answers.md", "/api/board"])

    def test_condition_header_matches_the_dashboard(self):
        self.answer()
        header = self.files["answers.md"].split("\n")[2]
        self.assertRegex(header, rf"^For needs\.md sha256 {SHA}, submitted (\S+) from")

    def test_second_run_replaces_answers_with_if_match(self):
        self.answer()
        self.job()["needs"] = f"Again: [needs.md](#{JOB}/needs)"
        self.answer(answers="- Export format: JSON\n")
        self.assertIn("- [ ] CSV (recommended): Opens in any spreadsheet\n- [x] JSON: Keeps nested fields\n\n"
                      "Other: none\n", self.files["answers.md"])

    def test_needs_human_keeps_needs(self):
        self.job()["stage"] = "needs-human"
        self.answer()
        self.assertEqual(self.files["answers.md"], EXPECTED)
        self.assertIn("needs", self.job())
        self.assertEqual(json.loads(self.output.getvalue())["needs"], "kept")
        self.assertEqual(self.puts(), 1)

    def refuse(self, code, **kw):
        err = self.answer(code, **kw)
        self.assertEqual(self.puts(), 0, err)
        self.assertNotIn("answers.md", self.files)
        return err

    def test_job_missing_from_board(self):
        self.assertIn("no job 043-other on the board", self.refuse(4, job="043-other"))

    def test_job_not_waiting(self):
        del self.job()["needs"]
        self.assertIn(f"{JOB} is not waiting on you", self.refuse(2))

    def test_no_needs_file(self):
        del self.files["needs.md"]
        self.assertIn("has no needs.md", self.refuse(4))

    def test_needs_without_question_form(self):
        self.files["needs.md"] = "# Needs\n\nTell me which format to use.\n"
        self.assertIn("has no question form", self.refuse(2))

    def test_label_not_an_option(self):
        self.assertIn("XML is not an option of Export format", self.refuse(2, answers="- Export format: XML\n"))

    def test_two_picks_on_pick_one(self):
        self.assertIn("Export format takes one pick", self.refuse(2, answers="- Export format: CSV, JSON\n"))

    def test_unanswered_question(self):
        self.assertIn(factory.NEEDS_SKIPPED, self.refuse(2, answers="- Export format: no answer\n"))
        self.assertIn("write one line per question", self.refuse(2, answers="\n"))

    def test_chat_file_refusals(self):
        self.assertIn("is empty", self.refuse(2, chat=" \n"))
        self.assertIn("--chat-file", self.refuse(2, chat="x" * 501))
        target = self.write("real.txt", MESSAGE)
        link = self.home / "link.txt"
        link.symlink_to(target)
        self.assertIn("symlink", self.refuse(6, chat=str(link)))

    def test_bad_usage(self):
        with contextlib.redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit) as raised:
            factory.needs_cmd("submit", JOB, "--answers", "a", "--chat-file", "c")
        self.assertEqual(raised.exception.code, 2)
        self.assertIn(factory.NEEDS_ANSWER_USAGE, err.getvalue())
        self.assertEqual(self.puts(), 0)
