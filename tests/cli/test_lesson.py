import json
from datetime import datetime, timezone
from unittest.mock import patch

from support import invoke
from test_backlog_triage import TriageFixture

FACT = "Worker route changes run pnpm test:protocol; pnpm test skips them."
WHY = "The close reason names the command but not which routes need it."
ITEM = f"factory: Lesson: {FACT}"
NOTES = "# Notes\n\n- Run pnpm test.\n"
JOB = "1712-export-csv"


class LessonTests(TriageFixture):
    def setUp(self):
        super().setUp()
        self.notes = (200, NOTES)
        self.history = (200, json.dumps([{"version": 7}, {"version": 6}]))
        self.retro = (200, "")
        self.puts = []
        fake = self.module.api

        def api(home, method, path, body=None, ok=(200,), headers=None, raw=False):
            if (method, path) == ("GET", "/projects.json"):
                return 200, {}, json.dumps([{"slug": "sample"}, {"slug": "factory"}])
            if (method, path) == ("GET", "/api/project-documents/factory/notes.md"):
                self.assertEqual(ok, (200, 404))
                return self.notes[0], {}, self.notes[1]
            if (method, path) == ("GET", "/api/project-history/factory/notes.md"):
                self.assertEqual(ok, (200, 404))
                return self.history[0], {}, self.history[1]
            if (method, path) == ("PUT", "/api/project-retro/factory/notes.md"):
                self.assertEqual(ok, (200, 400, 403, 404, 409, 412, 413, 429))
                self.assertEqual(headers, {"Content-Type": "application/json"})
                self.puts.append(json.loads(body))
                return self.retro[0], {"X-Project-Version": "8"}, self.retro[1]
            return fake(home, method, path, body, ok, headers, raw)
        patcher = patch.object(self.module, "api", api)
        patcher.start()
        self.addCleanup(patcher.stop)

    def file(self, name, text):
        path = self.dir / name
        path.write_text(text, encoding="utf-8")
        return str(path)

    def lesson(self, target=f"factory/{JOB}", fact=FACT, why=None):
        args = ["lesson", target, "--fact-file", self.file("fact.txt", fact)]
        if why is not None:
            args += ["--why-file", self.file("why.txt", why)]
        return invoke(self.module, args)

    def held(self, why):
        today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
        key = self.module.backlog_key(f"- [ ] {ITEM}")
        out = (f"added {ITEM} with context\nlink: https://factory.example/#/backlog/{key}\n"
               f"held {ITEM} for review: lesson cards always wait for a person\n")
        backlog = (f"# Backlog\n- [ ] {ITEM}\n  Recommend: approve\n\n"
                   f"  Approve adds this bullet to factory/notes.md. No job starts.\n  Why it waits: {why}\n\n"
                   f"  Lesson: from {JOB}, queued {today}\n")
        return out, backlog

    def test_appends_a_clear_lesson(self):
        today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
        before = dict(self.files)
        code, out, err = self.lesson(fact=f"  {FACT}\n")
        self.assertEqual((code, err), (0, ""))
        bullet = f"- {FACT} ({JOB}, {today})"
        self.assertEqual(out, f"added lesson to factory/notes.md version 8: {bullet}\n")
        self.assertEqual(len(self.puts), 1)
        body = self.puts[0]
        self.assertRegex(body.pop("run"), r"\Alesson-\d{8}T\d{6}Z\Z")
        self.assertEqual(body, {"schema": "factory.retro-doc-edit/1", "base_version": 7,
                                "find": "- Run pnpm test.", "replace": f"- Run pnpm test.\n{bullet}",
                                "summary": f"Notes: lesson from {JOB}"})
        self.assertEqual(self.files, before)
        self.assertEqual(self.posts, [])

    def test_refusal_queues_a_card(self):
        missing = "factory/notes.md is missing or empty, and only a person adds its first bullet."
        cases = [
            ("429", (200, NOTES), (429, "retro edit cap: 2 per file per day"),
             "not appended: 429 retro edit cap: 2 per file per day\n",
             "Not appended directly: 429 retro edit cap: 2 per file per day", 1),
            ("409", (200, NOTES), (409, "base_version 7 is not the current version 9"),
             "not appended: 409 base_version 7 is not the current version 9\n",
             "Not appended directly: 409 base_version 7 is not the current version 9", 1),
            ("missing", (404, ""), (200, ""), "", missing, 0),
            ("empty", (200, " \n\n"), (200, ""), "", missing, 0),
        ]
        for name, notes, retro, first, why, puts in cases:
            with self.subTest(name):
                self.files["/api/backlog"] = "# Backlog\n"
                self.notes, self.retro, self.puts = notes, retro, []
                code, out, err = self.lesson()
                self.assertEqual((code, err), (0, ""))
                held_out, backlog = self.held(why)
                self.assertEqual(out, first + held_out)
                self.assertEqual(self.files["/api/backlog"], backlog)
                self.assertEqual(len(self.puts), puts)

    def test_why_file_queues_a_card(self):
        code, out, err = self.lesson(why=WHY)
        self.assertEqual((code, err), (0, ""))
        held_out, backlog = self.held(WHY)
        self.assertEqual(out, held_out)
        self.assertEqual(self.files["/api/backlog"], backlog)
        self.assertEqual(self.puts, [])

    def test_known_lesson_writes_nothing(self):
        before = dict(self.files)
        for notes in (f"- Run pnpm test.\n- {FACT}\n", f"- {FACT} (0900-older, 2026-11-01)\n- Run pnpm test.\n"):
            with self.subTest(notes=notes):
                self.notes = (200, notes)
                code, out, err = self.lesson()
                self.assertEqual((code, out, err), (0, f"already in factory/notes.md: {FACT}\n", ""))
                self.assertEqual(self.files, before)
                self.assertEqual(self.puts, [])

    def test_bad_input_writes_nothing(self):
        (self.dir / ".env").write_text("FACTORY_TOKEN=abcdefghijklmnop\n", encoding="utf-8")
        before = dict(self.files)
        usage = self.module.LESSON_USAGE
        fact = self.file("fact.txt", FACT)
        for args in ([f"factory/{JOB}"], [f"factory/{JOB}", "--fact-file"],
                     [f"factory/{JOB}", "--why-file", fact],
                     [f"factory/{JOB}", "--fact-file", fact, "--fact-file", fact],
                     [f"factory/{JOB}", "--fact-file", fact, "--context", fact]):
            with self.subTest(args=args):
                code, out, err = invoke(self.module, ["lesson", *args])
                self.assertEqual((code, out), (2, ""))
                if len(args) > 2:
                    self.assertEqual(err, f"factory.py: {usage}\n")
        code, out, err = invoke(self.module, ["lesson", f"factory/{JOB}", "--fact-file", fact, "--why-file", ""])
        self.assertEqual((code, out, err), (2, "", "factory.py: --why-file : cannot read it as UTF-8 text; "
                                                   "nothing was changed\n"))
        self.assertEqual((self.files, self.puts), (before, []))
        cases = [
            ({"target": "factory"}, 2, usage),
            ({"target": "factory/../x"}, 2, usage),
            ({"fact": "x" * 201}, 2, "--fact-file must be a string of 1-200 characters; nothing was written"),
            ({"fact": "See https://example.test for it."}, 2,
             "--fact-file must not hold a code fence or a link; nothing was written"),
            ({"fact": "the token abcdefghijklmnop leaked"}, 2,
             "--fact-file holds this device's FACTORY_TOKEN; nothing was written"),
            ({"why": "x" * 301}, 2, "--why-file must be a string of 1-300 characters; nothing was written"),
            ({"target": f"nowhere/{JOB}"}, 4, "no factory project nowhere. Known projects: factory, sample"),
        ]
        for kwargs, status, message in cases:
            with self.subTest(kwargs=kwargs):
                code, out, err = self.lesson(**kwargs)
                self.assertEqual((code, out, err), (status, "", f"factory.py: {message}\n"))
                self.assertEqual(self.files, before)
                self.assertEqual(self.puts, [])
                self.assertEqual(self.posts, [])
