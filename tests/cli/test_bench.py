import json
import os
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from unittest.mock import Mock, patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

USAGE = ("factory.py: usage: factory.py bench score <cases-file> <arm-folder>... | "
         "bench replay <cases-file> <arm-folder> --role <reviewer> [--model <model>]\n")
HEAD = "3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d"


def case(n, name, source, defects):
    return {"id": f"{n}-{name}", "pr": f"https://github.com/example/alpha/pull/{n % 1000}", "head": HEAD,
            "source": source, "defects": defects}


EXAMPLE_CASES = {"cases": [
    case(1018, "add-y", "reworked", [
        {"path": "worker/export.js", "lines": [40, 52], "severity": "high", "note": "optional"},
        {"path": "docs/device.md", "lines": [12, 12], "severity": "low"}]),
    case(1019, "add-z", "reverted", [{"path": "worker/board.js", "lines": [88, 95], "severity": "medium"}])]}


def run(seconds, input, output, cache_write, cache_read=0):
    return {"seconds": seconds, "models": {"model-a": {
        "input": input, "output": output, "cache_write": cache_write, "cache_read": cache_read}}}


# arm -> case id -> (review files, run.json or None)
EXAMPLE_ARMS = {
    "opus-medium": {
        "1018-add-y": ({"review-code.md": "VERDICT: CHANGES\n\n- `worker/export.js:44` drops the header row.\n",
                        "review-docs.md": "VERDICT: PASS\n\n- `docs/device.md:12` reads fine.\n"},
                       run(300, 200000, 20000, 30000, 999999)),
        "1019-add-z": ({"review-code.md": "VERDICT: CHANGES\n\n- worker/board.js:90-92 loses the lock.\n"},
                       run(240, 150000, 3000, 10000))},
    "sonnet-low": {
        "1018-add-y": ({"review-code.md": "VERDICT: PASS\n\n- `worker/export.js:44` looks fine.\n"},
                       run(220, 100000, 20000, 30000)),
        "1019-add-z": ({"review-code.md": "VERDICT: CHANGES\n\n- board.js:95 loses the lock.\n"}, None)}}


def severity(high, medium, low):
    return {"high": {"caught": high, "of": 1}, "medium": {"caught": medium, "of": 1}, "low": {"caught": low, "of": 1}}


EXAMPLE_OUT = {"cases": 2, "defects": 3, "weights": {"high": 3, "medium": 2, "low": 1}, "arms": [
    {"arm": "opus-medium", "weighted_recall": 0.83, "caught": 2, "severity": severity(1, 1, 0),
     "missed": ["1018-add-y docs/device.md:12-12"], "tokens": 413000, "seconds": 540, "runs": 2,
     "missing": [], "no_run": []},
    {"arm": "sonnet-low", "weighted_recall": 0.33, "caught": 1, "severity": severity(0, 1, 0),
     "missed": ["1018-add-y worker/export.js:40-52", "1018-add-y docs/device.md:12-12"],
     "tokens": 150000, "seconds": 220, "runs": 1, "missing": [], "no_run": ["1019-add-z"]}]}


def write_example(root, cases=EXAMPLE_CASES, arms=EXAMPLE_ARMS):
    """Write the cases file and arm folders under root, return (cases file, [arm folders])."""
    root = Path(root)
    cases_file = root / "reviewer-cases.json"
    cases_file.write_text(json.dumps(cases), encoding="utf-8")
    folders = []
    for arm, runs in arms.items():
        folder = root / arm
        folder.mkdir()
        for case_id, (reviews, run_json) in runs.items():
            (folder / case_id).mkdir()
            for name, text in reviews.items():
                (folder / case_id / name).write_text(text, encoding="utf-8")
            if run_json is not None:
                (folder / case_id / "run.json").write_text(json.dumps(run_json), encoding="utf-8")
        folders.append(str(folder))
    return str(cases_file), folders


class BenchCase(IsolatedTestCase):
    def setUp(self):
        self.module = load_runtime()
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)

    def bench(self, *args):
        return invoke(self.module, ["bench", *args])


