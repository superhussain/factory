import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
USAGE = "factory.py with-cloudflare -- <command> [<arg>...]"
DUMP_ENV = "import json, os, sys; json.dump(dict(os.environ), open(sys.argv[1], 'w'))"


class WithCloudflareTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve() / "home"
        self.home.mkdir()
        self.env_file = self.home / ".env"
        self.env_file.write_text("CLOUDFLARE_API_TOKEN=cf-test-token\nCLOUDFLARE_ACCOUNT_ID=acct-test\n"
                                 "FACTORY_ACCESS_CLIENT_SECRET=access-test\n", encoding="utf-8")
        self.out = Path(temp.name) / "env.json"
        os.environ["FACTORY_HOME"] = str(self.home)

    def run_child(self, *args):
        code, stdout, stderr = invoke(factory, ["with-cloudflare", "--", *args])
        self.assertNotIn("cf-test-token", stdout + stderr)
        return code, stdout, stderr

    def assert_missing_token(self):
        code, stdout, stderr = self.run_child(sys.executable, "-c", DUMP_ENV, str(self.out))
        self.assertEqual((code, stdout), (5, ""))
        self.assertEqual(stderr, f"factory.py: CLOUDFLARE_API_TOKEN missing from {self.env_file}. "
                                 "docs/cloudflare.md, Device token, shows how to store it.\n")
        self.assertFalse(self.out.exists())

    def test_loads_only_cloudflare_keys(self):
        self.assertEqual(self.run_child(sys.executable, "-c", DUMP_ENV, str(self.out)), (0, "", ""))
        env = json.loads(self.out.read_text(encoding="utf-8"))
        self.assertEqual(env["CLOUDFLARE_API_TOKEN"], "cf-test-token")
        self.assertEqual(env["CLOUDFLARE_ACCOUNT_ID"], "acct-test")
        self.assertNotIn("FACTORY_ACCESS_CLIENT_SECRET", env)

    def test_child_exit_code(self):
        self.assertEqual(self.run_child(sys.executable, "-c", "raise SystemExit(7)"), (7, "", ""))

    def test_missing_token(self):
        self.env_file.write_text("CLOUDFLARE_ACCOUNT_ID=acct-test\n", encoding="utf-8")
        self.assert_missing_token()
        self.env_file.unlink()
        self.assert_missing_token()

    @unittest.skipIf(os.name == "nt", "symlinks need privileges on Windows")
    def test_linked_env(self):
        target = self.home / "real.env"
        self.env_file.rename(target)
        self.env_file.symlink_to(target)
        self.assert_missing_token()

    def test_usage(self):
        for args in (["with-cloudflare"], ["with-cloudflare", "--"],
                     ["with-cloudflare", sys.executable, "-c", "pass"]):
            code, stdout, stderr = invoke(factory, args)
            self.assertEqual((code, stdout), (2, ""))
            self.assertIn(USAGE, stderr)

    def test_command_not_found(self):
        missing = str(self.home / "no-such-command")
        self.assertEqual(self.run_child(missing), (127, "", f"factory.py: cannot run {missing}\n"))

    def test_flags_after_separator(self):
        code, stdout, stderr = invoke(factory, ["--home", str(self.home), "with-cloudflare", "--",
                                                sys.executable, "-c", "import json, sys; json.dump(sys.argv[1:], open(sys.argv[1], 'w'))",
                                                str(self.out), "--home", "/h", "--session", "x"])
        self.assertEqual((code, stdout, stderr), (0, "", ""))
        self.assertEqual(json.loads(self.out.read_text(encoding="utf-8")), [str(self.out), "--home", "/h", "--session", "x"])

    def test_documented_command_not_flagged(self):
        command = ("python3 ~/.claude/skills/factory/runtime/factory.py with-cloudflare -- sh -c "
                   "'curl -H \"Authorization: Bearer $CLOUDFLARE_API_TOKEN\" "
                   "https://api.cloudflare.com/client/v4/user/tokens/verify'")
        inp = {"command": command}
        self.assertEqual(factory.audit_flags("Bash", inp, factory.audit_target_raw("Bash", inp), False), [])


if __name__ == "__main__":
    unittest.main()
