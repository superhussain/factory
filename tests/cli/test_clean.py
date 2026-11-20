import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from support import IsolatedTestCase, invoke, load_runtime


factory = load_runtime()


class CleanTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        self.wt = self.home / "worktrees" / "sample" / "001-test"
        self.wt.mkdir(parents=True)
        self.git("init", "-q")
        (self.wt / ".gitignore").write_text("/node_modules/\n/sub/node_modules/\n/.context/\n")
        (self.wt / "sub").mkdir()
        (self.wt / "sub" / "tracked.txt").write_text("tracked\n")
        self.git("add", ".")
        self.git("commit", "-q", "-m", "base")

    def git(self, *args):
        subprocess.run(["git", "-C", str(self.wt), *args], check=True, capture_output=True)

    def clean(self, *args, target="sample/001-test"):
        code, out, err = invoke(factory, ["--home", str(self.home), "clean", target, *args])
        return code, (json.loads(out) if out else None), err

    def folder(self, rel):
        path = self.wt / rel
        path.mkdir(parents=True)
        (path / "file.js").write_text("x\n")
        return path

    def test_node_modules_removes_ignored_folders(self):
        top, sub = self.folder("node_modules"), self.folder("sub/node_modules")
        self.folder(".context/capture-1")
        code, out, err = self.clean("node_modules")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, {"removed": [str(top), str(sub)], "left": []})
        self.assertFalse(top.exists() or sub.exists())
        self.assertTrue((self.wt / "sub" / "tracked.txt").is_file())
        self.assertTrue((self.wt / ".gitignore").is_file())
        self.assertTrue((self.wt / ".context" / "capture-1" / "file.js").is_file())

    def test_scratch_removes_one_folder(self):
        one, other = self.folder(".context/capture-1"), self.folder(".context/capture-2")
        modules = self.folder("node_modules")
        code, out, err = self.clean("scratch", "capture-1")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, {"removed": [str(one)], "left": []})
        self.assertFalse(one.exists())
        self.assertTrue(other.is_dir() and modules.is_dir())
        self.assertEqual(self.clean("scratch", "absent"), (0, {"removed": [], "left": []}, ""))

    def test_scratch_not_ignored_is_left(self):
        (self.wt / ".gitignore").write_text("/node_modules/\n")
        kept = self.folder(".context/capture-1")
        code, out, _ = self.clean("scratch", "capture-1")
        self.assertEqual((code, out), (1, {"removed": [], "left": [f"{kept}: not ignored by git, left in place"]}))
        self.assertTrue((kept / "file.js").is_file())

    def test_link_is_left_in_place(self):
        outside = self.home / "outside"
        outside.mkdir()
        (outside / "keep.txt").write_text("keep\n")
        try:
            os.symlink(outside, self.wt / "node_modules", target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unavailable")
        (self.wt / ".gitignore").write_text("/node_modules\n/.context/\n")
        (self.wt / ".context").mkdir()
        os.symlink(outside, self.wt / ".context" / "capture-1", target_is_directory=True)
        code, out, _ = self.clean("node_modules")
        self.assertEqual((code, out), (1, {"removed": [], "left": [
            f"{self.wt / 'node_modules'}: a symlink or junction, left in place"]}))
        code, out, _ = self.clean("scratch", "capture-1")
        self.assertEqual((code, out), (1, {"removed": [], "left": [
            f"{self.wt / '.context' / 'capture-1'}: a symlink or junction, left in place"]}))
        self.assertTrue((outside / "keep.txt").is_file())

    def test_rmtree_refusal_names_the_error(self):
        path = self.folder(".context/capture-1")
        with mock.patch.object(factory.shutil, "rmtree",
                               side_effect=OSError("Cannot call rmtree on a symbolic link")):
            code, out, _ = self.clean("scratch", "capture-1")
        self.assertEqual((code, out), (1, {"removed": [], "left": [
            f"{path}: Cannot call rmtree on a symbolic link"]}))

    def test_worktree_link_and_missing(self):
        code, out, err = self.clean("node_modules", target="sample/002-absent")
        self.assertEqual((code, out), (4, None))
        self.assertIn(f"no worktree {self.home / 'worktrees' / 'sample' / '002-absent'}; nothing was removed", err)
        link = self.home / "worktrees" / "sample" / "003-link"
        try:
            os.symlink(self.wt, link, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unavailable")
        modules = self.folder("node_modules")
        code, out, err = self.clean("node_modules", target="sample/003-link")
        self.assertEqual((code, out), (6, None))
        self.assertIn(f"{link} is a symlink or junction; nothing was removed", err)
        self.assertTrue(modules.is_dir())

    def test_bad_usage_exits_2(self):
        modules, scratch = self.folder("node_modules"), self.folder(".context/capture-1")
        for args in (("scratch", "../x"), ("scratch",), ("node_modules", "extra"), ("dist",),
                     ("scratch", ".hidden")):
            code, out, err = self.clean(*args)
            self.assertEqual((code, out), (2, None), args)
            self.assertIn(factory.USAGE_CLEAN, err)
        self.assertEqual(self.clean("node_modules", target="sample/../x")[0], 2)
        self.assertTrue(modules.is_dir() and scratch.is_dir())


if __name__ == "__main__":
    unittest.main()