class BenchScoreTest(BenchCase):
    def test_running_example(self):
        cases_file, folders = write_example(self.root)
        code, out, err = self.bench("score", cases_file, *folders)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), EXAMPLE_OUT)
        self.assertEqual(out, json.dumps(EXAMPLE_OUT, indent=2) + "\n")

    def test_offline(self):
        cases_file, folders = write_example(self.root)

        def refuse(*args, **kwargs):
            raise AssertionError("bench score must stay offline")

        with patch.object(self.module, "api", refuse), patch.object(self.module, "factory_home", refuse):
            code, out, err = self.bench("score", cases_file, *folders)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), EXAMPLE_OUT)

    def test_arms_print_in_argument_order(self):
        cases_file, folders = write_example(self.root)
        code, out, _ = self.bench("score", cases_file, *reversed(folders))
        self.assertEqual(code, 0)
        self.assertEqual([a["arm"] for a in json.loads(out)["arms"]], ["sonnet-low", "opus-medium"])

    def test_matching_rows(self):
        one = {"cases": [case(1018, "add-y", "reworked",
                              [{"path": "worker/export.js", "lines": [40, 52], "severity": "high"}])]}
        rows = [("VERDICT: CHANGES", "worker/export.js:44", 1),
                ("VERDICT: CHANGES", "export.js:50", 1),
                ("VERDICT: CHANGES", "worker/export.js:60", 0),
                ("VERDICT: PASS", "worker/export.js:44", 0),
                ("VERDICT: CHANGES", "worker/export.js:30-41", 1),
                ("VERDICT: CHANGES", "other/export.js:44", 0)]
        for n, (verdict, cite, caught) in enumerate(rows):
            with self.subTest(verdict=verdict, cite=cite):
                root = self.root / str(n)
                root.mkdir()
                arms = {"arm": {"1018-add-y": ({"review-code.md": f"{verdict}\n\n- `{cite}` breaks.\n"}, None)}}
                cases_file, folders = write_example(root, one, arms)
                code, out, err = self.bench("score", cases_file, *folders)
                self.assertEqual((code, err), (0, ""))
                arm = json.loads(out)["arms"][0]
                self.assertEqual((arm["caught"], arm["weighted_recall"]), (caught, float(caught)))

    def test_missing_case_folder_misses_every_defect(self):
        arms = {"opus-medium": {"1018-add-y": EXAMPLE_ARMS["opus-medium"]["1018-add-y"]}}
        cases_file, folders = write_example(self.root, arms=arms)
        code, out, _ = self.bench("score", cases_file, *folders)
        self.assertEqual(code, 0)
        arm = json.loads(out)["arms"][0]
        self.assertEqual(arm["missing"], ["1019-add-z"])
        self.assertEqual(arm["no_run"], ["1019-add-z"])
        self.assertEqual(arm["missed"], ["1018-add-y docs/device.md:12-12", "1019-add-z worker/board.js:88-95"])
        self.assertEqual((arm["caught"], arm["weighted_recall"], arm["runs"]), (1, 0.5, 1))

    def test_invalid_run_json_counts_as_no_run(self):
        cases_file, folders = write_example(self.root)
        run_json = Path(folders[0]) / "1019-add-z" / "run.json"
        for text in ("not json", '{"seconds": Infinity, "models": {}}', '{"seconds": NaN, "models": {}}'):
            with self.subTest(text=text):
                run_json.write_text(text, encoding="utf-8")
                code, out, _ = self.bench("score", cases_file, folders[0])
                self.assertEqual(code, 0)
                arm = json.loads(out)["arms"][0]
                self.assertEqual((arm["no_run"], arm["tokens"], arm["seconds"], arm["runs"]),
                                 (["1019-add-z"], 250000, 300, 1))

    def test_review_folder_is_skipped(self):
        cases_file, folders = write_example(self.root)
        (Path(folders[0]) / "1018-add-y" / "review-extra.md").mkdir()
        code, out, err = self.bench("score", cases_file, *folders)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), EXAMPLE_OUT)


