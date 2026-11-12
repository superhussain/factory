import contextlib
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

    def batched(self):
        self.text = factory.dump_board({"updated": "", "jobs": [
            {"id": "412-fix-login", "project": "sample", "stage": "build",
             "refs": [{"id": "411-sample-lead", "kind": "batched-into"}]}]})

    def test_refuses_removing_a_batched_into_ref(self):
        for body in ('{"refs": null}', '{"refs": []}'):
            with self.subTest(body=body):
                self.batched()
                self.assertEqual(self.patch_board("412-fix-login", body), (2, "", (
                    "factory.py: only factory.py batch and the lead's mirror add or remove a batched-into ref. "
                    "A refs patch for 412-fix-login must keep exactly the batched-into refs it has now, none for a new row. Nothing was written.\n")))
                self.assertEqual(self.puts, 0)

    def test_refuses_adding_a_batched_into_ref(self):
        refs = '"refs": [{"id": "411-sample-lead", "kind": "batched-into"}]'
        for job_id, body in (("412-fix-login", "{" + refs + "}"),
                             ("413-add-export", '{"stage": "idea", "project": "sample", ' + refs + "}")):
            with self.subTest(job_id=job_id):
                self.assertEqual(self.patch_board(job_id, body), (2, "", (
                    "factory.py: only factory.py batch and the lead's mirror add or remove a batched-into ref. "
                    f"A refs patch for {job_id} must keep exactly the batched-into refs it has now, none for a new row. Nothing was written.\n")))
                self.assertEqual((self.ids(), self.puts), (["412-fix-login"], 0))

    def test_keeps_a_batched_into_ref_while_editing_refs(self):
        self.batched()
        refs = [{"id": "411-sample-lead", "kind": "batched-into"}, {"id": "400-sample-old", "kind": "follow-up-of"}]
        code, out, _ = self.patch_board("412-fix-login", json.dumps({"refs": refs}))
        self.assertEqual((code, json.loads(out)["refs"], self.puts), (0, refs, 1))

    def test_refuses_a_bad_refs_kind(self):
        code, out, err = self.patch_board(
            "412-fix-login", json.dumps({"refs": [{"id": "411-sample-lead", "kind": "javascript:"}]}))
        self.assertEqual((code, out), (2, ""))
        self.assertIn("refs must be a list", err)
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


class ArchiveFloorTest(IsolatedTestCase):
    """Ids up to the board's archived.max_id belong to archived jobs, so none is handed out again."""

    def setUp(self):
        self.home = Path(os.environ["HOME"], "factory")
        self.home.mkdir()
        (self.home / "config.md").write_text("---\ndevice: test\n---\n")
        self.files = {
            "/api/board": factory.dump_board({"updated": "", "jobs": [
                {"id": "414-retry-sync", "project": "sample", "stage": "closed"},
                {"id": "415-add-export", "project": "sample", "stage": "merged"}],
                "archived": {"max_id": 416, "fingerprints": {}}}),
            "/api/backlog": "- [ ] sample: Add thing\n",
        }
        api = patch.object(factory, "api", side_effect=self.api)
        api.start()
        self.addCleanup(api.stop)

    def api(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        if path.startswith("/api/board/archive?"):
            return (200, {}, json.dumps({"jobs": [{"id": "412-fix-login", "stage": "merged"}]})) \
                if path == "/api/board/archive?id=412-fix-login" else (404, {}, "not found")
        if method == "PUT":
            self.files[path] = body
        return 200, {"ETag": '"synthetic"'}, self.files[path]

    def ids(self):
        return [j["id"] for j in json.loads(self.files["/api/board"])["jobs"]]

    def board(self, job_id, body):
        with patch.object(factory.sys, "stdin", io.StringIO(body)), \
                patch.dict(os.environ, {"FACTORY_SESSION": "tag", "PASEO_AGENT_ID": ""}):
            return invoke(factory, ["board", str(self.home), job_id])

    def test_archive_floor_reads_only_a_positive_int(self):
        for archived, floor in (({"max_id": 416}, 416), ({"max_id": True}, 0), ({"max_id": "416"}, 0),
                                ({"max_id": -1}, 0), ([416], 0), (None, 0)):
            with self.subTest(archived=archived):
                self.assertEqual(factory.archive_floor({"jobs": [], "archived": archived}), floor)
        self.assertEqual(factory.archive_floor({"jobs": []}), 0)
        self.assertEqual(factory.next_job_id(["415-add-export"], "x", 416), "417-x")
        self.assertEqual(factory.next_job_id(["420-later"], "x", 416), "421-x")

    def test_claim_starts_above_the_floor(self):
        with patch.dict(os.environ, {"FACTORY_SESSION": "tag", "PASEO_AGENT_ID": ""}):
            self.assertEqual(factory.claim_id(self.home, "new-thing", "sample"), "417-new-thing")
        self.assertEqual(self.ids()[-1], "417-new-thing")

    def test_backlog_triage_queues_above_the_floor(self):
        verdict = {"model": "m", "confidence": 90, "threshold": 80, "reason": "Small."}
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertFalse(factory.queue_backlog_item(self.home, "sample", "Add thing", verdict))
        self.assertEqual(self.ids()[-1], "417-add-thing")
        self.assertIn("(417-add-thing)", self.files["/api/backlog"])

    def test_board_refuses_an_archived_id(self):
        message = ("factory.py: ids up to 416 belong to archived jobs, so 412-fix-login cannot go back on "
                   "the board. Read it with factory.py job 412-fix-login. Claim new ids with claim. "
                   "Nothing was written.\n")
        before = self.files["/api/board"]
        for body in ('{"stage": "closed", "project": "sample"}', '{"note": "x"}'):
            with self.subTest(body=body):
                self.assertEqual(self.board("412-fix-login", body), (2, "", message))
                self.assertEqual(self.files["/api/board"], before)
        self.assertEqual(self.board("417-new-thing", '{"stage": "closed", "project": "sample"}')[0], 0)
        self.assertEqual(self.board("414-retry-sync", '{"note": "x"}')[0], 0)

    def test_board_patches_without_an_archived_field(self):
        self.files["/api/board"] = factory.dump_board(
            {"updated": "", "jobs": [{"id": "414-retry-sync", "project": "sample", "stage": "closed"}]})
        self.assertEqual(self.board("413-add-export", '{"stage": "closed", "project": "sample"}')[0], 0)
        self.assertIn("413-add-export", self.ids())

    def test_board_skips_the_floor_for_an_existing_row(self):
        with patch.object(factory, "archive_floor", wraps=factory.archive_floor) as floor:
            self.assertEqual(self.board("414-retry-sync", '{"note": "x"}')[0], 0)
            floor.assert_not_called()
            self.assertEqual(self.board("417-new-thing", '{"stage": "closed", "project": "sample"}')[0], 0)
            floor.assert_called_once()

    def test_job_falls_back_to_the_archive(self):
        with patch.object(factory, "factory_home", return_value=self.home):
            code, out, err = invoke(factory, ["job", "412-fix-login"])
            self.assertEqual((code, json.loads(out), err), (0, {"id": "412-fix-login", "stage": "merged"}, ""))
            self.assertEqual(invoke(factory, ["job", "415-add-export"])[0], 0)
            self.assertEqual(invoke(factory, ["job", "999-missing"]), (
                4, "", "factory.py: no job 999-missing on the board or in the archive. Nothing was written.\n"))
