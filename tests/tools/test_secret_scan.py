"""Tests for tools/secret-scan.py against a temp git repo with a runtime-generated token."""
import os
import secrets
import shutil
import string
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
TOOL = ROOT / "tools" / "secret-scan.py"


class SecretScanTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.repo = Path(tmp.name) / "repo"
        self.repo.mkdir()
        shutil.copy(ROOT / ".gitleaks.toml", self.repo / ".gitleaks.toml")
        (self.repo / "clean.txt").write_text("nothing here\n")
        self.patterns = Path(tmp.name) / "patterns.txt"
        self.patterns.write_text("synthetic-owner\n(?i)acme-internal\n")
        subprocess.run(["git", "init", "-q"], cwd=self.repo, check=True)

    def commit(self):
        subprocess.run(["git", "add", "-A"], cwd=self.repo, check=True)
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.test", "commit", "-qm", "x", "--no-verify"], cwd=self.repo, check=True)

    def scan(self, *argv):
        return subprocess.run([sys.executable, str(TOOL), *argv], cwd=self.repo, capture_output=True, text=True, env={**os.environ, "NO_COLOR": "1"})

    def test_private_patterns_report_location_not_match(self):
        (self.repo / "notes.md").write_text("line one\nowned by synthetic-owner\nsee ACME-Internal docs\n")
        (self.repo / "untracked.md").write_text("synthetic-owner\n")
        self.commit()
        (self.repo / "untracked.md").unlink()
        (self.repo / "later.md").write_text("synthetic-owner\n")
        result = self.scan("--private-patterns", str(self.patterns))
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "notes.md:2: private pattern 1\nnotes.md:3: private pattern 2\n")
        self.assertNotIn("synthetic-owner", result.stdout + result.stderr)
        self.assertNotIn("ACME", result.stdout + result.stderr)

    def test_private_patterns_clean(self):
        self.commit()
        self.assertEqual(self.scan("--private-patterns", str(self.patterns)).returncode, 0)

    @unittest.skipIf(shutil.which("gitleaks") is None, "gitleaks not installed")
    def test_gitleaks_finds_planted_token_and_redacts(self):
        token = "ghp_" + "".join(secrets.choice(string.ascii_letters + string.digits) for _ in range(36))
        (self.repo / "config.txt").write_text(f"token = {token}\n")
        self.commit()
        result = self.scan()
        self.assertEqual(result.returncode, 1)
        self.assertNotIn(token, result.stdout + result.stderr)
        history = Path(self.repo.parent) / "history.json"
        result = self.scan("--history", str(history))
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "history: 1 findings\n")
        self.assertNotIn(token, history.read_text())

    @unittest.skipIf(shutil.which("gitleaks") is None, "gitleaks not installed")
    def test_gitleaks_clean_tree(self):
        self.commit()
        self.assertEqual(self.scan().returncode, 0)

    def test_gitleaks_missing(self):
        self.commit()
        bin_dir = self.repo.parent / "bin"
        bin_dir.mkdir()
        (bin_dir / "git").symlink_to(shutil.which("git"))
        result = subprocess.run([sys.executable, str(TOOL)], cwd=self.repo, capture_output=True, text=True, env={**os.environ, "PATH": str(bin_dir)})
        self.assertEqual(result.returncode, 2)
        self.assertIn("gitleaks not found", result.stderr)


if __name__ == "__main__":
    unittest.main()