class BenchErrorTest(BenchCase):
    def assertFails(self, args, code, message):
        got = self.bench(*args)
        self.assertEqual(got, (code, "", f"factory.py: {message}\n"))

    def cases_error(self, cases):
        path = self.root / "cases.json"
        path.write_text(cases if isinstance(cases, str) else json.dumps(cases), encoding="utf-8")
        arm = self.root / "arm"
        arm.mkdir(exist_ok=True)
        return self.bench("score", str(path), str(arm)), str(path)

    def test_wrong_args(self):
        for args in ([], ["score"], ["score", "cases.json"], ["rank", "cases.json", "arm"]):
            with self.subTest(args=args):
                self.assertEqual(self.bench(*args), (2, "", USAGE))

    def test_no_cases_file(self):
        path = str(self.root / "absent.json")
        self.assertFails(["score", path, str(self.root)], 4, f"bench: no cases file at {path}")

    def test_not_a_cases_file(self):
        for text in ("not json", "[]", '{"cases": []}', '{"cases": {}}', '{"other": [1]}'):
            with self.subTest(text=text):
                got, path = self.cases_error(text)
                self.assertEqual(got, (2, "", f'factory.py: bench: {path} is not a cases file: needs {{"cases": [...]}}\n'))

    def test_case_reasons(self):
        good = case(1018, "add-y", "reworked", [{"path": "worker/export.js", "lines": [40, 52], "severity": "high"}])

        def with_defect(**fields):
            return {**good, "defects": [good["defects"][0], {**good["defects"][0], **fields}]}

        rows = [
            ("not a case", 1, "id must look like 1018-add-y"),
            ({**good, "id": "-bad"}, 1, "id must look like 1018-add-y"),
            ({**good, "id": 1018}, 1, "id must look like 1018-add-y"),
            (good, 2, "duplicate id 1018-add-y"),
            ({**good, "pr": "https://example.test/pull/1"}, 1, "pr must be a GitHub pull request URL"),
            ({**good, "head": "3c4d5e6"}, 1, "head must be a 40-character commit"),
            ({**good, "source": "held"}, 1, "source must be reverted or reworked"),
            ({**good, "defects": []}, 1, "defects must be a non-empty list"),
            ({**good, "defects": "worker/export.js"}, 1, "defects must be a non-empty list"),
            (with_defect(path="/etc/export.js"), 1, "defect 2: path must be a relative repository path"),
            (with_defect(path="worker\\export.js"), 1, "defect 2: path must be a relative repository path"),
            (with_defect(path="worker/../export.js"), 1, "defect 2: path must be a relative repository path"),
            (with_defect(path=""), 1, "defect 2: path must be a relative repository path"),
            (with_defect(lines=[0, 4]), 1, "defect 2: lines must be [start, end] with 1 <= start <= end"),
            (with_defect(lines=[9, 4]), 1, "defect 2: lines must be [start, end] with 1 <= start <= end"),
            (with_defect(lines=[4]), 1, "defect 2: lines must be [start, end] with 1 <= start <= end"),
            (with_defect(lines=[True, 4]), 1, "defect 2: lines must be [start, end] with 1 <= start <= end"),
            (with_defect(severity="critical"), 1, "defect 2: severity must be high, medium or low"),
            (with_defect(severity=["high"]), 1, "defect 2: severity must be high, medium or low"),
        ]
        for bad, n, reason in rows:
            with self.subTest(reason=reason, bad=bad):
                got, path = self.cases_error({"cases": [bad] if n == 1 else [good, bad]})
                self.assertEqual(got, (2, "", f"factory.py: bench: {path}: case {n}: {reason}\n"))

    def test_no_arm_folder(self):
        cases_file, folders = write_example(self.root)
        absent = str(self.root / "absent")
        self.assertFails(["score", cases_file, folders[0], absent], 4, f"bench: no arm folder at {absent}")

    def test_bad_arm_names(self):
        cases_file, folders = write_example(self.root)
        odd = self.root / "odd name"
        odd.mkdir()
        other = self.root / "other"
        other.mkdir()
        (other / "opus-medium").mkdir()
        for folder, name in ((str(odd), "odd name"), (str(other / "opus-medium"), "opus-medium")):
            with self.subTest(name=name):
                self.assertFails(["score", cases_file, folders[0], folder], 2,
                                 f"bench: arm name {name} must be unique letters, digits, ., _ or -")


