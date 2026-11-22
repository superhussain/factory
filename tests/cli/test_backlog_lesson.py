import contextlib
import io
import json
from datetime import datetime, timezone
from unittest.mock import patch

from support import invoke
from test_backlog_triage import TriageFixture

FACT = "Worker route changes run pnpm test:protocol; pnpm test skips them."
WHY = "The close reason names the command but not which routes need it."
ITEM = f"factory: Lesson: {FACT}"


def card(slug, job, day):
    return (f"- [ ] {slug}: Lesson: {FACT}\n  Recommend: approve\n\n"
            f"  Approve adds this bullet to {slug}/notes.md. No job starts.\n  Why it waits: {WHY}\n\n"
            f"  Lesson: from {job}, queued {day}\n")


class BacklogLessonTests(TriageFixture):
    def setUp(self):
        super().setUp()
        fake = self.module.api

        def api(home, method, path, *args, **kwargs):
            if (method, path) == ("GET", "/projects.json"):
                return 200, {}, json.dumps([{"slug": "sample"}, {"slug": "factory"}])
            return fake(home, method, path, *args, **kwargs)
        patcher = patch.object(self.module, "api", api)
        patcher.start()
        self.addCleanup(patcher.stop)

    def file(self, name, text):
        path = self.dir / name
        path.write_text(text, encoding="utf-8")
        return str(path)

    def lesson(self, target="factory/1712-export-csv", fact=FACT, why=WHY):
        return invoke(self.module, ["backlog", "lesson", target, "--why-file", self.file("why.txt", why),
                                    "--fact-file", self.file("fact.txt", fact)])

    def test_queues_the_card(self):
        today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
        code, out, err = self.lesson(fact=f"  {FACT}\n", why=f"{WHY}\n")
        self.assertEqual((code, err), (0, ""))
        key = self.module.backlog_key(f"- [ ] {ITEM}")
        self.assertEqual(out, f"added {ITEM} with context\nlink: https://factory.example/#/backlog/{key}\n"
                              f"held {ITEM} for review: lesson cards always wait for a person\n")
        self.assertEqual(self.files["/api/backlog"], "# Backlog\n" + card("factory", "1712-export-csv", today))
        self.assertEqual(self.posts, [])
        self.assertEqual(len(self.board()), 1)

    def test_bad_input_writes_nothing(self):
        (self.dir / ".env").write_text("FACTORY_TOKEN=abcdefghijklmnop\n", encoding="utf-8")
        before = dict(self.files)
        cases = [
            ({"target": "factory"}, 2, self.module.BACKLOG_USAGE),
            ({"target": "factory/../x"}, 2, self.module.BACKLOG_USAGE),
            ({"target": "factory/1712 x"}, 2, self.module.BACKLOG_USAGE),
            ({"target": "factory/1712-é"}, 2, self.module.BACKLOG_USAGE),
            ({"fact": "x" * 201}, 2, "--fact-file must be a string of 1-200 characters; nothing was written"),
            ({"why": "x" * 301}, 2, "--why-file must be a string of 1-300 characters; nothing was written"),
            ({"fact": "See https://example.test for it."}, 2,
             "--fact-file must not hold a code fence or a link; nothing was written"),
            ({"why": "the token abcdefghijklmnop leaked"}, 2,
             "--why-file holds this device's FACTORY_TOKEN; nothing was written"),
            ({"fact": "  \n"}, 2, None),
            ({"target": "nowhere/1712-export-csv"}, 4, None),
        ]
        for kwargs, status, message in cases:
            with self.subTest(kwargs=kwargs):
                code, out, err = self.lesson(**kwargs)
                self.assertEqual((code, out), (status, ""))
                if message:
                    self.assertEqual(err, f"factory.py: {message}\n")
                self.assertEqual(self.files, before)
                self.assertEqual(self.posts, [])

    def test_lesson_cards_wait_for_a_person(self):
        pdf = "- [ ] sample: Add PDF export to reports"
        lesson = card("sample", "001-capture", "2026-10-27")
        raw = lesson.split("\n", 1)[0]
        self.files["/api/backlog"] = f"# Backlog\n{lesson}{pdf}\n"
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.module.backlog_triage_pass(self.dir)
        self.assertEqual(self.reads, [["sample: Add PDF export to reports"]])
        self.assertIn(lesson, self.files["/api/backlog"])

        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            found = self.module.first_item(f"# Backlog\n{lesson}", {"sample"}, {"sample": "/x"}, set())
        self.assertIsNone(found)
        self.assertEqual(err.getvalue(), f'skipping "sample: Lesson: {FACT}": a lesson card waits for a person\n')

        self.assertEqual(list(self.module.backlog_candidates(f"# Backlog\n{lesson}{pdf}\n", "sample")),
                         [f"backlog:{self.module.backlog_key(pdf)}"])

        self.files["/api/backlog"] = f"# Backlog\n{lesson}"
        (self.dir / "config.md").write_text("---\ndevice: wsl\n---\n", encoding="utf-8")
        key = self.module.backlog_key(raw)
        before = dict(self.files)
        for decision, extra in (("approve", []), ("rework", ["--notes-file", self.file("notes.txt", "Shorter.")])):
            with self.subTest(decision=decision):
                code, out, err = invoke(self.module, ["backlog", "decide", key, decision, *extra,
                                                      "--chat-file", self.file("chat.txt", "approve the lesson")])
                self.assertEqual((code, out), (2, ""))
                self.assertEqual(err, "factory.py: lesson cards are approved on the dashboard, which adds the bullet "
                                      "to notes.md; nothing was changed\n")
                self.assertEqual(self.files, before)
        code, _, err = invoke(self.module, ["backlog", "decide", key, "deny",
                                            "--chat-file", self.file("chat.txt", "deny the lesson")])
        self.assertEqual(code, 0, err)
        job = self.board()[-1]
        self.assertEqual((job["stage"], job["note"], job["backlog"]["decision"]),
                         ("closed", "denied from the backlog", "denied"))
        self.assertTrue(self.files["/api/backlog"].startswith(f"# Backlog\n- [x] sample: Lesson: {FACT} ({job['id']})\n"))
