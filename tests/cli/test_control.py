import json

from test_suggest import SuggestCase, factory

MESSAGE = "prioritize 042-sample-export"


class ControlTest(SuggestCase):
    def setUp(self):
        super().setUp()
        self.board["jobs"].append({"id": "042-sample-export", "stage": "build", "project": "sample"})

    def job(self):
        return next(j for j in self.board["jobs"] if j["id"] == "042-sample-export")

    def file(self, text, name="chat.txt"):
        path = self.home / name
        path.write_text(text, encoding="utf-8")
        return str(path)

    def control(self, *args):
        self.output.seek(0)
        self.output.truncate()
        return self.run_human(factory.COMMANDS["control"][0], *args, tty=False)

    def chat(self, job_id, action, message=MESSAGE):
        return self.control(job_id, action, "--chat-file", self.file(message))

    def close(self, job_id, reason="Duplicate of 041-sample-export", message="close 042-sample-export"):
        return self.control(job_id, "close", "--reason-file", self.file(reason, "reason.txt"),
                            "--chat-file", self.file(message))

    def result(self):
        return json.loads(self.output.getvalue())

    def test_prioritize_writes_the_chat_record(self):
        code, err, _ = self.chat("042-sample-export", "prioritize")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(), {"id": "042-sample-export", "action": "prioritize", "result": "prioritized"})
        priority = self.job()["priority"]
        self.assertEqual(list(priority.items()), [
            ("at", priority["at"]), ("via", "chat"), ("quote", MESSAGE), ("by", "device")])
        self.assertEqual(self.job()["updated"], priority["at"])

    def test_second_prioritize_is_unchanged(self):
        self.chat("042-sample-export", "prioritize")
        before, puts = json.dumps(self.board), self.puts()
        code, err, _ = self.chat("042-sample-export", "prioritize")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result()["result"], "unchanged")
        self.assertEqual((json.dumps(self.board), self.puts()), (before, puts))

    def test_unprioritize_removes_priority(self):
        self.chat("042-sample-export", "prioritize")
        code, err, _ = self.chat("042-sample-export", "unprioritize", "unprioritize 042-sample-export")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(),
                         {"id": "042-sample-export", "action": "unprioritize", "result": "unprioritized"})
        self.assertNotIn("priority", self.job())
        puts = self.puts()
        self.chat("042-sample-export", "unprioritize", "unprioritize 042-sample-export")
        self.assertEqual(self.result()["result"], "unchanged")
        self.assertEqual(self.puts(), puts)

    def refusals(self):
        self.board["jobs"] += [
            {"id": "043-rider", "stage": "build", "project": "sample",
             "refs": [{"kind": factory.BATCH_REF, "id": "042-sample-export"}]},
            {"id": "044-no-project", "stage": "idea"},
            {"id": "045-open", "stage": "pr-open", "project": "sample"},
            {"id": "046-merged", "stage": "merged", "project": "sample"},
            {"id": "047-closed", "stage": "closed", "project": "sample"},
            {"id": "048-idea", "stage": "idea", "project": "sample"}]

    def assert_refused(self, run, cases):
        before, puts = json.dumps(self.board), self.puts()
        for arg, want, why in cases:
            with self.subTest(arg=arg):
                code, err, _ = run(arg)
                self.assertEqual(code, want, err)
                self.assertIn(why, err)
        self.assertEqual((json.dumps(self.board), self.puts()), (before, puts))

    def test_refuses_missing_rider_projectless_and_pr_open_jobs(self):
        self.refusals()
        self.assert_refused(lambda job_id: self.chat(job_id, "prioritize"), (
            ("099-missing", 4, "no job 099-missing on the board. Nothing was written."),
            ("043-rider", 2, "043-rider cannot be prioritized: it is a rider of 042-sample-export. Nothing was written."),
            ("044-no-project", 2, "044-no-project cannot be prioritized: it has no project. Nothing was written."),
            ("045-open", 2, "045-open cannot be prioritized: it is at pr-open. Nothing was written.")))

    def test_pause_and_resume(self):
        code, err, _ = self.chat("042-sample-export", "pause", "pause 042-sample-export")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(), {"id": "042-sample-export", "action": "pause", "result": "paused"})
        paused = self.job()["paused"]
        self.assertEqual(list(paused.items()), [
            ("at", paused["at"]), ("via", "chat"), ("quote", "pause 042-sample-export"), ("by", "device")])
        self.assertEqual(self.job()["updated"], paused["at"])
        self.assertTrue(factory.paused(self.job()))
        puts = self.puts()
        self.chat("042-sample-export", "pause")
        self.assertEqual(self.result()["result"], "unchanged")
        self.assertEqual(self.puts(), puts)
        code, err, _ = self.chat("042-sample-export", "resume", "resume 042-sample-export")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(), {"id": "042-sample-export", "action": "resume", "result": "resumed"})
        self.assertNotIn("paused", self.job())
        self.assertFalse(factory.paused(self.job()))
        puts = self.puts()
        self.chat("042-sample-export", "resume")
        self.assertEqual(self.result()["result"], "unchanged")
        self.assertEqual(self.puts(), puts)

    def test_pause_refusals(self):
        self.refusals()
        self.assert_refused(lambda job_id: self.chat(job_id, "pause"), (
            ("099-missing", 4, "no job 099-missing on the board. Nothing was written."),
            ("043-rider", 2, "043-rider cannot be paused: it is a rider of 042-sample-export. Nothing was written."),
            ("044-no-project", 2, "044-no-project cannot be paused: it has no project. Nothing was written."),
            ("046-merged", 2, "046-merged cannot be paused: it is at merged. Nothing was written."),
            ("047-closed", 2, "047-closed cannot be paused: it is at closed. Nothing was written.")))
        self.assert_refused(lambda job_id: self.chat(job_id, "resume"), (
            ("099-missing", 4, "no job 099-missing on the board. Nothing was written."),))
        code, err, _ = self.chat("048-idea", "pause")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result()["result"], "paused")

    def test_close_queues_the_chat_record(self):
        code, err, _ = self.close("042-sample-export", "  Duplicate of 041-sample-export\n")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(), {"id": "042-sample-export", "action": "close", "result": "queued",
                                         "reason": "Duplicate of 041-sample-export"})
        close = self.job()["close"]
        self.assertEqual(list(close.items()), [
            ("reason", "Duplicate of 041-sample-export"), ("at", close["at"]), ("via", "chat"),
            ("quote", "close 042-sample-export"), ("by", "device")])
        self.assertEqual(self.job()["updated"], close["at"])
        self.assertTrue(factory.closing(self.job()))
        self.assertEqual(factory.close_reason(self.job()), "Duplicate of 041-sample-export")
        puts = self.puts()
        code, err, _ = self.close("042-sample-export", "Not needed")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(), {"id": "042-sample-export", "action": "close", "result": "unchanged",
                                         "reason": "Duplicate of 041-sample-export"})
        self.assertEqual(self.puts(), puts)

    def test_close_refusals(self):
        self.refusals()
        self.assert_refused(self.close, (
            ("099-missing", 4, "no job 099-missing on the board. Nothing was written."),
            ("043-rider", 2, "043-rider cannot be closed: it is a rider of 042-sample-export. Nothing was written."),
            ("044-no-project", 2, "044-no-project cannot be closed: it has no project. Nothing was written."),
            ("048-idea", 2, "048-idea cannot be closed: it is at idea. Nothing was written."),
            ("046-merged", 2, "046-merged cannot be closed: it is at merged. Nothing was written."),
            ("047-closed", 2, "047-closed cannot be closed: it is at closed. Nothing was written.")))
        bad = "--reason-file must be one line of 1 to 200 characters; nothing was changed"
        self.assert_refused(lambda reason: self.close("042-sample-export", reason), (
            ("Duplicate\nof 041", 2, bad),
            ("a" * 201, 2, bad),
            ("  \n", 2, "is empty; nothing was changed")))

    def test_usage(self):
        before = json.dumps(self.board)
        chat, reason = self.file(MESSAGE), self.file("Duplicate", "reason.txt")
        for args in (("042-sample-export", "prioritize", "--chat", MESSAGE),
                     ("042-sample-export", "pause", "--chat", MESSAGE),
                     ("042-sample-export", "archive", "--chat-file", chat),
                     ("042-sample-export", "prioritize", "--note", chat),
                     ("042-sample-export", "pause", "--chat-file", chat, "--reason-file", reason),
                     ("042-sample-export", "close", "--chat-file", chat),
                     ("042-sample-export", "close", "--reason", "Duplicate", "--chat-file", chat),
                     ("042-sample-export", "close", "--chat-file", chat, "--chat-file", chat)):
            with self.subTest(args=args):
                code, err, _ = self.control(*args)
                self.assertEqual(code, 2)
                self.assertIn(factory.CONTROL_USAGE, err)
        self.assertEqual(json.dumps(self.board), before)
        path = self.home / "crlf.txt"
        path.write_bytes((MESSAGE + "\r\n").encode("utf-8"))
        code, err, _ = self.control("042-sample-export", "prioritize", "--chat-file", str(path))
        self.assertEqual(code, 0, err)
        self.assertEqual(self.job()["priority"]["quote"], MESSAGE)

    def test_prioritized_reads_the_chat_record(self):
        self.chat("042-sample-export", "prioritize")
        self.assertTrue(factory.prioritized(self.job()))
