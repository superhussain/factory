import os
import re
import shutil
from pathlib import Path
from unittest.mock import patch
from xml.etree import ElementTree

from support import ROOT, SKILL, IsolatedTestCase, load_runtime, snapshot


class RolesTest(IsolatedTestCase):
    HEADERS = (
        b'---\nmodel: provider/model # local pin\npermission: deny\n---\n',
        b'---\r\n# comment\r\nmodel: "provider/model#variant"\r\nunknown: [\r\n---\r\n',
        b'---\ndescription: >-\n  expanded description\npermission:\n  bash: deny\n---\n',
        b'---\nmodel: "invalid YAML\nunknown: true\n---',
    )

    def setUp(self):
        self.runtime = load_runtime()
        self.root = Path(os.environ["HOME"]).resolve()
        self.home = self.root / "state"
        self.home.mkdir()
        self.config = self.home / "config.md"
        self.config.write_text("---\napi_url:\n---\n")
        self.claude = self.root / ".claude/agents"
        self.opencode = Path(os.environ["XDG_CONFIG_HOME"]).resolve() / "opencode/agents"
        self.addCleanup(patch.stopall)
        patch.object(self.runtime, "factory_home", return_value=self.home).start()

    def install(self, selected=("claude", "opencode"), skill=SKILL):
        self.runtime.install_agents(skill, selected)

    def refused(self, call):
        before = snapshot(self.root)
        with self.assertRaises(SystemExit) as error:
            call()
        self.assertEqual(error.exception.code, 2)
        self.assertEqual(snapshot(self.root), before)

    def test_separate_and_combined_selection_and_custom_locations(self):
        self.install(())
        self.assertFalse(self.claude.exists())
        self.assertFalse(self.opencode.exists())
        self.install(("claude",))
        self.assertEqual(len(list(self.claude.iterdir())), 9)
        self.assertFalse(self.opencode.exists())
        self.install(("opencode",))
        for name in self.runtime.ROLE_NAMES:
            self.assertEqual((self.claude / name).read_bytes(), (SKILL / "agents" / name).read_bytes())
            meta = self.runtime.frontmatter(self.opencode / name)[0]
            self.assertEqual(set(meta), {"description", "mode"})
            self.assertEqual(meta["mode"], "all")
        self.config.write_text("---\nclaude_agents_dir: " + str(self.root / "custom-claude") +
                               "\nopencode_agents_dir: " + str(self.root / "custom-opencode") + "\n---\n")
        self.install()
        self.assertEqual(len(list((self.root / "custom-claude").iterdir())), 9)
        self.assertEqual(len(list((self.root / "custom-opencode").iterdir())), 9)

    def test_every_opaque_header_on_every_role_in_both_runtimes(self):
        self.install()
        for directory in (self.claude, self.opencode):
            unrelated = directory / "unrelated.md"
            unrelated.write_bytes(b"unrelated content")
            for name in self.runtime.ROLE_NAMES:
                source = SKILL / "agents" / name
                prefix = self.runtime.native_header(source).encode()
                body = source.read_bytes()[len(prefix):]
                self.assertNotIn(b"model:", prefix)
                for header in self.HEADERS:
                    destination = directory / name
                    destination.write_bytes(header + b"\nold body")
                    self.install()
                    separator = b"" if header.endswith(b"\n") else b"\n"
                    self.assertEqual(destination.read_bytes(), header + separator + body)
            self.assertEqual(unrelated.read_bytes(), b"unrelated content")

    def test_malformed_last_role_prevents_all_writes_for_either_runtime(self):
        self.install()
        for directory in (self.claude, self.opencode):
            last = directory / self.runtime.ROLE_NAMES[-1]
            saved = last.read_bytes()
            for invalid in (b"plain", b"---\nmissing close", b"---\n...\n---\n", b" ---\n---\n"):
                last.write_bytes(invalid)
                self.refused(self.install)
            last.write_bytes(saved)

    def test_source_and_destination_links_overlap_and_relative_paths_refused(self):
        clone = self.root / "skill"
        shutil.copytree(SKILL, clone)
        last = clone / "agents" / self.runtime.ROLE_NAMES[-1]
        last.unlink()
        self.refused(lambda: self.install(skill=clone))
        for path in ("relative", str(SKILL / "agents")):
            self.config.write_text("---\nclaude_agents_dir: " + path + "\n---\n")
            self.refused(self.install)
        shared = str(self.root / "shared")
        self.config.write_text("---\nclaude_agents_dir: " + shared + "\nopencode_agents_dir: " + shared + "\n---\n")
        self.refused(self.install)
        if os.name == "nt":
            return
        self.config.write_text("---\napi_url:\n---\n")
        self.install()
        victim = self.claude / self.runtime.ROLE_NAMES[-1]
        victim.unlink()
        victim.symlink_to(SKILL / "agents" / victim.name)
        self.refused(self.install)
        victim.unlink()
        last.symlink_to(SKILL / "agents" / last.name)
        self.refused(lambda: self.install(skill=clone))

    def test_disk_failure_names_destination_and_stops(self):
        self.install()
        with patch.object(self.runtime, "write_atomic", side_effect=OSError("synthetic failure")) as write:
            with self.assertRaises(SystemExit) as error:
                self.install()
        self.assertEqual(error.exception.code, 2)
        self.assertEqual(write.call_count, 1)

    def test_prompt_contracts_and_both_fresh_approval_rows(self):
        skill = (SKILL / "SKILL.md").read_text(encoding="utf-8")
        execution = (SKILL / "references/execution.md").read_text(encoding="utf-8")
        commands = SKILL / "references/commands"
        feature = (commands / "feature.md").read_text(encoding="utf-8")
        recover = (commands / "recover.md").read_text(encoding="utf-8")
        self.assertIn("WAIT FOR ALL", skill)
        self.assertIn("Max `$PIPELINE.review_rounds` review rounds", skill)
        self.assertIn("One approver CHANGES", feature)
        for text in (recover, execution):
            row = next(line for line in text.splitlines() if line.startswith("| approve |"))
            self.assertIn("fresh approver", row)
            self.assertIn("current HEAD and round", row)
            self.assertIn("never ship", row)
        for name in self.runtime.ROLE_NAMES:
            text = (SKILL / "agents" / name).read_text(encoding="utf-8")
            self.assertNotIn("\nmodel:", text)
            if "review-" in name:
                self.assertIn("VERDICT: PASS", text)
                self.assertIn("VERDICT: CHANGES", text)
                self.assertIn(name.replace("factory-", "").replace(".md", ".md"), text)
        self.assertIn("two failed candidate verifications total", (SKILL / "agents/factory-builder.md").read_text(encoding="utf-8"))
        self.assertIn("Private build-tab evidence", skill)
        self.assertEqual(skill.count("decisions-via-askuserquestion"), 1)
        self.assertEqual((commands / "next.md").read_text(encoding="utf-8").count("decisions-via-askuserquestion"), 1)
        self.assertIn("is the question tool it names", " ".join(skill.split()))
        blocks = []
        for name in self.runtime.ROLE_NAMES:
            if name in ("factory-explorer.md", "factory-retro.md"):
                continue
            text = (SKILL / "agents" / name).read_text(encoding="utf-8")
            start = text.index("### Job files stay readable")
            end = text.index("\nThe dashboard renders markdown tables", start)
            blocks.append(text[start:end])
            self.assertIn("120 words", blocks[-1])
            self.assertIn("spec.md 800", blocks[-1])
            self.assertIn("<file>-evidence.md", blocks[-1])
        self.assertEqual(len(set(blocks)), 1)
        visuals = []
        for name in self.runtime.ROLE_NAMES:
            if name in ("factory-explorer.md", "factory-retro.md"):
                continue
            text = (SKILL / "agents" / name).read_text(encoding="utf-8")
            start = text.index("\nThe dashboard renders markdown tables")
            tail = "free of secrets and user data.\n"
            visuals.append(text[start:text.index(tail, start) + len(tail)])
        self.assertEqual(len(set(visuals)), 1)
        for phrase in ("3 to 6 bullets", "one running example", "classDef", "strict mode", "Word budgets count prose"):
            self.assertIn(phrase, visuals[0])
        self.assertNotIn("sentence or two per point", visuals[0])
        self.assertIn("walkthrough of 4 to 8 short steps", (SKILL / "agents/factory-spec-writer.md").read_text(encoding="utf-8"))
        self.assertIn("#### N. Title", (SKILL / "agents/factory-spec-writer.md").read_text(encoding="utf-8"))
        writer = (SKILL / "agents/factory-spec-writer.md").read_text(encoding="utf-8")
        self.assertIn("walkthrough.html", writer)
        self.assertIn("load nothing from outside", writer)
        self.assertIn("When you revise an existing spec that has no walkthrough", writer)
        for phrase in ("Also write `walkthrough.html`", "changes something a person sees",
                       "is a complex system change", "changes a protocol or wire format",
                       "touches three or more components or hosts", "Other jobs may skip it",
                       "or add one when it helps",
                       "synthetic or code-level values only", "it under 1 MiB"):
            self.assertIn(phrase, " ".join(writer.split()))
        self.assertNotIn("you may also write", writer)
        self.assertNotIn("Skip it for other jobs", writer)
        self.assertIn("also get a `walkthrough.html`", skill)
        self.assertIn("Other jobs may skip it", skill)
        self.assertNotIn("A spec may add an", skill)
        self.assertIn("walkthrough.html", skill)
        self.assertIn("### Step 1. Title", skill)
        self.assertIn("shows as a stepper", skill)
        self.assertIn("step-by-step walkthrough", skill)
        self.assertIn("<file>-evidence.md", skill)
        for budget in ("spec 800", "build run section 300", "review 400", "decision 200"):
            self.assertIn(budget, skill)
        text = (SKILL / "schedules" / "resume.md").read_text(encoding="utf-8")
        self.assertEqual(text.count("stuck --limit 2"), 1)
        for value in ("<installed-skill>", "<python-launcher>", "needs-human", "/factory recover"):
            self.assertIn(value, text)
        self.assertFalse((SKILL / "schedules" / "ideas.md").exists())

    def test_walkthrough_pages_and_visuals(self):
        own = {
            "factory-spec-writer.md": "Also write `walkthrough.html`",
            "factory-builder.md": "`<job folder>/build.walkthrough.html`",
            "factory-review-code.md": "`<round folder>/review-code.walkthrough.html`",
            "factory-review-security.md": "`<round folder>/review-security.walkthrough.html`",
            "factory-review-ui.md": "`<round folder>/review-ui.walkthrough.html`",
            "factory-review-ux.md": "`<round folder>/review-ux.walkthrough.html`",
            "factory-approver.md": "`<job folder>/decision.walkthrough.html`",
        }
        shared = set()
        for name, phrase in own.items():
            text = (SKILL / "agents" / name).read_text(encoding="utf-8")
            start = text.index("An interactive walkthrough is a self-contained HTML page")
            shared.add(text[start:text.index("\n\n", start)])
            self.assertIn(phrase, text, name)
        self.assertEqual(len(shared), 1)
        block = shared.pop()
        self.assertIn("`<page>.walkthrough.html`", block)
        self.assertIn("references/visuals.md", block)
        self.assertIn("references/walkthrough.html", block)
        self.assertNotIn("stepthru", block)
        for name in ("factory-explorer.md", "factory-retro.md"):
            self.assertNotIn("walkthrough", (SKILL / "agents" / name).read_text(encoding="utf-8"))
        skill = (SKILL / "SKILL.md").read_text(encoding="utf-8")
        self.assertIn("build.walkthrough.html", skill)
        self.assertIn("references/visuals.md", skill)
        visuals = (SKILL / "references/visuals.md").read_text(encoding="utf-8")
        headings = re.findall(r"^## (.+)$", visuals, re.M)
        for heading in ("Data", "Map", "Visual kinds", "By role", "Flow", "Sequence", "Before and after",
                        "UI mockup", "Markdown pages"):
            self.assertIn(heading, headings)
        self.assertNotIn("Bar chart", headings)
        self.assertNotIn("Timeline", headings)
        svgs = 0
        for html in re.findall(r"```html\n(.*?)```", visuals, re.S):
            for bad in ("`", "${", "'", "http"):
                self.assertNotIn(bad, html)
            if not html.startswith("<svg"):
                continue
            svgs += 1
            svg = ElementTree.fromstring(html)
            self.assertEqual(svg.get("class"), "v")
            self.assertTrue(svg.get("aria-label"))
            view = re.fullmatch(r"0 0 640 (\d+)", svg.get("viewBox"))
            self.assertTrue(view, svg.get("viewBox"))
            self.assertLessEqual(int(view.group(1)), 180)
        self.assertEqual(svgs, 3)

    def test_walkthrough_template(self):
        template = (SKILL / "references/walkthrough.html").read_bytes()
        self.assertLess(len(template), 40000)
        text = template.decode("utf-8")
        for bad in ("http", "@import", "@font-face", "url("):
            self.assertNotIn(bad, text)
        for needed in ("system-ui", "// DATA START", "// DATA END"):
            self.assertIn(needed, text)
        fixture = (ROOT / "tests/fixtures/walkthrough-map.html").read_text(encoding="utf-8")
        outside = lambda page: (page[:page.index("// DATA START")], page[page.index("// DATA END"):])
        self.assertEqual(outside(fixture), outside(text))

    def test_explorer_role_and_schedule(self):
        self.assertIn("factory-explorer.md", self.runtime.ROLE_NAMES)
        role = (SKILL / "agents/factory-explorer.md").read_text(encoding="utf-8")
        self.assertEqual(self.runtime.frontmatter(SKILL / "agents/factory-explorer.md")[0]["name"], "factory-explorer")
        self.assertIn("\ntools: Read, Grep, Glob\n", role.split("\n---\n")[0] + "\n")
        self.assertIn('{"none": true}', role)
        schedule = (SKILL / "schedules/explore.md").read_text(encoding="utf-8")
        for value in ("<installed-skill>", "<python-launcher>", "<project-slug>", "suggest admit",
                      "explore <project-slug> done <run>", "no claude route", "claude-", "factory-explorer"):
            self.assertIn(value, schedule)
        approve = [line for line in schedule.splitlines() if "suggest approve" in line]
        self.assertEqual(len(approve), 1)
        self.assertTrue(approve[0].startswith("Never "))

    def test_retro_role_and_command(self):
        self.assertEqual(self.runtime.ROLE_NAMES[-1], "factory-retro.md")
        role = (SKILL / "agents/factory-retro.md").read_text(encoding="utf-8")
        self.assertEqual(self.runtime.frontmatter(SKILL / "agents/factory-retro.md")[0]["name"], "factory-retro")
        self.assertIn("\ntools: Read, Grep, Glob\n", role.split("\n---\n")[0] + "\n")
        self.assertIn('{"none": true}', role)
        for value in (*self.runtime.RETRO_CATEGORIES, "160 characters", "`<host>/`", "tooling"):
            self.assertIn(value, role)
        command = (SKILL / "references/commands/retro.md").read_text(encoding="utf-8")
        for value in ("retro push", "retro aggregate", "factory-retro", "byte for byte", "retro admit <run>"):
            self.assertIn(value, command)
        self.assertNotIn("suggest approve", command)
        self.assertIn("\nNever run an autonomy command or decide a backlog line; a person decides each line.\n",
                      command)
        self.assertIn("| `/factory retro [--days N]` | [retro](references/commands/retro.md) |",
                      (SKILL / "SKILL.md").read_text(encoding="utf-8"))

    def test_retro_schedule(self):
        schedule = (SKILL / "schedules/retro.md").read_text(encoding="utf-8")
        for value in ("<installed-skill>", "<python-launcher>", "retro: autonomy paused", "no claude route", "claude-",
                      "byte for byte"):
            self.assertIn(value, schedule)
        steps = ("routes pick", "autonomy status", "retro push", "retro aggregate --days 3", "factory-retro",
                 "retro admit <run>")
        positions = [schedule.index(step) for step in steps]
        self.assertEqual(positions, sorted(positions))
        for banned in ("pause", "resume", "allow", "deny", "self"):
            self.assertIsNone(re.search(rf"autonomy {banned}\b", schedule), banned)
        approve = [line for line in schedule.splitlines() if "suggest approve" in line]
        self.assertEqual(len(approve), 1)
        self.assertTrue(approve[0].startswith("Never "))

    def test_unattended_standing_instruction(self):
        line = self.runtime.UNATTENDED
        self.assertNotIn("\n", line)
        text = {name: (SKILL / name).read_text(encoding="utf-8") for name in
                ("SKILL.md", "references/pickup.md", "schedules/resume.md", "schedules/explore.md",
                 "schedules/retro.md")}
        self.assertIn(line, text["references/pickup.md"])
        self.assertIn(line, (ROOT / "web/dashboard.html").read_text(encoding="utf-8"))
        for name in ("schedules/resume.md", "schedules/explore.md", "schedules/retro.md"):
            self.assertEqual(text[name].rstrip().splitlines()[-1], line, name)
        skill = " ".join(text["SKILL.md"].split())
        self.assertIn("is a report, not the end of the run", skill)
        self.assertIn("Interactive `/factory` use never adds it", skill)
        self.assertIn("then the standing instruction that ends this prompt, byte for byte",
                      " ".join(text["schedules/resume.md"].split()))

    def test_open_items_are_absorbed_before_queued(self):
        def flat(name):
            return " ".join((SKILL / name).read_text(encoding="utf-8").split())
        risk = "auth, secret, data, protocol, storage, dependency, workflow or permission risk"
        skill = flat("SKILL.md") + " " + flat("references/commands/feature.md")
        for phrase in ("Fix open items in the job that found them", "Rounds left: <n>",
                       "Items under `### Absorbed` are done", risk,
                       "or raises the round cap (F3)",
                       "or 0 when the approver already returned CHANGES this cycle"):
            self.assertIn(phrase, skill)
        builder = flat("agents/factory-builder.md")
        for phrase in ("Absorb small open items", "### Absorbed", "### Left open", risk):
            self.assertIn(phrase, builder)
        for name in ("code", "security", "ui", "ux"):
            review = flat(f"agents/factory-review-{name}.md")
            for phrase in ("### Fix now", "### Follow-up", "Nits never change the verdict", risk):
                self.assertIn(phrase, review, name)
            self.assertNotIn("a separate heading", review, name)
            self.assertIn("goes under `### Fix now` or is dropped", review, name)
        self.assertIn("Drop cosmetic nits", skill)
        self.assertIn("Cosmetic nits never become follow-ups", skill)
        approver = flat("agents/factory-approver.md")
        self.assertIn("which it drops", approver)
        self.assertIn("Rounds left", approver)
        self.assertIn("the `### Fix now` nits are worth one more review round", approver)
        self.assertNotIn("absorb bar", approver)
        self.assertIn("rather than Sub-jobs", flat("agents/factory-spec-writer.md"))

    def test_checks_are_sized_to_the_change(self):
        builder = " ".join((SKILL / "agents/factory-builder.md").read_text(encoding="utf-8").split())
        babysit = " ".join((SKILL / "references/commands/babysit.md").read_text(encoding="utf-8").split())
        for text in (builder, babysit):
            for phrase in ("| copy |", "| test-only |", "| one-file logic |", "| cross-cutting |",
                           "When two rows fit, the lower one wins.", "never another"):
                self.assertIn(phrase, text)
        self.assertIn("Change size: <class>; checks: <commands>", builder)
        for phrase in ("check plan: <class>: <checks>", 'git -C "$WT" push --no-verify',
                       "Babysit never runs spec, build or review rounds.", "| `wait-base` |",
                       "rerun once per session", 'gh run rerun <run-id> --failed -R "$REPO"',
                       "Never merge while `stacked` is true."):
            self.assertIn(phrase, babysit)
