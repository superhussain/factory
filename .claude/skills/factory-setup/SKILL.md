---
name: factory-setup
description: Use when a person asks to set up, install, deploy or configure their own factory from this checkout. Walks them through deploy, device install, project registration, hosts, pipeline and next steps with one question at a time.
---

# Factory setup

Walk a person from nothing to a first job, one question at a time. The written version of this path is [getting-started.md](../../../docs/getting-started.md). Link it instead of copying its detail.

## Rules

- Ask with the runtime's question tool: `AskUserQuestion` in Claude Code, `question` in OpenCode. With neither, ask one numbered question per message and wait.
- One decision per question, 2 to 4 options, the recommended option first with ` (Recommended)` in its label. Free text (account ID, zone, emails, paths) is a plain message, one value at a time.
- Validate each answer before moving on (account ID is 32 hex characters, hostname ends in `.<zone>`, path exists and is a git checkout).
- Never ask for, accept, print or store a secret: the Cloudflare API token, `FACTORY_ACCESS_CLIENT_*`, pairing codes. Commands that take one run in the person's own terminal. If a secret lands in chat, do not repeat it; tell them to revoke it in the Cloudflare dashboard and create a new one.
- Ask before installing any tool, editing any file outside this checkout and the factory home, or running `--replace`. Never run `bootstrap.py down`.
- Do not create timers, edit agent settings or save pipeline settings yourself. Give the exact block or the exact dashboard controls and wait for "done".
- Run read-only checks yourself and show their result. Use synthetic examples in explanations.
- End with the summary table in phase 11.

In the phases below, `$FACTORY` is `<skills dir>/factory/runtime/factory.py` once phase 4 has chosen the skills folder.

## 1. Find where you are

- Check: `git rev-parse --show-toplevel`, `test -f deploy/profile.json`, `test -f deploy/bootstrap.json`, `python3 skills/factory/runtime/factory.py doctor`.
- Ask, Goal: "What should we set up?" Options: New instance (Recommended when no profile), Add this computer to my instance, Register a project, Configure next steps.
- Then jump to phase 2, 4, 6 or 8.

## 2. Tools

- Check: `mise --version`, `python3 --version`, `git --version`, `gh auth status`.
- Ask, Install: "`<tool>` is missing. Install it now?" Options: Show me the command (Recommended), I installed it, Skip.
- Then, after a yes, run `mise install` and `mise exec -- pnpm install --frozen-lockfile`.

## 3. Cloudflare

- Check: none.
- Ask, Zone: "Is your domain's zone active in this Cloudflare account?" Options: Yes, No.
- Ask as plain messages, one value each: account ID, zone.
- Ask, Hostname: "Use factory.<zone>?" Options: factory.<zone> (Recommended), Other.
- Ask as a plain message: allowed emails.
- Ask, Plan: "Create these resources?" Options: Yes (Recommended), Change an answer, Stop.
- Then write `deploy/bootstrap.json` as `{"version":1,"account_id":...,"zone":...,"hostname":...,"emails":[...]}`, run `python3 tools/bootstrap.py plan --answers deploy/bootstrap.json` and show it.
- Show the token permission list from [cloudflare.md](../../../docs/cloudflare.md#guided-setup).
- Hand over `mise exec -- python3 tools/bootstrap.py up` for their own terminal. They press Enter four times and paste the token at the hidden prompt. Wait for "done".
- On the Zero Trust message, send them to open Zero Trust once in the dashboard, then rerun.

## 4. Skill and roles

- Check: `ls` the chosen destination.
- Ask, Runtime: "Which agent runs factory work here?" Options: Claude Code (Recommended), OpenCode, Both, Other.
- Ask, Skills: "Install the skill to which folder?" Options: `$HOME/.claude/skills` (Recommended), `$HOME/.agents/skills`, Other.
- Then run `python3 tools/install.py --destination <dir>`.
- On a differing install, ask Replace: "Replace the installed copy? A backup is kept." Options: Replace (Recommended), Keep.
- Then run `python3 "$FACTORY" init --agents <choice>`.

## 5. Check

- Check: `python3 "$FACTORY" doctor`.
- Ask: none while every check passes.
- On a fail, name the check and its section in [troubleshooting.md](../../../docs/troubleshooting.md), then stop at that fix.

## 6. Project

- Check: `git -C <path> remote get-url origin`.
- Ask, Path: free text checkout path.
- Then run `python3 "$FACTORY" init <path>` and `python3 "$FACTORY" pull <slug>`. Read the repo and draft all 12 sections in `<home>/projects/<slug>.md`. Ask one question per section you cannot infer (test command, dev ports, deploy on merge, UI paths).
- Ask, Draft: "I drafted the project file from the repo. Save it?" Options: Save (Recommended), Edit a section, Skip.
- Then show the draft and run `python3 "$FACTORY" push <slug>`. Exit 6 follows the rule in [troubleshooting.md](../../../docs/troubleshooting.md#exit-6-sync-conflicts).

## 7. Hosts

- Check: `python3 "$FACTORY" where <slug>`.
- Ask, Hosts: "Will another computer run factory work?" Options: No, just this one (Recommended), Yes.
- On yes, list every host first. For each host, give Settings, Hosts, Add host, then `python3 "$FACTORY" host pair <id> <code>` for that host's own terminal, then Confirm. Say all hosts pair in one sitting. See [device.md](../../../docs/device.md#host-registration).

## 8. Pipeline

- Check: none.
- Ask, Merge: "How should approved PRs merge?" Options: Merge after green checks (Recommended), Hold every PR for a person.
- Ask, Rounds: "How many review rounds?" Options: 2 (Recommended), 1, 3.
- Ask, Spec gate: "Approve each spec before the build?" Options: No (Recommended), Yes.
- Ask, Pickup: "Pick up ideas automatically?" Options: Not yet (Recommended), Yes.
- Ask, Triage: "Let a model triage the backlog?" Options: Not yet (Recommended), Yes.
- Ask, Archive: "Archive finished jobs?" Options: Off (Recommended), By age, By count.
- Then print one checklist of the Settings, Pipeline controls to set, and wait for "done". See [autonomy.md](../../../docs/autonomy.md).

## 9. Pickup

- Only when pickup is yes. Check: `python3 "$FACTORY" pickup --dry-run` after the files exist.
- Ask, Scheduler: "Which timer does this host use?" Options: cron (Recommended on Linux), launchd (macOS), Task Scheduler (Windows).
- Then, after a yes, write `<home>/routes.md` and `<home>/launchers.json` from [pickup.md](../../../skills/factory/references/pickup.md) and hand over the timer block.

## 10. Extras

- Check: none.
- Ask, Extras (multi-select): "Set up any of these now?" Options: Deploy on merge to main, PR screenshots, Tracer mirror, Notifications.
- Then do each chosen one from its doc section, in that order: [cloudflare.md](../../../docs/cloudflare.md#8-continuous-deployment-from-main), [integrations.md](../../../docs/integrations.md).

## 11. First job and summary

- Check: `gh auth status`.
- Ask, First job: "Start a small first job now?" Options: Yes (Recommended), Later.
- Then suggest `/factory <one small feature>` in the checkout.
- Print a table with columns Step, Status (done, skipped, waiting on you) and Next.
