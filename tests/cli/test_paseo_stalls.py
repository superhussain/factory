import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from support import SKILL

SCRIPT = SKILL / "runtime" / "paseo_stalls.py"
ID = "4cec0a47-1d21-4115-945b-9e7dfb668ef4"
SECOND = "aaaaaaaab-bbbbbbbbccccddddeeeeffff"
LIMIT = "You've hit your session limit · resets 7pm (UTC)"
WEEKLY = "You've hit your weekly limit · resets Oct 28, 9pm (UTC)"
INFO = {"Status": "idle", "Archived": False, "UpdatedAt": "2026-10-29T18:50:00.000Z",
        "Provider": "claude/model-a", "Model": "model-a"}


def zones_available():
    try:
        import zoneinfo
        zoneinfo.ZoneInfo("UTC")
    except (ImportError, KeyError, ValueError, OSError):
        return False
    return True


ZONES = zones_available()
NO_ZONES = ("zoneinfo cannot resolve UTC here (Python 3.8 or no zone data), "
            "so the runtime leaves stalls out")

FAKE_PASEO = """#!/usr/bin/env python3
import json, os, sys
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
with open(os.path.join(root, "paseo-fixture.json"), encoding="utf-8") as f:
    conf = json.load(f)
path = os.path.join(root, "paseo-calls.json")
calls = []
if os.path.exists(path):
    with open(path, encoding="utf-8") as f:
        calls = json.load(f)
calls.append(sys.argv[1:])
with open(path, "w", encoding="utf-8") as f:
    json.dump(calls, f)
args = sys.argv[1:]
if args[:1] == ["--home"]:
    args = args[2:]
if args and args[0] == "ls":
    print(conf["ls_raw"] if "ls_raw" in conf else json.dumps(conf["ls"]))
    sys.exit(conf.get("ls_code", 0))
if len(args) > 1 and args[0] == "logs":
    print(conf["logs"].get(args[1], ""))
    sys.exit(0)
if len(args) > 1 and args[0] == "inspect":
    print(json.dumps(conf["inspect"].get(args[1], {})))
    sys.exit(0)
if args and args[0] == "archive":
    sys.exit(conf.get("archive_code", 0))
sys.exit(0)
"""