FAKE_CLAUDE = """import json, os, sys
from pathlib import Path
prompt = sys.stdin.read()
log = Path(os.environ["FAKE_LOG"])
calls = json.loads(log.read_text()) if log.exists() else []
calls.append({"argv": sys.argv[1:], "cwd": os.getcwd(), "stdin": prompt,
              "ceiling": os.environ.get("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS")})
log.write_text(json.dumps(calls))
your = next(line.split(": ", 1)[1] for line in prompt.splitlines() if line.startswith("Your file: "))
if not os.environ.get("FAKE_NO_REVIEW"):
    Path(your).write_text("VERDICT: CHANGES\\n\\n- `worker/export.js:44` drops the header row.\\n")
session = sys.argv[sys.argv.index("--session-id") + 1]
transcript = Path(os.environ["CLAUDE_CONFIG_DIR"], "projects", "fake", session + ".jsonl")
transcript.parent.mkdir(parents=True, exist_ok=True)
transcript.write_text(json.dumps({"type": "assistant", "message": {"id": "m1", "model": "claude-opus-5-5", "usage": {
    "input_tokens": 200000, "output_tokens": 20000, "cache_creation_input_tokens": 30000,
    "cache_read_input_tokens": 999}}}) + "\\n")
print("fake review done")
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
"""
REVIEW = "VERDICT: CHANGES\n\n- `worker/export.js:44` drops the header row.\n"
MODELS = {"claude-opus-5-5": {"input": 200000, "output": 20000, "cache_write": 30000, "cache_read": 999}}
SUMMARY = "replayed {}, skipped {}, failed {} of 1 cases into opus-medium\n"
RUNNING = "1018-add-y: running review-code ...\n"


class BenchReplayCase(BenchCase):
    def setUp(self):
        super().setUp()
        self.root = self.root.resolve()
        self.repo = self.root / "alpha"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("symbolic-ref", "HEAD", "refs/heads/main")
        (self.repo / "worker").mkdir()
        (self.repo / "worker/export.js").write_text("export const rows = [];\n", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "start")
        self.base = self.git("rev-parse", "HEAD")
        self.git("checkout", "-qb", "factory/1018-add-y")
        (self.repo / "worker/export.js").write_text("export const rows = [1];\n", encoding="utf-8")
        self.git("commit", "-qam", "add y")
        self.head = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", "-m", "Merge pull request #18", "factory/1018-add-y")
        self.merge = self.git("rev-parse", "HEAD")

        self.home = self.root / "home"
        (self.home / "projects").mkdir(parents=True)
        self.project = self.home / "projects/alpha.md"
        self.project.write_text("---\nrepo: example/alpha\n---\n", encoding="utf-8")
        (self.home / "config.md").write_text("---\ndevice: host-a\n---\n", encoding="utf-8")
        (self.home / "paths.md").write_text(f"---\nalpha: {self.repo}\n---\n", encoding="utf-8")
        self.job = self.home / "jobs/alpha/1018-add-y"
        self.job.mkdir(parents=True)
        (self.job / "spec.md").write_text("# Add y\n", encoding="utf-8")
        (self.job / "build.md").write_text("# Build\n", encoding="utf-8")

        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        claude = bin_dir / "claude"
        claude.write_text(f"#!{sys.executable}\n" + FAKE_CLAUDE, encoding="utf-8")
        claude.chmod(claude.stat().st_mode | stat.S_IXUSR)
        self.log = self.root / "claude-calls.json"
        os.environ.update(FACTORY_HOME=str(self.home), CLAUDE_CONFIG_DIR=str(self.root / "claude"),
                          FAKE_LOG=str(self.log), PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
        self.cases = self.root / "reviewer-cases.json"
        self.cases.write_text(json.dumps({"cases": [{
            "id": "1018-add-y", "pr": "https://github.com/example/alpha/pull/18", "head": self.head,
            "source": "reworked", "defects": [{"path": "worker/export.js", "lines": [40, 52], "severity": "high"}]}]}),
            encoding="utf-8")
        self.arm = self.root / "opus-medium"
        self.merges = {18: self.merge}
        self.gh_calls = []

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], check=True, capture_output=True,
                              text=True).stdout.strip()

    def merge_commits(self, repo, numbers, device):
        self.gh_calls.append((repo, numbers, device))
        return (None, self.merges) if isinstance(self.merges, str) else (self.merges, None)

    def replay(self, *flags, arm=None):
        clock = Mock(wraps=time)
        clock.monotonic.side_effect = [100.0, 400.0]
        with patch.object(self.module, "pr_merge_commits", self.merge_commits), \
                patch.object(self.module, "time", clock):
            return self.bench("replay", str(self.cases), str(arm or self.arm), *flags)

    def calls(self):
        return json.loads(self.log.read_text()) if self.log.exists() else []


