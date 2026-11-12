import json

from test_suggest import SuggestCase, factory

MESSAGE = "prioritize 042-sample-export"


class ControlTest(SuggestCase):
    def setUp(self):
        super().setUp()
        self.board["jobs"].append({"id": "042-sample-export", "stage": "build", "project": "sample"})

    def job(self):
        return next(j for j in self.board["jobs"] if j["id"] == "042-sample-export")

    def control(self, *args):
        self.output.seek(0)
        self.output.truncate()
        return self.run_human(factory.COMMANDS["control"][0], *args, tty=False)

    def test_prioritize_writes_the_chat_record(self):
        code, err, _ = self.control("042-sample-export", "prioritize", "--chat", MESSAGE)
        self.assertEqual(code, 0, err)
        self.assertEqual(json.loads(self.output.getvalue()),
                         {"id": "042-sample-export", "action": "prioritize", "result": "prioritized"})
        priority = self.job()["priority"]
        self.assertEqual(list(priority.items()), [
            ("at", priority["at"]), ("via", "chat"), ("quote", MESSAGE), ("by", "device")])
        self.assertEqual(self.job()["updated"], priority["at"])

    def test_second_prioritize_is_unchanged(self):
        self.control("042-sample-export", "prioritize", "--chat", MESSAGE)
        before, puts = json.dumps(self.board), self.puts()
        code, err, _ = self.control("042-sample-export", "prioritize", "--chat", MESSAGE)
        self.assertEqual(code, 0, err)
        self.assertEqual(json.loads(self.output.getvalue())["result"], "unchanged")
        self.assertEqual((json.dumps(self.board), self.puts()), (before, puts))

    def test_unprioritize_removes_priority(self):
        self.control("042-sample-export", "prioritize", "--chat", MESSAGE)
        code, err, _ = self.control("042-sample-export", "unprioritize", "--chat", "unprioritize 042-sample-export")
        self.assertEqual(code, 0, err)
        self.assertEqual(json.loads(self.output.getvalue()),
                         {"id": "042-sample-export", "action": "unprioritize", "result": "unprioritized"})
        self.assertNotIn("priority", self.job())
        puts = self.puts()
        self.control("042-sample-export", "unprioritize", "--chat", "unprioritize 042-sample-export")
        self.assertEqual(json.loads(self.output.getvalue())["result"], "unchanged")
        self.assertEqual(self.puts(), puts)

    def test_refuses_missing_rider_projectless_and_pr_open_jobs(self):
        self.board["jobs"] += [
            {"id": "043-rider", "stage": "build", "project": "sample",
             "refs": [{"kind": factory.BATCH_REF, "id": "042-sample-export"}]},
            {"id": "044-no-project", "stage": "idea"},
            {"id": "045-open", "stage": "pr-open", "project": "sample"}]
        before, puts = json.dumps(self.board), self.puts()
        for job_id, want, why in (
                ("099-missing", 4, "no job 099-missing on the board. Nothing was written."),
                ("043-rider", 2, "043-rider cannot be prioritized: it is a rider of 042-sample-export. Nothing was written."),
                ("044-no-project", 2, "044-no-project cannot be prioritized: it has no project. Nothing was written."),
                ("045-open", 2, "045-open cannot be prioritized: it is at pr-open. Nothing was written.")):
            with self.subTest(job=job_id):
                code, err, _ = self.control(job_id, "prioritize", "--chat", MESSAGE)
                self.assertEqual(code, want, err)
                self.assertIn(why, err)
        self.assertEqual((json.dumps(self.board), self.puts()), (before, puts))

    def test_usage(self):
        before = json.dumps(self.board)
        for args in (("042-sample-export", "prioritize", "--chat", "  "),
                     ("042-sample-export", "pause", "--chat", MESSAGE),
                     ("042-sample-export", "prioritize", "--note", MESSAGE)):
            with self.subTest(args=args):
                code, err, _ = self.control(*args)
                self.assertEqual(code, 2)
                self.assertIn(factory.CONTROL_USAGE, err)
        self.assertEqual(json.dumps(self.board), before)
        path = self.home / "chat.txt"
        path.write_bytes((MESSAGE + "\r\n").encode("utf-8"))
        code, err, _ = self.control("042-sample-export", "prioritize", "--chat-file", str(path))
        self.assertEqual(code, 0, err)
        self.assertEqual(self.job()["priority"]["quote"], MESSAGE)

    def test_prioritized_reads_the_chat_record(self):
        self.control("042-sample-export", "prioritize", "--chat", MESSAGE)
        self.assertTrue(factory.prioritized(self.job()))
