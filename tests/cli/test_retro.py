import json
import os
import shutil
import tempfile
import time
from pathlib import Path

from support import ROOT, IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
FIXTURE = ROOT / "tests/fixtures/retro"
SLUG = "-home-u--paseo-worktrees-ab12-factory-1017-add-x"


def assistant(mid, *tools, usage=None):
    content = [{"type": "tool_use", "id": f"{mid}-{n}", "name": name, "input": inp}
               for n, (name, inp) in enumerate(tools)]
    return {"type": "assistant", "timestamp": "2026-11-10T10:00:00Z",
            "message": {"id": mid, "model": "claude-opus-5-5", "usage": usage or {"input_tokens": 1},
                        "content": content}}


def result(mid, text, is_error=True, n=0):
    return {"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": f"{mid}-{n}", "is_error": is_error, "content": text}]}}


def bash(cmd):
    return ("Bash", {"command": cmd})


class RetroDigestTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        shutil.copytree(FIXTURE, self.root / "claude")
        self.projects = self.root / "claude/projects"
        for path in self.projects.rglob("*.jsonl"):
            os.utime(path)
        self.home = self.root / "home"
        self.home.mkdir()
        os.environ.update(CLAUDE_CONFIG_DIR=str(self.root / "claude"), FACTORY_HOME=str(self.home))

    def digest(self, *args):
        code, out, err = invoke(factory, ["retro", "digest", *args])
        self.assertEqual((code, err), (0, ""))
        return [json.loads(line) for line in out.splitlines()]

    def session(self, entries, slug="-home-u-scratch", name="s2"):
        path = self.projects / slug / f"{name}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        lines = [{"timestamp": "2026-11-10T10:00:00Z", **e} for e in entries]
        path.write_text("".join(json.dumps(e) + "\n" for e in lines), encoding="utf-8")
        return path

    def scan(self, entries):
        return factory.retro_scan(self.session(entries), True)

    def test_usage_dedupes_by_message_id(self):
        row = self.digest()[0]
        self.assertEqual(row["turns"], 2)
        self.assertEqual(row["tokens"], {"cache_read": 170000, "cache_write": 0, "uncached_input": 3000,
                                         "output": 600})

    def test_subagent_errors_by_type(self):
        row = self.digest()[0]
        builder = row["subagents"]["by_type"]["factory-builder"]
        self.assertEqual(row["subagents"]["count"], 1)
        self.assertEqual((builder["count"], builder["turns"], builder["errors"]), (1, 1, 1))
        self.assertEqual(builder["tokens"]["cache_write"], 19000)
        self.assertEqual(row["errors"]["count"], 0)
        (self.projects / SLUG / "s1/subagents/agent-b1.meta.json").unlink()
        self.assertEqual(self.digest()[0]["subagents"]["by_type"]["unknown"]["errors"], 1)

    def test_list_form_human_messages(self):
        scan = self.scan([
            {"type": "user", "message": {"content": [{"type": "text", "text": "ship it"}]}},
            {"type": "user", "message": {"content": "plain words"}},
            {"type": "user", "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}},
            {"type": "user", "message": {"content": "<command-name>/clear</command-name>"}},
            {"type": "user", "isMeta": True, "message": {"content": "meta"}},
            {"type": "user", "message": {"content": [{"type": "text", "text": ""}]}},
            assistant("a1", bash("ls")), result("a1", "fine", False)])
        self.assertEqual((scan["human_msgs"], scan["interrupts"]), (3, 1))
        self.assertEqual(self.digest()[0]["human_msgs"], 1)

    def test_denials_and_blocked_sleep(self):
        texts = ["Permission to use Bash has been denied by the Claude Code auto mode classifier.",
                 "The user doesn't want to proceed with this tool use.",
                 "Blocked: sleep 30 is not allowed",
                 "Exit code 1 Permission denied"]
        entries = []
        for n, text in enumerate(texts):
            entries += [assistant(f"a{n}", bash("ls")), result(f"a{n}", text)]
        scan = self.scan(entries + [assistant("z", bash("ls")), result("z", "ok", False)])
        self.assertEqual((scan["denials"]["count"], scan["denials"]["blocked_sleep"]), (2, 1))
        self.assertEqual(scan["errors"]["count"], 1)
        self.assertEqual(list(scan["errors"]["patterns"]), ["Exit code N Permission denied"])

    def test_final_self_termination_is_benign(self):
        archive = bash("python3 factory.py archive-workspace factory/1017-add-x")
        paseo = ("mcp__paseo__archive_workspace", {"id": "w1"})
        final = self.scan([assistant("a1", archive), result("a1", "Exit code 137")])
        self.assertEqual((final["errors"]["count"], final["errors"]["benign"]), (0, 1))
        closed = self.scan([assistant("a1", paseo), result("a1", "MCP error: Connection closed")])
        self.assertEqual((closed["errors"]["count"], closed["errors"]["benign"]), (0, 1))
        middle = self.scan([assistant("a1", archive), result("a1", "Exit code 137"),
                            assistant("a2", bash("ls")), result("a2", "ok", False)])
        self.assertEqual((middle["errors"]["count"], middle["errors"]["benign"]), (1, 0))
        row = self.digest()[0]
        self.assertEqual((row["errors"]["count"], row["errors"]["benign"]), (0, 1))

    def test_repeated_cmds_strip_assignments(self):
        cmds = ["FOO=1 git status", "git status", "A='x y' B=2 git status", "git log"]
        scan = self.scan([assistant(f"a{n}", bash(c)) for n, c in enumerate(cmds)])
        self.assertEqual(scan["repeated_cmds"], {"git status": 3})
        self.assertEqual(factory.retro_command_key("FOO=1 git status"), factory.retro_command_key("git status"))

    def test_context_and_markers(self):
        row = self.digest()[0]
        self.assertEqual(row["context"], {"first_turn": 82000, "mean_per_turn": 86500})
        self.assertEqual(row["agent_prompts"], {"count": 1, "chars": 40})
        self.assertEqual(list(row["first_turn_attachments"]), ["skill_listing"])
        self.assertGreater(row["first_turn_attachments"]["skill_listing"], 100)
        self.assertIsNone(row["review_round_max"])
        scan = self.scan([
            {"type": "attachment", "attachment": {"type": "skill_listing", "content": "x"}},
            assistant("a1", bash("until [ -f /tmp/done ]; do sleep 5; done"),
                      ("Read", {"file_path": "/tmp/tasks/b7-x.output"}),
                      bash("stat -c %Y round-2/review.md"), ("Read", {"file_path": "jobs/round-3/review.md"})),
            {"type": "attachment", "attachment": {"type": "late", "content": "y"}},
            result("a1", "zsh: command not found: foo\nNode.js v24.0.0"),
            result("a1", "zsh: no matches found: *.png", n=1)])
        self.assertEqual(scan["polls"], {"until_file": 1, "task_output": 1, "quiet_window": 1})
        self.assertEqual(scan["review_round_max"], 3)
        self.assertEqual(list(scan["first_turn_attachments"]), ["skill_listing"])
        self.assertEqual(scan["env_noise"], {"winsize_crash": 0, "command_not_found": 1,
                                             "zsh_no_matches": 1, "node_crash": 1})
        sub = self.projects / SLUG / "s1/subagents/agent-b2.jsonl"
        sub.write_text(json.dumps(result("x", "undefined method `winsize' for nil")) + "\n", encoding="utf-8")
        self.assertEqual(self.digest()[0]["env_noise"]["winsize_crash"], 1)

    def test_error_patterns_normalized(self):
        entries = []
        for n in range(20):
            text = f"cat: /tmp/run{n}/x.txt: No such file or directory (exit {n})"
            entries += [assistant(f"a{n}", bash("cat")), result(f"a{n}", text)]
            entries += [assistant(f"b{n}", bash("cat")), result(f"b{n}", f"error kind {chr(97 + n)}")]
        scan = self.scan(entries)
        self.assertEqual(scan["errors"]["count"], 40)
        self.assertEqual(scan["errors"]["patterns"]["cat: <path>: No such file or directory (exit N)"], 20)
        self.assertEqual(len(scan["errors"]["patterns"]), 21)

    def test_excerpts_redacted_and_clipped(self):
        github = "ghp_" + "A" * 36
        key = "sk-" + "b" * 30
        texts = [f"push failed with {github} " + "y " * 200, f"curl -H password={key}x", "api_key: hunter-value"]
        entries = []
        for n, text in enumerate(texts):
            entries += [assistant(f"a{n}", bash(text)), result(f"a{n}", text)]
        entries += [assistant("d", bash("ls")), result("d", f"Permission to use Bash with {github}")]
        self.session(entries, slug=SLUG, name="s3")
        out = json.dumps(self.digest())
        for secret in (github, key, "hunter"):
            self.assertNotIn(secret, out)
        self.assertIn("[redacted]", out)

        def strings(value):
            if isinstance(value, dict):
                for k, v in value.items():
                    yield k
                    yield from strings(v)
            elif isinstance(value, list):
                for v in value:
                    yield from strings(v)
            elif isinstance(value, str):
                yield value
        self.assertTrue(all(len(s) <= 160 for s in strings(self.digest())))
        self.assertEqual(len(factory.retro_pattern("z" * 400)), 160)

    def test_window_job_and_host(self):
        old = self.session([{"type": "user", "message": {"content": "old"}}], name="old")
        os.utime(old, (time.time() - 3 * 86400,) * 2)
        self.session([{"type": "user", "message": {"content": "hi"}, "timestamp": "2026-11-10T08:00:00Z"}])
        self.session([{"type": "summary", "timestamp": None}], name="empty")
        (self.home / "config.md").write_text("---\ndevice: example-host\n---\n", encoding="utf-8")
        rows = self.digest()
        self.assertEqual([(r["session"], r["job"]) for r in rows], [("s2", "ad-hoc"), ("s1", "1017")])
        self.assertEqual({r["host"] for r in rows}, {"example-host"})
        self.assertEqual(sorted(r["session"] for r in self.digest("--days", "5")), ["old", "s1", "s2"])
        (self.home / "config.md").unlink()
        self.assertEqual(self.digest()[0]["host"], factory.socket.gethostname())

    def test_usage_errors(self):
        for args in (["retro"], ["retro", "push"], ["retro", "digest", "--days"], ["retro", "digest", "--days", "0"],
                     ["retro", "digest", "--days", "91"], ["retro", "digest", "--days", "x"], ["retro", "digest", "--days", "²"],
                     ["retro", "digest", "--weeks", "2"]):
            code, out, _ = invoke(factory, args)
            self.assertEqual((code, out), (2, ""), args)
        self.assertIn("usage: factory.py retro digest [--days N], N from 1 to 90", invoke(factory, ["retro", "push"])[2])
        os.environ["CLAUDE_CONFIG_DIR"] = str(self.root / "missing")
        code, out, err = invoke(factory, ["retro", "digest"])
        self.assertEqual((code, out), (4, ""))
        self.assertIn("no Claude Code transcripts at", err)