class BenchReplayTest(BenchReplayCase):
    def test_running_example(self):
        code, out, err = self.replay("--role", "review-code", "--model", "claude-opus-5-5")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, RUNNING + "1018-add-y: wrote review-code.md (VERDICT: CHANGES), 300 s, 250000 tokens\n"
                         + SUMMARY.format(1, 0, 0))
        case_dir = self.arm / "1018-add-y"
        self.assertEqual((case_dir / "review-code.md").read_text(encoding="utf-8"), REVIEW)
        self.assertEqual(json.loads((case_dir / "run.json").read_text(encoding="utf-8")), {
            "role": "review-code", "model": "claude-opus-5-5", "head": self.head, "base": self.base, "exit": 0,
            "seconds": 300, "models": MODELS})
        self.assertEqual((case_dir / "replay.log").read_text(encoding="utf-8"), "fake review done\n")
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE((case_dir / "replay.log").stat().st_mode), 0o600)
        self.assertEqual(self.gh_calls, [("example/alpha", [18], "host-a")])

    def test_claude_call(self):
        (self.home / "projects/alpha.documents.md").write_text("notes\n", encoding="utf-8")
        code, _, _ = self.replay("--role", "review-code", "--model", "claude-opus-5-5")
        self.assertEqual(code, 0)
        [call] = self.calls()
        tmp = Path(call["cwd"]).parent
        self.assertTrue(tmp.name.startswith("factory-bench-1018-add-y-"))
        self.assertEqual(Path(call["cwd"]).name, "wt")
        session = call["argv"][call["argv"].index("--session-id") + 1]
        self.assertEqual(call["argv"], [
            "-p", "--agent", "factory-review-code", "--session-id", session, "--permission-mode", "dontAsk",
            "--allowedTools", "Read", "Grep", "Glob", "Write", "Bash(git:*)", "--add-dir", str(tmp / "job"),
            "--model", "claude-opus-5-5"])
        self.assertEqual(call["ceiling"], "0")
        self.assertEqual(call["stdin"], "\n".join([
            "Job id: 1018-add-y",
            f"Job folder: {tmp / 'job'}",
            f"Worktree: {tmp / 'wt'} (detached at {self.head}, base commit {self.base})",
            f"Project file: {self.project}",
            f"Project documents: {self.home / 'projects/alpha.documents.md'}",
            f"Round folder: {tmp / 'job/round-1'}",
            f"Your file: {tmp / 'job/round-1/review-code.md'}",
            "",
            "This is a reviewer benchmark replay. The pull request already merged; review it as if it were open. "
            f"There is no branch factory/1018-add-y: run git diff {self.base}...{self.head} wherever your "
            "instructions name the branch diff. There is no pre-review.md, and Bash runs only git. "
            "Write only Your file.\n"]))
        self.assertFalse(tmp.exists())
        self.assertEqual(len(self.git("worktree", "list").splitlines()), 1)

    def test_alias_without_model(self):
        code, _, err = self.replay("--role", "factory-review-code")
        self.assertEqual((code, err), (0, ""))
        [call] = self.calls()
        self.assertNotIn("--model", call["argv"])
        self.assertNotIn("Project documents", call["stdin"])
        run_json = json.loads((self.arm / "1018-add-y/run.json").read_text(encoding="utf-8"))
        self.assertIsNone(run_json["model"])

    def test_score_reads_replay(self):
        self.assertEqual(self.replay("--role", "review-code")[0], 0)
        code, out, err = self.bench("score", str(self.cases), str(self.arm))
        self.assertEqual((code, err), (0, ""))
        arm = json.loads(out)["arms"][0]
        self.assertEqual((arm["caught"], arm["weighted_recall"], arm["missed"]), (1, 1.0, []))
        self.assertEqual((arm["tokens"], arm["seconds"], arm["runs"]), (250000, 300, 1))

    def test_rerun_skips(self):
        self.assertEqual(self.replay("--role", "review-code")[0], 0)
        code, out, err = self.replay("--role", "review-code")
        self.assertEqual((code, out, err), (0, "1018-add-y: skipped, review-code.md already exists\n"
                                            + SUMMARY.format(0, 1, 0), ""))
        self.assertEqual(len(self.calls()), 1)

    def test_claude_failure_keeps_run_json(self):
        os.environ["FAKE_EXIT"] = "3"
        code, out, err = self.replay("--role", "review-code")
        self.assertEqual((code, out, err), (1, RUNNING + "1018-add-y: failed: claude exited 3, see replay.log\n"
                                            + SUMMARY.format(0, 0, 1), ""))
        case_dir = self.arm / "1018-add-y"
        self.assertFalse((case_dir / "review-code.md").exists())
        run_json = json.loads((case_dir / "run.json").read_text(encoding="utf-8"))
        self.assertEqual((run_json["exit"], run_json["seconds"], run_json["models"]), (3, 300, MODELS))
        self.assertEqual(len(self.git("worktree", "list").splitlines()), 1)

    def test_no_review_written(self):
        os.environ["FAKE_NO_REVIEW"] = "1"
        code, out, err = self.replay("--role", "review-code")
        self.assertEqual((code, out, err), (1, RUNNING + "1018-add-y: failed: no review-code.md with a VERDICT line, "
                                            "see replay.log\n" + SUMMARY.format(0, 0, 1), ""))
        case_dir = self.arm / "1018-add-y"
        self.assertFalse((case_dir / "review-code.md").exists())
        self.assertEqual(json.loads((case_dir / "run.json").read_text(encoding="utf-8"))["exit"], 0)

    def test_commit_missing_after_fetch(self):
        absent = "0" * 40
        cases = json.loads(self.cases.read_text(encoding="utf-8"))
        cases["cases"][0]["head"] = absent
        self.cases.write_text(json.dumps(cases), encoding="utf-8")
        code, out, err = self.replay("--role", "review-code")
        self.assertEqual((code, out, err), (1, RUNNING + f"1018-add-y: failed: commit {absent} is not in "
                                            f"{self.repo} after fetch\n" + SUMMARY.format(0, 0, 1), ""))
        self.assertEqual(self.calls(), [])

    def test_no_merge_base(self):
        tree = self.git("rev-parse", "HEAD^{tree}")
        first = self.git("commit-tree", tree, "-m", "unrelated")
        self.merges = {18: self.git("commit-tree", tree, "-p", first, "-m", "unrelated merge")}
        code, out, err = self.replay("--role", "review-code")
        self.assertEqual((code, out, err), (1, RUNNING + "1018-add-y: failed: no merge base of "
                                            f"{self.merges[18]}^1 and {self.head} in {self.repo}\n" + SUMMARY.format(0, 0, 1), ""))
        self.assertEqual(self.calls(), [])
        self.assertFalse((self.arm / "1018-add-y").exists())


