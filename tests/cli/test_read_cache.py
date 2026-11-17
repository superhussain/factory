import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
PATH = "/api/pipeline/effective/sample"
VIEW = {"project": "sample", "merge": "human", "review_rounds": 1, "reviewers": ["security", "ux", "ui", "code"],
        "gates": [], "stage_notes": {}, "versions": {"global": 1, "project": 1},
        "floors": ["F" + str(n) for n in range(1, 12)],
        "pickup": {"enabled": True, "hosts": ["host-a"]}, "triage": {"enabled": True}}


class ReadCacheTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)

    def config(self, server):
        (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")

    def reads(self, server):
        return [r[1] for r in server.requests if r[0] == "GET"]

    def test_one_read_per_command(self):
        with http_fixture({PATH: (200, VIEW)}) as server, patch.object(factory, "_READS", {}):
            self.config(server)
            self.assertTrue(factory.triage_enabled(self.home, "sample"))
            self.assertEqual(factory.hold_config(self.home, "sample"), (factory.HOLD_MIN_IDEAS, factory.HOLD_MAX_HOURS))
            self.assertEqual(factory.pickup_config(self.home, "sample"), (True, ["host-a"], None))
            self.assertEqual(self.reads(server), [PATH])

    def test_clears_on_pipeline_write(self):
        routes = {PATH: (200, VIEW), "PUT /api/pipeline/retro/global": (200, b""),
                  "PUT /api/projects/sample.md": (200, b""), "PUT /api/board": (200, b"")}
        with http_fixture(routes) as server, patch.object(factory, "_READS", {}):
            self.config(server)
            factory.triage_enabled(self.home, "sample")
            factory.api(self.home, "PUT", "/api/board", "{}")
            factory.triage_enabled(self.home, "sample")
            self.assertEqual(self.reads(server), [PATH])
            for path in ("/api/pipeline/retro/global", "/api/projects/sample.md"):
                factory.api(self.home, "PUT", path, "x")
                factory.triage_enabled(self.home, "sample")
            self.assertEqual(self.reads(server), [PATH] * 3)

    def test_404_not_cached(self):
        with http_fixture({}) as server, patch.object(factory, "_READS", {}):
            self.config(server)
            self.assertFalse(factory.triage_enabled(self.home, "sample"))
            self.assertFalse(factory.triage_enabled(self.home, "sample"))
            self.assertEqual(self.reads(server), [PATH, PATH])

    def test_expires_after_ttl(self):
        clock = [1000.0]
        with http_fixture({PATH: (200, VIEW)}) as server, patch.object(factory, "_READS", {}), \
                patch.object(factory.time, "monotonic", lambda: clock[0]):
            self.config(server)
            factory.triage_enabled(self.home, "sample")
            clock[0] += factory.READ_TTL - 1
            factory.triage_enabled(self.home, "sample")
            self.assertEqual(self.reads(server), [PATH])
            clock[0] += 1
            factory.triage_enabled(self.home, "sample")
            self.assertEqual(self.reads(server), [PATH, PATH])

    def test_scope_is_one_command(self):
        with http_fixture({PATH: (200, VIEW)}) as server:
            self.config(server)
            for _ in range(2):
                code, out, err = invoke(factory, ["--home", str(self.home), "pipeline", "sample"])
                self.assertEqual((code, err), (0, ""))
                self.assertEqual(json.loads(out)["project"], "sample")
            self.assertEqual(self.reads(server), [PATH, PATH])
        self.assertIsNone(factory._READS)
