import json

from support import invoke
from test_backlog_triage import TriageFixture

LINE = "- [ ] sample: Guard the ls alias"
KEY = "cd4086d2362d"
CONTEXT = ("  Recommend: approve\n  The alias crashes shells with no terminal.\n\n"
           "  ## Question: Guard the ls alias\n  How should the alias be guarded?\n  Pick: one\n"
           "  - Interactive only (recommended): Alias ls only in interactive shells\n"
           "  - Drop the alias: Remove it\n")
MESSAGE = "approve the ls alias item, interactive only"
ANSWER = "- Guard the ls alias: Interactive only"


class BacklogDecideTests(TriageFixture):
    def setUp(self):
        super().setUp()
        self.files["/api/backlog"] = f"# Backlog\n{LINE}\n{CONTEXT}"
        (self.dir / "config.md").write_text("---\ndevice: wsl\n---\n", encoding="utf-8")

    def decide(self, decision, answers=ANSWER, *extra, chat=MESSAGE):
        args = ["backlog", "decide", KEY, decision]
        if answers is not None:
            path = self.dir / "answers.md"
            path.write_text(answers, encoding="utf-8")
            args += ["--answers", str(path)]
        return invoke(self.module, args + list(extra) + ([] if chat is None else ["--chat", chat]))

    def footer(self, verb, at):
        return f'\n\n---\n{verb} in chat by wsl at {at}: "{MESSAGE}"'

    def unchanged(self, before):
        self.assertEqual(self.files, before)

    def test_approve_writes_the_job_and_checks_the_line_off(self):
        code, out, err = self.decide("approve")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), {"id": "004-guard-the-ls", "project": "sample", "decision": "approved",
                                           "line": LINE})
        job = self.board()[-1]
        at = job["updated"]
        self.assertEqual(job, {
            "id": "004-guard-the-ls", "project": "sample", "title": "Guard the ls alias", "stage": "idea",
            "updated": at,
            "idea": "Guard the ls alias\n\nAnswers from the backlog decision:\n" + ANSWER
                    + self.footer("Approved", at),
            "backlog": {"line": LINE, "decision": "approved", "at": at, "answers": ANSWER, "via": "chat",
                        "quote": MESSAGE, "by": "wsl"}})
        self.assertEqual(self.files["/api/backlog"],
                         f"# Backlog\n- [x] sample: Guard the ls alias (004-guard-the-ls)\n{CONTEXT}")

    def test_approve_with_notes_and_other(self):
        code, _, err = self.decide("approve", "- Guard the ls alias: Interactive only. Other: and   warn once",
                                   "--notes", "  Keep it small.  ")
        self.assertEqual(code, 0, err)
        job = self.board()[-1]
        self.assertEqual(job["backlog"]["notes"], "Keep it small.")
        self.assertEqual(job["backlog"]["answers"], "- Guard the ls alias: Interactive only. Other: and warn once")
        self.assertTrue(job["idea"].startswith(
            "Guard the ls alias\n\nNotes from the backlog decision:\nKeep it small.\n\n"
            "Answers from the backlog decision:\n- Guard the ls alias: Interactive only. Other: and warn once\n\n---\n"))

    def test_deny_and_rework_write_the_dashboard_shapes(self):
        code, _, err = self.decide("deny", "- Guard the ls alias: Drop the alias")
        self.assertEqual(code, 0, err)
        job = self.board()[-1]
        at = job["updated"]
        self.assertEqual(job, {
            "id": "004-guard-the-ls", "project": "sample", "title": "Guard the ls alias",
            "idea": "Guard the ls alias" + self.footer("Denied", at), "stage": "closed", "updated": at, "done": at,
            "note": "denied from the backlog",
            "backlog": {"line": LINE, "decision": "denied", "at": at, "answers": "- Guard the ls alias: Drop the alias",
                        "via": "chat", "quote": MESSAGE, "by": "wsl"}})
        self.files["/api/backlog"] = f"# Backlog\n{LINE} (rework-of 002-ls)\n{CONTEXT}"
        key = self.module.backlog_key(f"{LINE} (rework-of 002-ls)")
        path = self.dir / "none.md"
        path.write_text("- Guard the ls alias: no answer\n", encoding="utf-8")
        code, out, err = invoke(self.module, ["backlog", "decide", key, "rework", "--answers", str(path),
                                              "--notes", "Say which shells.", "--chat", MESSAGE])
        self.assertEqual(code, 0, err)
        job = self.board()[-1]
        at = job["updated"]
        self.assertEqual(job, {
            "id": "005-guard-the-ls", "project": "sample", "title": "Guard the ls alias",
            "idea": "Guard the ls alias" + self.footer("Rework requested", at), "stage": "idea", "updated": at,
            "note": "rework requested from the backlog",
            "backlog": {"line": f"{LINE} (rework-of 002-ls)", "decision": "rework", "at": at,
                        "notes": "Say which shells.", "via": "chat", "quote": MESSAGE, "by": "wsl"},
            "refs": [{"id": "002-ls", "kind": "rework-of"}]})
        self.assertEqual(json.loads(out)["decision"], "rework")

    def test_rework_needs_no_answers(self):
        code, _, err = self.decide("rework", None, "--notes", "Say which shells.")
        self.assertEqual(code, 0, err)
        self.assertNotIn("answers", self.board()[-1]["backlog"])

    def test_refusals_write_nothing(self):
        self.files["/api/backlog"] += "- [ ] nowhere: Unregistered item\n"
        before = dict(self.files)
        cases = [
            (("approve", None), 2, "answer every question to approve or deny; pass --answers with one line per question"),
            (("deny", "- Guard the ls alias: no answer"), 2,
             "answer every question to approve or deny; pass --answers with one line per question"),
            (("approve", "- Guard the ls alias: Keep it"), 2, "Keep it is not an option of Guard the ls alias"),
            (("rework", "- Guard the ls alias: Keep it", "--notes", "n"), 2, "Keep it is not an option"),
            (("approve", "- Guard the ls alias: Interactive only, Drop the alias"), 2,
             "Guard the ls alias takes one pick"),
            (("approve", ""), 2, "one line per question"),
            (("approve", ANSWER + "\n- Another: x"), 2, "one line per question"),
            (("approve", "- Other header: Interactive only"), 2, "one line per question"),
            (("approve", ANSWER + " see https://example.test/x"), 2, "links are not allowed"),
            (("rework", None), 2, "rework needs --notes that say what to change"),
            (("rework", None, "--notes", "   "), 2, "rework needs --notes that say what to change"),
            (("approve", ANSWER, "--notes", "a" * 2001), 2, "--notes must be a string of 1-2000 characters"),
        ]
        for (decision, answers, *extra), code, message in cases:
            with self.subTest(message=message, decision=decision):
                result = self.decide(decision, answers, *extra)
                self.assertEqual(result[0], code, result[2])
                self.assertIn(message, result[2])
                self.unchanged(before)
        for chat in (None, "", "   "):
            with self.subTest(chat=chat):
                result = self.decide("approve", chat=chat)
                self.assertEqual(result[0], 2)
                self.unchanged(before)
        result = self.decide("approve", chat="see https://example.test/x")
        self.assertEqual(result[0], 2)
        self.assertIn("--chat must not hold a code fence or a link; nothing was changed", result[2])
        result = invoke(self.module, ["backlog", "decide", "0123456789ab", "deny", "--chat", MESSAGE])
        self.assertEqual(result, (4, "", "factory.py: no open backlog item 0123456789ab; it was decided, reworded "
                                         "or removed\n"))
        unregistered = self.module.backlog_key("- [ ] nowhere: Unregistered item")
        result = invoke(self.module, ["backlog", "decide", unregistered, "approve", "--chat", MESSAGE])
        self.assertEqual(result, (4, "", "factory.py: register project nowhere before deciding this item\n"))
        for args in (["decide", KEY, "maybe", "--chat", MESSAGE], ["decide", "nothex", "deny", "--chat", MESSAGE],
                     ["decide", KEY, "deny", "--chat", MESSAGE, "--chat", MESSAGE],
                     ["decide", KEY, "deny", "--bogus", "x", "--chat", MESSAGE], ["decide", KEY, "deny", "--chat"]):
            with self.subTest(args=args):
                self.assertEqual(invoke(self.module, ["backlog", *args])[0], 2)
        self.unchanged(before)

    def test_two_picks_allowed_under_pick_many(self):
        self.files["/api/backlog"] = f"# Backlog\n{LINE}\n{CONTEXT.replace('Pick: one', 'Pick: many')}"
        code, _, err = self.decide("approve", "- Guard the ls alias: Interactive only, Drop the alias")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.board()[-1]["backlog"]["answers"], "- Guard the ls alias: Interactive only, Drop the alias")

    def test_unparsed_context_has_no_questions(self):
        self.files["/api/backlog"] = f"# Backlog\n{LINE}\n  Retro: from run 20261111T090000Z-a1b2c3\n"
        code, _, err = self.decide("approve", None)
        self.assertEqual(code, 0, err)
        self.assertNotIn("answers", self.board()[-1]["backlog"])
        self.assertIn("  Retro: from run", self.files["/api/backlog"])

    def test_line_gone_between_writes_removes_the_job(self):
        def edit():
            self.files["/api/backlog"] = "# Backlog\n- [ ] sample: Guard the ls alias, reworded\n"
        self.after_board = edit
        code, out, err = self.decide("approve")
        self.assertEqual((code, out), (6, ""))
        self.assertEqual(err, "factory.py: this backlog item changed elsewhere, so nothing was changed\n")
        self.assertEqual([j["id"] for j in self.board()], ["003-old-job"])

    def test_line_gone_after_a_claim_keeps_the_job(self):
        def edit():
            self.files["/api/backlog"] = "# Backlog\n"
            board = json.loads(self.files["/api/board"])
            board["jobs"][-1]["claim"] = {"device": "mac"}
            self.files["/api/board"] = json.dumps(board)
        self.after_board = edit
        code, _, err = self.decide("approve")
        self.assertEqual(code, 6)
        self.assertEqual(err, "factory.py: 004-guard-the-ls was saved, but its backlog line changed elsewhere. "
                              "Check backlog.md.\n")
        self.assertEqual(self.board()[-1]["id"], "004-guard-the-ls")

    def test_rerun_with_the_same_quote_reuses_the_job(self):
        self.cas_down = {"/api/backlog"}
        self.assertEqual(self.decide("approve")[0], 5)
        self.cas_down = set()
        self.assertEqual(self.board()[-1]["id"], "004-guard-the-ls")
        code, out, err = self.decide("approve")
        self.assertEqual(code, 0, err)
        self.assertEqual(json.loads(out)["id"], "004-guard-the-ls")
        self.assertEqual([j["id"] for j in self.board()], ["003-old-job", "004-guard-the-ls"])
        self.assertIn("- [x] sample: Guard the ls alias (004-guard-the-ls)", self.files["/api/backlog"])
