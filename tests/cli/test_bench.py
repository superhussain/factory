import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

USAGE = "factory.py: usage: factory.py bench score <cases-file> <arm-folder>...\n"
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
