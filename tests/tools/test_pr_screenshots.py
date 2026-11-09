"""Tests for skills/factory/runtime/pr_screenshots.py with a patched urlopen and synthetic credentials."""
import importlib.util
import io
import re
import tempfile
import unittest
import urllib.error
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent.parent

spec = importlib.util.spec_from_file_location("pr_screenshots", ROOT / "skills" / "factory" / "runtime" / "pr_screenshots.py")
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)

LABEL = "factory-236-make-pr-screenshot"
ENV = "CLOUDFLARE_API_TOKEN=synthetic-token\nCLOUDFLARE_ACCOUNT_ID=synthetic-account\nPR_SCREENSHOTS_BUCKET=synthetic-bucket\nPR_SCREENSHOTS_ORIGIN=https://images.example.test/\n"


class UploaderTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.home = Path(tmp.name)
        (self.home / ".env").write_text(ENV)
        self.image = self.home / "after-board.png"
        self.image.write_bytes(b"png")

    def run_main(self, *argv, home=None):
        with mock.patch("urllib.request.urlopen") as urlopen, redirect_stdout(io.StringIO()) as out, redirect_stderr(io.StringIO()) as err:
            try:
                code = tool.main(["--home", str(home or self.home), *argv])
            except SystemExit as e:
                code = e.code
        return code, out.getvalue(), err.getvalue(), urlopen

    def test_key_with_project(self):
        code, out, _, urlopen = self.run_main("--project", "factory", LABEL, str(self.image))
        self.assertEqual(code, 0)
        match = re.fullmatch(rf"{re.escape(str(self.image))}\thttps://images\.example\.test/(factory/{LABEL}/[0-9a-f]{{12}}-after-board\.png)\n", out)
        self.assertIsNotNone(match, out)
        request = urlopen.call_args.args[0]
        self.assertEqual(request.get_method(), "PUT")
        self.assertTrue(request.full_url.endswith(f"/accounts/synthetic-account/r2/buckets/synthetic-bucket/objects/{match.group(1)}"))
        self.assertEqual(request.get_header("Authorization"), "Bearer synthetic-token")
        self.assertEqual(request.get_header("Content-type"), "image/png")

    def test_key_without_project(self):
        code, out, _, _ = self.run_main(LABEL, str(self.image))
        self.assertEqual(code, 0)
        self.assertRegex(out, rf"https://images\.example\.test/other/{LABEL}/[0-9a-f]{{12}}-after-board\.png")

    def test_missing_credentials(self):
        (self.home / ".env").write_text("CLOUDFLARE_API_TOKEN=\nCLOUDFLARE_ACCOUNT_ID=synthetic-account\n")
        code, out, err, urlopen = self.run_main(LABEL, str(self.image))
        self.assertEqual(code, 1)
        self.assertEqual(err, f"CLOUDFLARE_API_TOKEN / PR_SCREENSHOTS_BUCKET / PR_SCREENSHOTS_ORIGIN missing from {self.home / '.env'}\n")
        urlopen.assert_not_called()
        self.assertNotIn("synthetic-account", out + err)

    def test_bad_publisher_config(self):
        cases = (
            ("PR_SCREENSHOTS_ORIGIN=http://images.example.test", "PR_SCREENSHOTS_ORIGIN must start with https://"),
            ("PR_SCREENSHOTS_BUCKET=Synthetic_Bucket", "PR_SCREENSHOTS_BUCKET is not a valid R2 bucket name"),
        )
        for line, message in cases:
            (self.home / ".env").write_text(ENV + line + "\n")
            code, _, err, urlopen = self.run_main(LABEL, str(self.image))
            self.assertEqual(code, 1)
            self.assertEqual(err, message + "\n")
            urlopen.assert_not_called()

    def test_validates_all_before_upload(self):
        missing = self.home / "gone.png"
        code, _, err, urlopen = self.run_main(LABEL, str(self.image), str(missing))
        self.assertEqual(code, 1)
        self.assertIn(f"no such file: {missing}", err)
        urlopen.assert_not_called()
        text = self.home / "notes.txt"
        text.write_text("x")
        code, _, err, urlopen = self.run_main(LABEL, str(self.image), str(text))
        self.assertEqual(code, 1)
        self.assertIn(f"not an image: {text}", err)
        urlopen.assert_not_called()

    def test_http_error_hides_token(self):
        error = urllib.error.HTTPError("https://api.cloudflare.com", 403, "Forbidden", {}, None)
        with mock.patch("urllib.request.urlopen", side_effect=error), redirect_stdout(io.StringIO()) as out, redirect_stderr(io.StringIO()) as err:
            code = tool.main(["--home", str(self.home), LABEL, str(self.image)])
        self.assertEqual(code, 1)
        self.assertIn(f"upload failed: {self.image}: HTTP 403", err.getvalue())
        self.assertNotIn("synthetic-token", out.getvalue() + err.getvalue())

    def test_network_error_hides_token(self):
        for error, reason in ((urllib.error.URLError("offline"), "offline"), (TimeoutError("timed out"), "timed out")):
            with mock.patch("urllib.request.urlopen", side_effect=error), redirect_stdout(io.StringIO()) as out, redirect_stderr(io.StringIO()) as err:
                code = tool.main(["--home", str(self.home), LABEL, str(self.image)])
            self.assertEqual(code, 1)
            self.assertIn(f"upload failed: {self.image}: {reason}", err.getvalue())
            self.assertNotIn("synthetic-token", out.getvalue() + err.getvalue())

    def test_usage_branch(self):
        for argv, home in ((("Factory-236", str(self.image)), None), (("x\n", str(self.image)), None), ((LABEL, str(self.image)), "relative")):
            code, _, err, urlopen = self.run_main(*argv, home=home)
            self.assertEqual(code, 1)
            self.assertEqual(err, tool.USAGE + "\n")
            urlopen.assert_not_called()

    def test_env_parsing(self):
        path = self.home / "parse.env"
        path.write_bytes(b"# comment\r\n\r\nA=\"quoted value\"\r\nB='single'\r\n  C = plain \r\nnoequals\r\n")
        self.assertEqual(tool.read_env(path), {"A": "quoted value", "B": "single", "C": "plain"})


if __name__ == "__main__":
    unittest.main()