@unittest.skipIf(os.name == "nt", "the fake paseo script needs a POSIX bin directory on PATH")
class PaseoStallsTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        fake = self.bin / "paseo"
        fake.write_text(FAKE_PASEO, encoding="utf-8")
        fake.chmod(0o755)
        self.fixture = self.root / "paseo-fixture.json"
        self.calls = self.root / "paseo-calls.json"
        self.paseo_home = self.root / "paseo-home"
        self.set_fixture()
        old_env = dict(os.environ)
        self.addCleanup(os.environ.update, old_env)
        self.addCleanup(os.environ.clear)
        os.environ["PATH"] = f"{self.bin}{os.pathsep}{old_env.get('PATH', '')}"
        os.environ["PASEO_HOME"] = str(self.paseo_home)

    def set_fixture(self, **conf):
        self.fixture.write_text(json.dumps(conf), encoding="utf-8")
        if self.calls.exists():
            self.calls.unlink()

    def run_script(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True,
                              text=True, timeout=60)

    def recorded(self):
        if not self.calls.exists():
            return []
        return json.loads(self.calls.read_text(encoding="utf-8"))

    @unittest.skipUnless(ZONES, NO_ZONES)
    def test_list_reports_only_idle_claude_limit_stalls(self):
        os.environ["PASEO_HOME"] = str(self.root / "paseo-home")
        self.set_fixture(
            ls=[{"id": ID, "status": "idle", "provider": "claude/model-a"},
                {"id": SECOND, "status": "idle", "provider": "claude/model-a"},
                {"id": "bbbb1111-2222-4333-8444-555566667777", "status": "running",
                 "provider": "claude/model-a"},
                {"id": "cccc2222-3333-8444-9555-666677778888", "status": "idle",
                 "provider": "openai/model-o"},
                {"id": "dddd3333-4444-8555-8666-777788889999", "status": "idle",
                 "provider": "claude/model-a"},
                {"id": "eeee4444-5555-8666-9777-888899990000", "status": "idle",
                 "provider": "claude/model-a"},
                {"id": "bad id", "status": "idle", "provider": "claude/model-a"},
                {"id": "ffff5555-6666-8777-8888-999900001111", "status": "idle",
                 "provider": "claude-pe3/model-a"},
                {"id": "abcd6666-7777-8888-9999-000011112222", "status": "idle",
                 "provider": "claudex/model-a"}],
            logs={ID: LIMIT, SECOND: WEEKLY,
                  "ffff5555-6666-8777-8888-999900001111": LIMIT,
                  "abcd6666-7777-8888-9999-000011112222": LIMIT,
                  "cccc2222-3333-8444-9555-666677778888": LIMIT,
                  "dddd3333-4444-8555-8666-777788889999": "Summary of the run.",
                  "eeee4444-5555-8666-9777-888899990000": LIMIT,
                  "bad id": LIMIT},
            inspect={ID: INFO, SECOND: dict(INFO, UpdatedAt="2026-10-28T20:30:00.000Z"),
                     "cccc2222-3333-8444-9555-666677778888": INFO,
                     "eeee4444-5555-8666-9777-888899990000": dict(INFO, Archived=True),
                     "bad id": INFO,
                     "ffff5555-6666-8777-8888-999900001111": INFO})
        result = self.run_script("list")
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads(result.stdout)
        self.assertEqual(doc, {"version": 1, "stalls": [
            {"session": ID, "reset": "2026-10-29T19:00:00Z", "provider": "claude/model-a",
             "model": "model-a"},
            {"session": SECOND, "reset": "2026-10-28T21:00:00Z", "provider": "claude/model-a",
             "model": "model-a"},
            {"session": "ffff5555-6666-8777-8888-999900001111", "reset": "2026-10-29T19:00:00Z",
             "provider": "claude/model-a", "model": "model-a"}]})
        self.assertEqual(self.recorded()[0], ["--home", str(self.paseo_home), "ls", "-g", "--json"])

    @unittest.skipUnless(ZONES, NO_ZONES)
    def test_dateless_reset_anchors_on_message_time(self):
        # UpdatedAt 18:50, resets 7pm (UTC): the reset is 19:00 that day, never the next
        # day, so pickup never takes a stall the limit-resume sweep owns.
        self.set_fixture(ls=[{"id": ID, "status": "idle", "provider": "claude/model-a"}],
                         logs={ID: LIMIT}, inspect={ID: INFO})
        result = self.run_script("list")
        self.assertEqual(result.returncode, 0, result.stderr)
        [stall] = json.loads(result.stdout)["stalls"]
        self.assertEqual(stall["reset"], "2026-10-29T19:00:00Z")

    def test_missing_zoneinfo_leaves_stalls_out(self):
        shim = self.root / "no-zoneinfo" / "zoneinfo"
        shim.mkdir(parents=True)
        (shim / "__init__.py").write_text('raise ImportError("no zoneinfo")\n', encoding="utf-8")
        os.environ["PYTHONPATH"] = str(self.root / "no-zoneinfo")
        self.set_fixture(ls=[{"id": ID, "status": "idle", "provider": "claude/model-a"}],
                         logs={ID: LIMIT}, inspect={ID: INFO})
        result = self.run_script("list")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"version": 1, "stalls": []})

    def test_archive_rechecks(self):
        self.set_fixture(ls=[{"id": ID, "status": "idle", "provider": "claude/model-a"}],
                         logs={ID: LIMIT}, inspect={ID: INFO})
        result = self.run_script("archive", ID)
        self.assertEqual((result.returncode, result.stderr), (0, ""))
        self.assertIn(["--home", str(self.paseo_home), "archive", ID], self.recorded())
        for call in self.recorded():
            self.assertNotIn("--force", call)
        # A session a human or the sweep has woken is kept: exit 3, no archive call.
        self.set_fixture(ls=[{"id": ID, "status": "idle", "provider": "claude/model-a"}],
                         logs={ID: LIMIT},
                         inspect={ID: dict(INFO, Status="running")})
        result = self.run_script("archive", ID)
        self.assertEqual(result.returncode, 3)
        self.assertNotIn(["--home", str(self.paseo_home), "archive", ID], self.recorded()[1:])
        result = self.run_script("archive", "../escape")
        self.assertEqual((result.returncode, result.stderr),
                         (2, "usage: paseo_stalls.py list | archive <id> [--force]\n"))

    def test_archive_force_skips_the_stall_check(self):
        self.set_fixture(inspect={ID: dict(INFO, Status="running")})
        result = self.run_script("archive", ID, "--force")
        self.assertEqual((result.returncode, result.stderr), (0, ""))
        self.assertIn(["--home", str(self.paseo_home), "archive", "--force", ID], self.recorded())
        for call in self.recorded():
            self.assertNotIn("logs", call)

    def test_archive_force_is_a_no_op_on_an_archived_session(self):
        self.set_fixture(inspect={ID: dict(INFO, Archived=True)})
        result = self.run_script("archive", ID, "--force")
        self.assertEqual((result.returncode, result.stderr), (0, ""))
        for call in self.recorded():
            self.assertNotIn("archive", call)

    def test_archive_force_failure_exits_1(self):
        self.set_fixture(inspect={ID: INFO}, archive_code=1)
        result = self.run_script("archive", ID, "--force")
        self.assertEqual((result.returncode, result.stderr), (1, "paseo_stalls.py: paseo archive exit 1\n"))

    def test_paseo_missing_or_failing(self):
        os.environ["PATH"] = "/usr/bin:/bin"
        result = self.run_script("list")
        self.assertEqual((result.returncode, result.stderr),
                         (1, "paseo_stalls.py: paseo is not on PATH\n"))
        os.environ["PATH"] = f"{self.bin}{os.pathsep}{os.environ['PATH']}"
        self.set_fixture(ls=[], ls_code=3)
        result = self.run_script("list")
        self.assertEqual((result.returncode, result.stderr),
                         (1, "paseo_stalls.py: paseo ls failed\n"))
        self.set_fixture(ls=[], ls_raw="{oops")
        result = self.run_script("list")
        self.assertEqual((result.returncode, result.stderr),
                         (1, "paseo_stalls.py: paseo ls failed\n"))

    def test_usage(self):
        for args in ((), ("list", "extra"), ("bogus",), ("archive",), ("archive", "x", "y"), ("archive", ID, "--bogus")):
            with self.subTest(args=args):
                result = self.run_script(*args)
                self.assertEqual((result.returncode, result.stdout), (2, ""))
                self.assertEqual(result.stderr, "usage: paseo_stalls.py list | archive <id> [--force]\n")


if __name__ == "__main__":
    unittest.main()
