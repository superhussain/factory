import json
import os
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
OPUS = "claude-opus-5-5"
RUNNING_EXAMPLE = """\
job 1017-add-x on host-a: 3 transcripts
model            input  output  cache_write  cache_read
claude-opus-5-5  1,200  48,000      210,000   3,400,000
"""
MODELS = {OPUS: {"input": 1200, "output": 48000, "cache_write": 210000, "cache_read": 3400000}}
HOST_B = {"at": "2026-01-02T12:00:00Z", "transcripts": 1,
          "models": {OPUS: {"input": 300, "output": 9100, "cache_write": 52000, "cache_read": 610000}}}


def turn(msg_id, model, i, o, w, r, side=False):
    return {"type": "assistant", "isSidechain": side,
            "message": {"id": msg_id, "model": model, "usage": {
                "input_tokens": i, "output_tokens": o, "cache_creation_input_tokens": w,
                "cache_read_input_tokens": r}}}


def prompt(text):
    return {"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}}


def write(path, *lines):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(line if isinstance(line, str) else json.dumps(line) + "\n" for line in lines),
                    encoding="utf-8")
    return path


class UsageTestCase(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.projects = self.root / "claude/projects"
        self.home = self.root / "home"
        self.home.mkdir()
        self.config()
        os.environ.update(CLAUDE_CONFIG_DIR=str(self.root / "claude"), FACTORY_HOME=str(self.home))
        worktree = self.projects / "-home-u--paseo-worktrees-ab12-factory-1017-add-x"
        write(worktree / "s1.jsonl", prompt("build it"),
              turn("m1", OPUS, 1000, 20000, 200000, 3000000),
              turn("m1", OPUS, 1000, 40000, 200000, 3000000),
              turn("side", OPUS, 9, 9, 9, 9, side=True),
              turn("syn", "<synthetic>", 0, 0, 0, 0))
        write(worktree / "s1/subagents/agent-a1.jsonl", prompt("review it"),
              turn("m2", OPUS, 100, 5000, 5000, 200000, side=True))
        outside = self.projects / "-home-u-dev-factory"
        write(outside / "o1.jsonl", prompt("orchestrate"), turn("m3", OPUS, 7, 7, 7, 7))
        write(outside / "o1/subagents/agent-b1.jsonl",
              {"type": "user", "isMeta": True, "message": {"content": "meta"}},
              prompt(f"Your full prompt is in {self.home}/prompts/alpha/1017-add-x/spec.txt"),
              turn("m4", OPUS, 100, 3000, 5000, 200000, side=True))
        write(outside / "o1/subagents/agent-b2.jsonl",
              prompt(f"Job folder: {self.home}/jobs/alpha/1017-add-xy"), turn("m5", OPUS, 5, 5, 5, 5, side=True))
        write(self.projects / "-home-u--paseo-worktrees-cd34-factory-1017-add-xy/x1.jsonl",
              prompt("other"), turn("m6", OPUS, 5, 5, 5, 5))

    def config(self, origin="https://factory.example.test", device="device: host-a\n"):
        (self.home / "config.md").write_text(f"---\napi_url: {origin}\n{device}---\n", encoding="utf-8")


class UsageScanTest(UsageTestCase):
    def test_dedupes_and_folds(self):
        path = write(self.root / "t.jsonl",
                     turn("a", OPUS, 1, 2, 3, 4), turn("a", OPUS, 1, 5, 3, 4), turn("b", OPUS, 10, 0, 0, 0),
                     turn("c", "<synthetic>", 0, 7, 0, 0), turn("d", "zero-model", 0, 0, 0, 0),
                     "{broken\n", "\n", turn("e", OPUS, 9, 9, 9, 9, side=True))
        models, bad = factory.usage_scan(path, True)
        self.assertEqual(bad, 1)
        self.assertEqual(models, {OPUS: {"input": 11, "output": 5, "cache_write": 3, "cache_read": 4},
                                  "other": {"input": 0, "output": 7, "cache_write": 0, "cache_read": 0}})
        self.assertEqual(factory.usage_scan(path, False)[0][OPUS]["input"], 20)

    def test_prompt(self):
        self.assertEqual(factory.usage_prompt(self.projects / "-home-u-dev-factory/o1/subagents/agent-b1.jsonl"),
                         f"Your full prompt is in {self.home}/prompts/alpha/1017-add-x/spec.txt")
        late = write(self.root / "late.jsonl", *[turn(f"x{n}", OPUS, 1, 1, 1, 1) for n in range(50)], prompt("late"))
        self.assertEqual(factory.usage_prompt(late), "")


class UsageTranscriptsTest(UsageTestCase):
    def test_sweep_session_counts_each_subagent_once(self):
        outside = self.projects / "-home-u-dev-factory"
        write(outside / "o2.jsonl", prompt(f"Sweep {self.home}/jobs/alpha/1017-add-x"), turn("m7", OPUS, 1, 1, 1, 1))
        write(outside / "o2/subagents/agent-c1.jsonl", prompt(f"Prompt: {self.home}/prompts/alpha/1017-add-x/b.txt"),
              turn("m8", OPUS, 1, 1, 1, 1, side=True))
        write(outside / "o2/subagents/agent-c2.jsonl", prompt(f"Prompt: {self.home}/prompts/alpha/1018-add-y/b.txt"),
              turn("m9", OPUS, 1, 1, 1, 1, side=True))

        def found(job_id):
            return {(p.relative_to(self.projects).as_posix(), main)
                    for p, main in factory.usage_transcripts(self.home, "alpha", job_id)}
        self.assertEqual(found("1017-add-x"), {
            ("-home-u--paseo-worktrees-ab12-factory-1017-add-x/s1.jsonl", True),
            ("-home-u--paseo-worktrees-ab12-factory-1017-add-x/s1/subagents/agent-a1.jsonl", False),
            ("-home-u-dev-factory/o1/subagents/agent-b1.jsonl", False),
            ("-home-u-dev-factory/o2.jsonl", True),
            ("-home-u-dev-factory/o2/subagents/agent-c1.jsonl", False)})
        self.assertEqual(found("1018-add-y"), {("-home-u-dev-factory/o2/subagents/agent-c2.jsonl", False)})


def stamped(line, ts):
    return dict(line, timestamp=ts)


class DailyTokensTest(UsageTestCase):
    MIDNIGHT = datetime(2026, 1, 1, tzinfo=timezone.utc)

    def setUp(self):
        super().setUp()
        self.job = self.projects / "-home-u--paseo-worktrees-ab12-factory-1017-add-x"
        d2 = stamped(turn("d2", OPUS, 10, 20, 30, 40), "2026-01-01T00:00:00.250Z")
        self.day = write(self.job / "day.jsonl",
                         stamped(turn("d1", OPUS, 1, 2, 3, 4), "2025-12-31T23:59:59.900Z"), d2, d2)

    def test_window(self):
        self.assertEqual(factory.daily_tokens(self.MIDNIGHT), 60)

    def test_outside_a_job_folder(self):
        write(self.projects / "-home-u-dev-factory/day.jsonl",
              stamped(turn("d3", OPUS, 500, 0, 0, 0), "2026-01-01T01:00:00.000Z"))
        self.assertEqual(factory.daily_tokens(self.MIDNIGHT), 60)

    def test_subagent(self):
        write(self.job / "day/subagents/agent-d.jsonl",
              stamped(turn("d4", OPUS, 0, 5, 0, 0, side=True), "2026-01-01T02:00:00.000Z"))
        self.assertEqual(factory.daily_tokens(self.MIDNIGHT), 65)

    def test_no_root(self):
        os.environ["CLAUDE_CONFIG_DIR"] = str(self.root / "missing")
        self.assertEqual(factory.daily_tokens(self.MIDNIGHT), 0)

    def test_since_filter(self):
        self.assertEqual(factory.usage_scan(self.day, True, "2026-01-01T00:00:00"),
                         ({OPUS: {"input": 10, "output": 20, "cache_write": 30, "cache_read": 40}}, 0))


class UsageCommandTest(UsageTestCase):
    def test_running_example(self):
        code, out, err = invoke(factory, ["usage", "alpha/1017-add-x"])
        self.assertEqual((code, out, err), (0, RUNNING_EXAMPLE, ""))

    def test_failures_warn(self):
        bad = self.projects / "-home-u-dev-factory/o1/subagents/agent-b1.jsonl"
        bad.write_text(bad.read_text(encoding="utf-8") + "{x\nnot json\n", encoding="utf-8")
        code, out, err = invoke(factory, ["usage", "alpha/1017-add-x"])
        self.assertEqual((code, out), (0, RUNNING_EXAMPLE))
        self.assertEqual(err, "factory.py: usage: 2 unreadable lines in agent-b1.jsonl\n")

        sub = self.projects / "-home-u--paseo-worktrees-ab12-factory-1017-add-x/s1/subagents/agent-a1.jsonl"
        sub.unlink()
        sub.mkdir()
        code, out, err = invoke(factory, ["usage", "alpha/1017-add-x"])
        self.assertEqual(code, 0)
        self.assertTrue(out.startswith("job 1017-add-x on host-a: 2 transcripts\n"))
        self.assertIn("factory.py: usage: cannot read agent-a1.jsonl\n", err)

        def boom(*_):
            raise KeyError("x")
        with patch.object(factory, "usage_scan", boom):
            code, out, err = invoke(factory, ["usage", "record", "alpha/1017-add-x"])
        self.assertEqual((code, out, err), (0, "", "factory.py: usage: scan failed (KeyError: 'x'); nothing recorded\n"))

        code, out, err = invoke(factory, ["usage", "alpha/1018-add-x"])
        self.assertEqual((code, out, err),
                         (0, "no Claude Code transcripts on host-a name 1018-add-x; nothing recorded\n", ""))
        os.environ["CLAUDE_CONFIG_DIR"] = str(self.root / "missing")
        code, out, _ = invoke(factory, ["usage", "alpha/1017-add-x"])
        self.assertEqual((code, out), (0, "no Claude Code transcripts on host-a name 1017-add-x; nothing recorded\n"))

    def test_usage_errors(self):
        for args in (["x", "alpha/1017-add-x"], ["record", "alpha"]):
            with self.subTest(args=args):
                code, out, err = invoke(factory, ["usage", *args])
                self.assertEqual((code, out), (2, ""))
        self.assertIn("usage: factory.py usage [record] <project-slug>/<job-id>",
                      invoke(factory, ["usage", "x", "alpha/1017-add-x"])[2])
        self.assertIn("bad target", invoke(factory, ["usage", "alpha"])[2])


class UsageRecordTest(UsageTestCase):
    def board(self, job):
        return {"updated": "2026-01-02T12:00:00Z", "jobs": [job]}

    def record(self, board):
        with http_fixture({"GET /api/board": (200, board), "PUT /api/board": (200, b"ok")}) as server:
            self.config(server.origin)
            result = invoke(factory, ["usage", "record", "alpha/1017-add-x"])
        puts = [json.loads(body) for method, _, _, body in server.requests if method == "PUT"]
        return result, puts

    def test_keeps_other_hosts(self):
        job = {"id": "1017-add-x", "project": "alpha", "stage": "build", "tokens": {"host-b": HOST_B}}
        (code, out, err), puts = self.record(self.board(job))
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, RUNNING_EXAMPLE + "recorded host-a tokens on 1017-add-x\n")
        saved = puts[0]["jobs"][0]
        self.assertEqual(saved["tokens"]["host-b"], HOST_B)
        self.assertEqual(saved["tokens"]["host-a"], {"at": saved["updated"], "transcripts": 3, "models": MODELS})
        self.assertNotEqual(saved["updated"], "2026-01-02T12:00:00Z")

        stale = {**saved, "tokens": {**saved["tokens"], "host-a": {"at": "x", "transcripts": 9, "models": {}}}}
        (code, _, _), puts = self.record(self.board(stale))
        self.assertEqual(code, 0)
        self.assertEqual(puts[0]["jobs"][0]["tokens"]["host-a"]["models"], MODELS)
        self.assertEqual(puts[0]["jobs"][0]["tokens"]["host-b"], HOST_B)

    def test_errors(self):
        (code, _, err), puts = self.record(self.board({"id": "1999-other", "project": "alpha"}))
        self.assertEqual((code, puts), (4, []))
        self.assertIn("no job alpha/1017-add-x on the board", err)
        (code, _, err), puts = self.record(self.board({"id": "1017-add-x", "project": "beta"}))
        self.assertEqual((code, puts), (2, []))
        self.assertIn("1017-add-x is not assigned to alpha", err)
        for device in ("", "device: -bad\n"):
            with self.subTest(device=device):
                self.config(device=device)
                code, out, err = invoke(factory, ["usage", "record", "alpha/1017-add-x"])
                self.assertEqual((code, out), (2, ""))
                self.assertIn(f"set device: in {self.home / 'config.md'}; "
                              "pair this host with factory.py host pair <id> <code>", err)


if __name__ == "__main__":
    unittest.main()
