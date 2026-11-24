"""Ported from tests/cli/test_portability.py: topics docs/device.md and docs/integrations.md must keep covering."""
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent


class DocumentedTopicsTest(unittest.TestCase):
    def test_documented_setup_recovery_and_policy_topics(self):
        device = (ROOT / "docs/device.md").read_text(encoding="utf-8")
        integrations = (ROOT / "docs/integrations.md").read_text(encoding="utf-8")
        for value in ("Python 3.8+", "source checkout is unavailable", "FACTORY_ACCESS_CLIENT_ID", "--connection",
                      "XDG_CONFIG_HOME", "adopt-home", "fsync", "hard-link", "doctor", "init <repo>",
                      "where sample", "claim <factory-home>", "sync sample", "handoff sample/", "ACL"):
            self.assertIn(value, device)
        for code in range(1, 7):
            self.assertIn("| " + str(code) + " |", device)
        for value in ("opencode api get /api/agent", "native agent interface", "opaque", "byte-preserved",
                      "existing schedule ID", "PASEO_HOME", "PASEO_AGENT_ID", "paid fallback", "Private build-tab",
                      "does not deploy the Worker", "stored scheduler prompts"):
            self.assertIn(value, integrations)


if __name__ == "__main__":
    unittest.main()