class BenchReplayErrorTest(BenchReplayCase):
    def assertFails(self, flags, code, message, arm=None):
        self.assertEqual(self.replay(*flags, arm=arm), (code, "", f"factory.py: {message}\n"))
        self.assertEqual(self.calls(), [])

    def test_bad_args(self):
        rows = [[], ["--role"], ["--model", "claude-opus-5-5"], ["--role", "review-code", "--role", "review-code"],
                ["--role", "review-code", "--effort", "high"], ["--role", "review-code", "extra"]]
        for flags in rows:
            with self.subTest(flags=flags):
                self.assertFails(flags, 2, USAGE[len("factory.py: "):-1])
        self.assertEqual(self.bench("replay", str(self.cases)), (2, "", USAGE))

    def test_role_not_a_reviewer(self):
        self.assertFails(["--role", "build"], 2,
                         "bench: --role must be review-security, review-ux, review-ui or review-code")

    def test_bad_model(self):
        for model in ("claude opus", "", "x" * 201):
            with self.subTest(model=model):
                self.assertFails(["--role", "review-code", "--model", model], 2,
                                 "bench: --model must be 1 to 200 letters, digits or _ . / : @ [ ] -")

    def test_arm_name(self):
        self.assertFails(["--role", "review-code"], 2,
                         "bench: arm name odd name must be unique letters, digits, ., _ or -", self.root / "odd name")

    def test_arm_path_is_a_file(self):
        self.arm.write_text("not a folder\n", encoding="utf-8")
        self.assertFails(["--role", "review-code"], 2, f"bench: {self.arm} is not a folder")

    def test_no_claude(self):
        empty = self.root / "empty"
        empty.mkdir()
        os.environ["PATH"] = str(empty)
        self.assertFails(["--role", "review-code"], 4, "bench: replay needs claude on PATH")

    def test_no_job_folder(self):
        (self.job / "spec.md").unlink()
        self.assertFails(["--role", "review-code"], 4,
                         f"bench: 1018-add-y: no job folder with spec.md under {self.home / 'jobs'}; "
                         "pull it with factory.py pull <project>/1018-add-y")

    def test_two_job_folders(self):
        other = self.home / "jobs/beta/1018-add-y"
        other.mkdir(parents=True)
        (other / "spec.md").write_text("# Add y\n", encoding="utf-8")
        self.assertFails(["--role", "review-code"], 2, "bench: 1018-add-y: job folders in more than one project")

    def test_repo_mismatch(self):
        self.project.write_text("---\nrepo: example/other\n---\n", encoding="utf-8")
        self.assertFails(["--role", "review-code"], 2,
                         "bench: 1018-add-y: pr is not in example/other, the repo of project alpha")

    def test_project_without_repo(self):
        for text in ("---\nbase: main\n---\n", None):
            with self.subTest(text=text):
                if text:
                    self.project.write_text(text, encoding="utf-8")
                else:
                    self.project.unlink()
                self.assertFails(["--role", "review-code"], 4,
                                 "bench: 1018-add-y: project alpha has no repo; pull it with factory.py pull alpha")

    def test_no_checkout(self):
        (self.home / "paths.md").write_text("---\nbeta: /tmp/beta\n---\n", encoding="utf-8")
        self.assertFails(["--role", "review-code"], 4, "bench: 1018-add-y: no checkout of alpha in paths.md on host-a")

    def test_gh_failed(self):
        self.merges = "gh is not installed on host-a"
        self.assertFails(["--role", "review-code"], 5, "bench: gh is not installed on host-a")

    def test_no_merge_commit(self):
        self.merges = {18: None}
        self.assertFails(["--role", "review-code"], 4,
                         "bench: 1018-add-y: GitHub reports no merge commit for https://github.com/example/alpha/pull/18")


