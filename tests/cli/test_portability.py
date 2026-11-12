import ast
import importlib.util
import json
import os
import re
import subprocess

from support import ROOT, SKILL, IsolatedTestCase


def tracked_files():
    try:
        result = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True)
        relatives = [name for name in result.stdout.decode("utf-8").split("\0") if name]
        paths = [ROOT / relative for relative in relatives]
    except (OSError, subprocess.SubprocessError):
        paths = [path for path in ROOT.rglob("*") if path.is_file()
                 and not (set(path.relative_to(ROOT).parts) & {".git", "node_modules"})]
    return sorted(path for path in paths if path.is_file())


def owned_files():
    roots = [SKILL, ROOT / "tests/cli"]
    paths = [p for root in roots for p in root.rglob("*") if p.is_file()
             and "__pycache__" not in p.parts and p.suffix not in (".pyc", ".pyo")]
    return paths + [ROOT / name for name in (
        "tools/install.py", "tests/tools/test_install.py", "tests/fixtures/cli-v1.json",
        "docs/device.md", "docs/integrations.md", ".github/workflows/ci.yml",
        ".github/actions/ci-setup/action.yml")]


class PortabilityTest(IsolatedTestCase):
    def test_supplied_forbidden_literals_cover_all_tracked_files(self):
        literals = json.loads(os.environ.get("FACTORY_FORBID_LITERALS_JSON", "[]"))
        self.assertIsInstance(literals, list)
        for path in tracked_files():
            raw = path.read_bytes()
            relative = path.relative_to(ROOT).as_posix()
            try:
                content = relative + "\n" + raw.decode("utf-8")
            except UnicodeDecodeError:
                content = raw.lower()
            for literal in literals:
                self.assertIsInstance(literal, str)
                if isinstance(content, str):
                    self.assertNotIn(literal.casefold(), content.casefold(), relative)
                else:
                    self.assertNotIn(literal.lower().encode("utf-8"), content, relative)

    def test_owned_documents_are_valid_utf8(self):
        for path in owned_files():
            with self.subTest(str(path.relative_to(ROOT))):
                path.read_bytes().decode("utf-8")

    def test_python_38_syntax_and_stdlib_only_imports(self):
        # shlex substitutes launcher argv; zoneinfo resolves usage-limit reset times but is
        # imported under try/except ImportError in factory.py, because Python 3.8 lacks it.
        allowed = set("argparse ast concurrent contextlib datetime errno fnmatch hashlib http importlib io json os pathlib platform random re secrets shlex shutil signal socket ssl stat statistics subprocess sys tarfile tempfile threading time types unicodedata unittest urllib uuid xml zoneinfo".split())
        for path in owned_files():
            if path.suffix != ".py":
                continue
            tree = ast.parse(path.read_text(), filename=str(path), feature_version=8)
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    names = [item.name.split(".")[0] for item in node.names]
                elif isinstance(node, ast.ImportFrom):
                    names = [node.module.split(".")[0]]
                else:
                    continue
                self.assertTrue(set(names) <= allowed | {"support", "test_factory", "test_suggest"}, (path.name, names))

    def test_exact_bundle_and_relative_installed_references(self):
        spec = importlib.util.spec_from_file_location("installer", ROOT / "tools/install.py")
        installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(installer)
        files, _ = installer.tree(SKILL, source=True)
        self.assertEqual(set(files), set(installer.REQUIRED))
        self.assertIn("runtime/pr_screenshots.py", files)
        self.assertEqual(len(files), 39)
        for name, data in files.items():
            text = data.decode("utf-8")
            self.assertNotIn("file://", text)
            self.assertNotRegex(text, r"(?:^|\s)(?:/home/|/[Uu]sers/|[A-Za-z]:\\Users\\)\S+")
            if name.endswith(".md"):
                for link in re.findall(r"\]\(([^)]+)\)", text):
                    if link.startswith(("references/", "execution.md", "integrations.md")):
                        self.assertTrue((SKILL / name).parent.joinpath(link.split("#")[0]).is_file(), (name, link))

    def test_skill_md_is_small_and_indexes_every_command(self):
        skill = (SKILL / "SKILL.md").read_text(encoding="utf-8")
        self.assertLess(len(skill.encode("utf-8")), 45000)
        commands = sorted((SKILL / "references/commands").glob("*.md"))
        for path in commands:
            self.assertIn(f"](references/commands/{path.name})", skill)

    def test_documented_setup_recovery_and_policy_topics(self):
        device = (ROOT / "docs/device.md").read_text()
        integrations = (ROOT / "docs/integrations.md").read_text()
        for value in ("Python 3.8+", "tools/install.py --destination <absolute-directory>",
                      "source checkout is unavailable", "FACTORY_ACCESS_CLIENT_ID", "--connection",
                      "XDG_CONFIG_HOME", "adopt-home", "fsync", "hard-link", "doctor", "init <repo>",
                      "where sample", "claim <factory-home>", "sync sample", "handoff sample/", "ACL"):
            self.assertIn(value, device)
        for code in range(1, 7):
            self.assertIn("| " + str(code) + " |", device)
        for value in ("opencode api get /api/agent", "native agent interface", "opaque", "byte-preserved",
                      "existing schedule ID", "PASEO_HOME", "PASEO_AGENT_ID", "paid fallback", "Private build-tab",
                      "does not deploy the Worker", "stored scheduler prompts"):
            self.assertIn(value, integrations)
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        setup = (ROOT / ".github/actions/ci-setup/action.yml").read_text()
        self.assertIn("on: pull_request\n", workflow)
        self.assertNotIn("push", workflow)
        self.assertEqual(workflow.count("actions/checkout@"), 3)
        self.assertEqual(workflow.count("uses: ./.github/actions/ci-setup"), 3)
        self.assertIn("using: composite", setup)
        self.assertIn("actions/setup-python@", setup)
        self.assertIn("actions/setup-node@", setup)
        self.assertEqual(workflow.count("runs-on: ubuntu-22.04"), 4)
        self.assertEqual(workflow.count("runs-on:"), 4)
        self.assertIn("python-version: '3.12'", setup)
        self.assertIn("node-version-file: .node-version", setup)
        pnpm_ref = "pnpm/action-setup@73e46992e05b19abd31f2f3c20e0e7370d762115"
        self.assertIn(pnpm_ref, setup)
        self.assertIn("run_install: false", setup)
        self.assertLess(setup.index("actions/setup-node@"), setup.index(pnpm_ref))
        self.assertNotIn("version: 11.9.0", setup)  # action reads packageManager
        self.assertNotIn("cache:", workflow)
        self.assertNotIn("cache:", setup)
        self.assertIn('test "$(pnpm --version)" = 11.9.0', setup)
        self.assertIn("github.event.pull_request.number", workflow)
        self.assertIn("cancel-in-progress: true", workflow)
        self.assertIn("contents: read", workflow)
        self.assertEqual(workflow.count("matrix:"), 1)
        self.assertIn("shard: [1, 2, 3, 4]", workflow)
        self.assertNotIn("os:", workflow)
        self.assertEqual(setup.count("pnpm install --frozen-lockfile"), 1)
        self.assertEqual(workflow.count("pnpm install --frozen-lockfile"), 0)
        self.assertEqual(workflow.count("node tests/browser/provision.mjs"), 2)
        self.assertEqual(workflow.count("unittest discover"), 0)
        self.assertEqual(workflow.count("nx run-many -t cli install verify"), 1)
        self.assertIn("actions/cache/restore@", setup)
        self.assertIn("node-cache-Linux-x64-pnpm-", setup)
        self.assertNotIn(".nx/cache", workflow)
        self.assertNotIn(".nx/cache", setup)
        self.assertEqual(workflow.count("pnpm run format:check"), 1)
        self.assertEqual(workflow.count("pnpm test"), 0)
        self.assertEqual(workflow.count("pnpm run test:local"), 0)
        self.assertEqual(workflow.count("pnpm run test:browser"), 0)
        self.assertEqual(workflow.count("affected -t auth protocol assets deploy local --base"), 1)
        self.assertEqual(workflow.count("affected -t browser --base"), 1)
        self.assertNotIn("--exclude-task-dependencies", workflow)
        self.assertNotIn("verify.py all", workflow)
        self.assertNotIn("npm ci", workflow)
        self.assertNotIn("pip install", workflow)
