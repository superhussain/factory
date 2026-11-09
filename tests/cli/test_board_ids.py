import io
import json
import os
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()


class BoardIdTest(IsolatedTestCase):
    def setUp(self):
        self.home = Path(os.environ["HOME"], "factory")
        self.home.mkdir()
        (self.home / "config.md").write_text("---\ndevice: test\n---\n")
        self.puts = 0
        self.text = factory.dump_board({"updated": "", "jobs": [
            {"id": "412-fix-login", "project": "sample", "stage": "build"}]})
        api = patch.object(factory, "api", side_effect=self.api)
        api.start()
        self.addCleanup(api.stop)

    def api(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if method == "PUT":
            self.puts += 1
            self.text = body
        return 200, {"ETag": '"synthetic"'}, self.text

    def patch_board(self, job_id, body='{"stage": "closed", "project": "sample"}'):
        with patch.object(factory.sys, "stdin", io.StringIO(body)), \
                patch.dict(os.environ, {"FACTORY_SESSION": "tag", "PASEO_AGENT_ID": ""}):
            return invoke(factory, ["board", str(self.home), job_id])

    def ids(self):
        return [j["id"] for j in json.loads(self.text)["jobs"]]

    def test_refuses_a_malformed_new_id(self):
        for job_id in ("412-fix-login 413-add-export", "fix-login", "412", "412-", "-fix", "412-Fix",
                       "412_fix", "412-fix_login", "412--fix", "412-fix-", "412-fix\n", "４１２-fix", ""):
            with self.subTest(job_id=job_id):
                self.assertEqual(self.patch_board(job_id), (2, "", (
                    f"factory.py: bad job id {job_id!r}: a new job id is digits, a dash and a lowercase slug, "
                    "like 003-rate-limiting. Nothing was written.\n")))
                self.assertEqual((self.ids(), self.puts), (["412-fix-login"], 0))

    def test_refuses_an_id_key_in_the_patch(self):
        for job_id, body in (("412-fix-login", '{"id": "999-renamed"}'),
                             ("413-add-export", '{"id": "412-fix-login 413-add-export"}'),
                             ("412-fix-login", '{"id": null}'),
                             ("412-fix-login", '{"id": "412-fix-login", "stage": "closed"}')):
            with self.subTest(job_id=job_id, body=body):
                self.assertEqual(self.patch_board(job_id, body), (2, "", (
                    "factory.py: the job id comes from the <job-id> argument, so the patch cannot set id. "
                    "Nothing was written.\n")))
                self.assertEqual((self.ids(), self.puts), (["412-fix-login"], 0))

    def test_refuses_a_new_row_without_stage(self):
        self.assertEqual(self.patch_board("413-add-export", '{"note": "x"}'), (2, "", (
            "factory.py: 413-add-export is not on the board, and a new row must set stage. "
            "Claim new ids with claim. Nothing was written.\n")))
        self.assertEqual((self.ids(), self.puts), (["412-fix-login"], 0))

    def test_refuses_clearing_stage(self):
        for body in ('{"stage": null}', '{"stage": ""}'):
            with self.subTest(body=body):
                self.assertEqual(self.patch_board("412-fix-login", body), (2, "", (
                    "factory.py: a board write cannot clear 412-fix-login's stage. "
                    'Close the job with {"stage": "closed"} instead. Nothing was written.\n')))
                self.assertEqual((self.ids(), self.puts), (["412-fix-login"], 0))

    def test_adds_a_well_formed_new_id(self):
        for job_id in ("413-add-export", "414-a", "1000-a1-b2"):
            with self.subTest(job_id=job_id):
                code, out, _ = self.patch_board(job_id)
                self.assertEqual((code, json.loads(out)["id"]), (0, job_id))
        self.assertEqual(self.ids(), ["412-fix-login", "413-add-export", "414-a", "1000-a1-b2"])

    def test_patches_an_existing_row_whatever_its_id(self):
        for job_id in ("412-fix-login", "acme-009-web-nav", "412-fix-login 413-add-export"):
            with self.subTest(job_id=job_id):
                self.text = factory.dump_board({"updated": "", "jobs": [
                    {"id": job_id, "project": "sample", "stage": "build"}]})
                code, out, _ = self.patch_board(job_id)
                self.assertEqual((code, json.loads(out)["stage"]), (0, "closed"))
                self.assertEqual(self.ids(), [job_id])