HEAD_Z ="9f8e7d6c5b4a39281706f5e4d3c2b1a098765432"


def outcome_job(job_id, result, pr):
    return {"id": job_id, "project": "alpha", "stage": "merged", "updated": "2026-01-05T10:00:00Z",
            "pr": f"https://github.com/{pr}", "outcome": {"result": result, "at": "2026-01-09T12:00:00Z",
                                                          "evidence": []}}


DRAFT_BOARD = {"updated": "2026-01-09T12:00:00Z", "jobs": [
    outcome_job("1020-add-w", "reworked", "example/beta/pull/20"),
    outcome_job("1019-add-z", "reverted", "example/alpha/pull/19"),
    outcome_job("1018-add-y", "reworked", "example/alpha/pull/18"),
    outcome_job("1017-add-x", "held", "example/alpha/pull/17")]}
DRAFT_OUT = {"cases": [
    {"id": "1018-add-y", "pr": "https://github.com/example/alpha/pull/18", "head": HEAD, "source": "reworked",
     "defects": []},
    {"id": "1019-add-z", "pr": "https://github.com/example/alpha/pull/19", "head": HEAD_Z, "source": "reverted",
     "defects": []}]}
SKIP_W = "factory.py: bench cases skips 1020-add-w: PR is not in example/alpha\n"


