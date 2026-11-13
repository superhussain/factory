import json

from test_suggest import SuggestCase, factory

MESSAGE = "prioritize 042-sample-export"
BABYSIT = "babysit 042-sample-export"
MOVE = "move 043-sample-export to beta"
RERUN = "rerun 043-sample-export"
PR = "https://github.com/example/sample/pull/44"


class ControlTest(SuggestCase):
    def setUp(self):
        super().setUp()
        self.board["jobs"].append({"id": "042-sample-export", "stage": "build", "project": "sample"})
        self.hosts = [{"id": "alpha", "state": "active"}, {"id": "beta", "state": "active"},
                      {"id": "gamma", "state": "retired"}]
        self.pickup = {"enabled": True, "hosts": ["alpha", "beta"]}

    def request(self, home, method, path, body=None, ok=(200,), headers=None):
        if (method, path) == ("GET", "/api/hosts"):
            return 200, {}, json.dumps(self.hosts)
        if (method, path) == ("GET", "/api/pipeline/effective/sample"):
            return 200, {}, json.dumps({"pickup": self.pickup})
        return super().request(home, method, path, body, ok, headers)

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

    def test_close_over_a_malformed_close_is_unchanged(self):
        for close in ({"at": "2026-01-02T12:00:00Z"}, True):
            with self.subTest(close=close):
                self.job()["close"] = close
                puts = self.puts()
                code, err, _ = self.close("042-sample-export")
                self.assertEqual(code, 0, err)
                self.assertEqual(self.result(), {"id": "042-sample-export", "action": "close", "result": "unchanged",
                                                 "reason": None})
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

    def find(self, job_id):
        return next(j for j in self.board["jobs"] if j["id"] == job_id)

    def move(self, job_id, host, message=MOVE):
        return self.control(job_id, "move", host, "--chat-file", self.file(message))

    def test_babysit_writes_the_chat_record(self):
        self.job().update(stage="pr-open", pr=PR, needs="Approve the PR?", escalated=True, note="waiting")
        code, err, _ = self.chat("042-sample-export", "babysit", BABYSIT)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(), {"id": "042-sample-export", "action": "babysit", "result": "queued"})
        job = self.job()
        stamp = job["babysit"]["at"]
        record = [("at", stamp), ("via", "chat"), ("quote", BABYSIT), ("by", "device")]
        self.assertEqual(list(job["babysit"].items()), record)
        self.assertEqual(list(job["priority"].items()), record)
        self.assertNotIn("needs", job)
        self.assertNotIn("escalated", job)
        self.assertEqual((job["note"], job["updated"]), ("babysitting PR #44", stamp))
        self.assertTrue(factory.babysitting(job))
        before, puts = json.dumps(self.board), self.puts()
        code, err, _ = self.chat("042-sample-export", "babysit", BABYSIT)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(), {"id": "042-sample-export", "action": "babysit", "result": "unchanged"})
        self.assertEqual((json.dumps(self.board), self.puts()), (before, puts))

    def test_babysit_keeps_an_earlier_priority(self):
        self.job().update(stage="pr-open", pr=PR, priority={"at": "2026-01-02T12:00:00Z"})
        code, err, _ = self.chat("042-sample-export", "babysit", BABYSIT)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.job()["priority"], {"at": "2026-01-02T12:00:00Z"})

    def test_babysit_refusals(self):
        open_pr = {"stage": "pr-open", "project": "sample", "pr": PR}
        self.board["jobs"] += [
            {"id": "043-no-project", "stage": "pr-open", "pr": PR},
            dict(open_pr, id="044-rider", refs=[{"kind": factory.BATCH_REF, "id": "042-sample-export"}]),
            dict(open_pr, id="045-no-pr", pr="https://example.test/pull/44"),
            dict(open_pr, id="046-paused", paused={"at": "2026-01-02T12:00:00Z"}),
            dict(open_pr, id="047-closing", close={"reason": "Not needed", "at": "2026-01-02T12:00:00Z"})]
        self.assert_refused(lambda job_id: self.chat(job_id, "babysit", BABYSIT), (
            ("099-missing", 4, "no job 099-missing on the board. Nothing was written."),
            ("043-no-project", 2, "043-no-project cannot be babysat: it has no project. Nothing was written."),
            ("042-sample-export", 2, "042-sample-export cannot be babysat: it is at build. Nothing was written."),
            ("044-rider", 2, "044-rider cannot be babysat: it is a rider of 042-sample-export. Nothing was written."),
            ("045-no-pr", 2, "045-no-pr cannot be babysat: it has no GitHub pull request URL. Nothing was written."),
            ("046-paused", 2, "046-paused cannot be babysat: it is paused. Nothing was written."),
            ("047-closing", 2, "047-closing cannot be babysat: it has a close queued. Nothing was written.")))

    def claimed(self, **extra):
        job = dict({"id": "043-sample-export", "stage": "build", "project": "sample",
                    "claim": {"device": "alpha", "session": "s-alpha", "seen": "2026-01-02T12:00:00Z"},
                    "paseo": {"agent": "agent-alpha", "server": "server-alpha"}}, **extra)
        self.board["jobs"].append(job)
        return job

    def test_move_writes_the_chat_record_and_drops_the_claim(self):
        self.claimed()
        code, err, _ = self.move("043-sample-export", "beta")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(),
                         {"id": "043-sample-export", "action": "move", "result": "queued", "to": "beta"})
        job = self.find("043-sample-export")
        move = job["move"]
        self.assertEqual(list(move.items()), [
            ("from", "alpha"), ("session", "s-alpha"), ("agent", "agent-alpha"), ("server", "server-alpha"),
            ("to", "beta"), ("at", move["at"]), ("via", "chat"), ("quote", MOVE), ("by", "device")])
        self.assertNotIn("claim", job)
        self.assertNotIn("paseo", job)
        self.assertEqual(job["updated"], move["at"])
        self.assertTrue(factory.moved_here(job, "beta"))
        self.assertEqual([x["from"] for x in factory.queued_stops(move)], ["alpha"])
        before, puts = json.dumps(self.board), self.puts()
        code, err, _ = self.move("043-sample-export", "beta")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result()["result"], "unchanged")
        self.assertEqual((json.dumps(self.board), self.puts()), (before, puts))

    def test_move_refusals(self):
        self.claimed()
        self.claimed(id="044-stop-queued", move={"from": "beta", "to": "alpha", "at": "2026-01-02T11:00:00Z"})
        self.board["jobs"] += [
            {"id": "045-no-project", "stage": "build"},
            {"id": "046-open", "stage": "pr-open", "project": "sample"},
            {"id": "047-rider", "stage": "build", "project": "sample",
             "refs": [{"kind": factory.BATCH_REF, "id": "043-sample-export"}]}]
        self.assert_refused(lambda arg: self.move(*arg), (
            (("099-missing", "beta"), 4, "no job 099-missing on the board. Nothing was written."),
            (("045-no-project", "beta"), 2,
             "045-no-project cannot be moved to beta: it has no project. Nothing was written."),
            (("046-open", "beta"), 2, "046-open cannot be moved to beta: it is at pr-open. Nothing was written."),
            (("047-rider", "beta"), 2,
             "047-rider cannot be moved to beta: it is a rider of 043-sample-export. Nothing was written."),
            (("043-sample-export", "alpha"), 2,
             "043-sample-export cannot be moved to alpha: it already runs on alpha. Nothing was written."),
            (("044-stop-queued", "beta"), 2, "044-stop-queued cannot be moved to beta: a stop is still queued on "
             "beta; pick another host. Nothing was written."),
            (("043-sample-export", "gamma"), 2,
             "043-sample-export cannot be moved to gamma: gamma is not an active host. Nothing was written."),
            (("043-sample-export", "delta"), 2,
             "043-sample-export cannot be moved to delta: delta is not an active host. Nothing was written.")))
        self.pickup = {"enabled": True, "hosts": ["alpha"]}
        self.assert_refused(lambda arg: self.move(*arg), (
            (("043-sample-export", "beta"), 2, "043-sample-export cannot be moved to beta: beta does not run pickup "
             "for sample. Nothing was written."),))
        self.pickup = "on"
        self.assert_refused(lambda arg: self.move(*arg), (
            (("043-sample-export", "beta"), 2, "043-sample-export cannot be moved to beta: malformed pickup in the "
             "pipeline view. Nothing was written."),))

    def test_rerun_writes_a_self_move(self):
        self.claimed()
        code, err, _ = self.chat("043-sample-export", "rerun", RERUN)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(),
                         {"id": "043-sample-export", "action": "rerun", "result": "queued", "to": "alpha"})
        job = self.find("043-sample-export")
        move = job["move"]
        self.assertEqual(list(move.items()), [
            ("from", "alpha"), ("session", "s-alpha"), ("agent", "agent-alpha"), ("server", "server-alpha"),
            ("to", "alpha"), ("at", move["at"]), ("via", "chat"), ("quote", RERUN), ("by", "device")])
        self.assertNotIn("claim", job)
        self.assertFalse(factory.moved_here(job, "alpha"))
        before, puts = json.dumps(self.board), self.puts()
        code, err, _ = self.chat("043-sample-export", "rerun", RERUN)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.result(),
                         {"id": "043-sample-export", "action": "rerun", "result": "unchanged", "to": "alpha"})
        self.assertEqual((json.dumps(self.board), self.puts()), (before, puts))
        move["stopped"] = "2026-01-02T12:30:00Z"
        self.assertTrue(factory.moved_here(job, "alpha"))

    def test_rerun_refusals(self):
        self.claimed()
        for job_id, extra in (("044-no-claim", {}), ("045-paused", {"paused": {"at": "2026-01-02T12:00:00Z"}}),
                              ("046-needs", {"needs": "Pick A or B"}),
                              ("047-retired", {"claim": {"device": "gamma"}}),
                              ("048-unlisted", {"claim": {"device": "beta"}})):
            self.claimed(id=job_id, **extra)
        self.find("044-no-claim").pop("claim")
        self.board["jobs"] += [
            {"id": "049-no-project", "stage": "build", "claim": {"device": "alpha"}},
            {"id": "050-open", "stage": "pr-open", "project": "sample", "claim": {"device": "alpha"}},
            {"id": "051-rider", "stage": "build", "project": "sample", "claim": {"device": "alpha"},
             "refs": [{"kind": factory.BATCH_REF, "id": "043-sample-export"}]}]
        self.pickup = {"enabled": True, "hosts": ["alpha"]}
        tail = ", so nothing would restart it; move it to another host instead. Nothing was written."
        self.assert_refused(lambda job_id: self.chat(job_id, "rerun", RERUN), (
            ("099-missing", 4, "no job 099-missing on the board. Nothing was written."),
            ("049-no-project", 2, "049-no-project cannot be rerun: it has no project. Nothing was written."),
            ("050-open", 2, "050-open cannot be rerun: it is at pr-open. Nothing was written."),
            ("051-rider", 2, "051-rider cannot be rerun: it is a rider of 043-sample-export. Nothing was written."),
            ("044-no-claim", 2, "044-no-claim cannot be rerun: it has no claim, so no host runs it. Nothing was written."),
            ("045-paused", 2, "045-paused cannot be rerun: it is paused. Nothing was written."),
            ("046-needs", 2, "046-needs cannot be rerun: it is waiting on a person. Nothing was written."),
            ("047-retired", 2, "047-retired cannot be rerun: gamma is not an active host" + tail),
            ("048-unlisted", 2, "048-unlisted cannot be rerun: beta does not run pickup for sample" + tail)))

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
                     ("042-sample-export", "close", "--chat-file", chat, "--chat-file", chat),
                     ("042-sample-export", "move"),
                     ("042-sample-export", "move", "--chat-file", chat),
                     ("042-sample-export", "move", "beta"),
                     ("042-sample-export", "move", "be/ta", "--chat-file", chat),
                     ("042-sample-export", "move", "beta", "--chat-file", chat, "--reason-file", reason),
                     ("042-sample-export", "rerun", "beta", "--chat-file", chat),
                     ("042-sample-export", "rerun"),
                     ("042-sample-export", "babysit", "--chat-file", chat, "--chat-file", chat)):
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
