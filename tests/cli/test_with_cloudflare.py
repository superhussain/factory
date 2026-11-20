import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

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

    def assert_exit_5(self, message):
        code, stdout, stderr = self.run_child(sys.executable, "-c", DUMP_ENV, str(self.out))
        self.assertEqual((code, stdout), (5, ""))
        self.assertEqual(stderr, f"factory.py: {message}\n")
        self.assertFalse(self.out.exists())

    def assert_not_regular(self):
        self.assert_exit_5(f"{self.env_file} is a link or not a regular file, so it was not read. "
                           "Replace it with a regular file.")

    def test_loads_only_cloudflare_keys(self):
        self.assertEqual(self.run_child(sys.executable, "-c", DUMP_ENV, str(self.out)), (0, "", ""))
        env = json.loads(self.out.read_text(encoding="utf-8"))
        self.assertEqual(env["CLOUDFLARE_API_TOKEN"], "cf-test-token")
        self.assertEqual(env["CLOUDFLARE_ACCOUNT_ID"], "acct-test")
        self.assertNotIn("FACTORY_ACCESS_CLIENT_SECRET", env)

    def test_child_exit_code(self):
        self.assertEqual(self.run_child(sys.executable, "-c", "raise SystemExit(7)"), (7, "", ""))

    def test_no_token(self):
        self.env_file.write_text("CLOUDFLARE_ACCOUNT_ID=acct-test\n", encoding="utf-8")
        self.assert_exit_5(f"CLOUDFLARE_API_TOKEN missing from {self.env_file}. "
                           "docs/cloudflare.md, Device token, shows how to store it.")

    def test_no_env_file(self):
        self.env_file.unlink()
        self.assert_exit_5(f"no {self.env_file}. docs/cloudflare.md, Device token, "
                           "shows how to store CLOUDFLARE_API_TOKEN there.")

    @unittest.skipIf(os.name == "nt", "symlinks need privileges on Windows")
    def test_linked_env(self):
        target = self.home / "real.env"
        self.env_file.rename(target)
        self.env_file.symlink_to(target)
        self.assert_not_regular()

    def test_env_is_folder(self):
        self.env_file.unlink()
        self.env_file.mkdir()
        self.assert_not_regular()

    @unittest.skipIf(os.name == "nt" or os.geteuid() == 0, "chmod 0 does not block reads here")
    def test_unreadable_env(self):
        self.env_file.chmod(0)
        self.addCleanup(self.env_file.chmod, 0o600)
        self.assert_exit_5(f"cannot read {self.env_file}: check its permissions and that it is UTF-8 text.")

    def test_usage(self):
        for args in (["with-cloudflare"], ["with-cloudflare", "--"],
                     ["with-cloudflare", sys.executable, "-c", "pass"]):
            self.assertEqual(invoke(factory, args), (2, "", f"factory.py: usage: {USAGE}\n"))

    def test_interrupt(self):
        with patch.object(factory.subprocess, "run", side_effect=KeyboardInterrupt):
            self.assertEqual(self.run_child("true"), (130, "", ""))

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
                   "'printf \"Authorization: Bearer %s\\n\" \"$CLOUDFLARE_API_TOKEN\" | curl -H @- "
                   "https://api.cloudflare.com/client/v4/user/tokens/verify'")
        inp = {"command": command}
        self.assertEqual(factory.audit_flags("Bash", inp, factory.audit_target_raw("Bash", inp), False), [])


if __name__ == "__main__":
    unittest.main()