class BenchDraftTest(BenchCase):
    def setUp(self):
        super().setUp()
        self.home = self.root / "home"
        (self.home / "projects").mkdir(parents=True)
        (self.home / "projects/alpha.md").write_text("---\nrepo: example/alpha\n---\n", encoding="utf-8")
        os.environ["FACTORY_HOME"] = str(self.home)

    def draft(self, *args, board=DRAFT_BOARD, states=({18: ("MERGED", HEAD), 19: ("MERGED", HEAD_Z)}, None)):
        calls = []

        def pr_states(repo, numbers, device):
            calls.append((repo, numbers, device))
            return states

        with http_fixture({"GET /api/board": (200, board), "PUT /api/board": (200, b"ok")}) as server, \
                patch.object(self.module, "pr_states", pr_states):
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\ndevice: host-a\n---\n",
                                                 encoding="utf-8")
            got = self.bench(*args)
        self.puts = [r for r in server.requests if r[0] == "PUT"]
        self.calls = calls
        return got

    def test_running_example(self):
        code, out, err = self.draft("cases", "alpha")
        self.assertEqual((code, err), (0, SKIP_W))
        self.assertEqual(out, json.dumps(DRAFT_OUT, indent=2) + "\n")
        self.assertEqual(self.puts, [])
        self.assertEqual(self.calls, [("example/alpha", [18, 19], "host-a")])

    def test_no_head(self):
        skip = "factory.py: bench cases skips 1019-add-z: no head commit from GitHub\n"
        for name, states in (("omitted", {18: ("MERGED", HEAD)}),
                             ("bad head", {18: ("MERGED", HEAD), 19: ("MERGED", "9f8e7d6")}),
                             ("no head", {18: ("MERGED", HEAD), 19: ("MERGED", None)})):
            with self.subTest(name):
                code, out, err = self.draft("cases", "alpha", states=(states, None))
                self.assertEqual((code, err), (0, SKIP_W + skip))
                self.assertEqual(json.loads(out), {"cases": DRAFT_OUT["cases"][:1]})

    def test_errors(self):
        usage = "factory.py: usage: factory.py bench cases <project-slug>\n"
        nothing = "factory.py: bench: no reverted or reworked jobs to draft in alpha\n"
        held = {"updated": "2026-01-09T12:00:00Z", "jobs": [DRAFT_BOARD["jobs"][0], DRAFT_BOARD["jobs"][3]]}
        rows = [("no slug", ["cases"], {}, (2, "", usage)),
                ("two slugs", ["cases", "alpha", "beta"], {}, (2, "", usage)),
                ("bad slug", ["cases", "al pha"], {}, (2, "", usage)),
                ("no project file", ["cases", "beta"], {}, (4, "", "factory.py: bench: no project file for beta\n")),
                ("gh fails", ["cases", "alpha"], {"states": (None, "gh is not installed on host-a")},
                 (5, "", SKIP_W + "factory.py: bench: gh is not installed on host-a\n")),
                ("nothing drafted", ["cases", "alpha"], {"board": held}, (3, "", SKIP_W + nothing))]
        for name, args, kwargs, want in rows:
            with self.subTest(name):
                self.assertEqual(self.draft(*args, **kwargs), want)

    def test_draft_needs_labels(self):
        code, out, _ = self.draft("cases", "alpha")
        self.assertEqual(code, 0)
        path = self.root / "reviewer-cases.json"
        path.write_text(out, encoding="utf-8")
        arm = self.root / "arm"
        arm.mkdir()
        self.assertEqual(self.bench("score", str(path), str(arm)),
                         (2, "", f"factory.py: bench: {path}: case 1: defects must be a non-empty list\n"))
