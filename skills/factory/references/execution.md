# Execution and handoff

## Dispatch roles

Use the same nine role contracts in `agents/factory-*.md` on every runtime.
Claude's source `tools` apply only to Claude Code. Source roles have no model pin. The Markdown
body defines the work and output file for other agents.

| Runtime | Dispatch |
| --- | --- |
| Claude Code | Explicitly install with `init --agents claude`, confirm all nine roles through the native agent interface, then use native agent/message tools. No activation CLI is assumed. |
| OpenCode V2 | Explicitly install with `init --agents opencode`. Use the native subagent tool with the role name, background execution, and configured model. A rerun keeps each existing role's frontmatter verbatim and refreshes only its prompt body. Init validates only the `---` header delimiters, never the frontmatter content. Before using these native roles, run `opencode api get /api/agent` and manually confirm all nine `factory-*` role IDs are registered; a missing or invalid installed role stops native-role dispatch until its frontmatter is fixed manually with the native diagnostics. |
| Other agents | Launch a general worker and have it read the canonical role file's body, the project file, and job files. Pass absolute paths and the base branch. |
| Paseo (optional) | Discover configured profiles/providers with native tools and create a worker in the existing job worktree, not a second branch. Resolve that host's checkout and paths first. See integrations.md. |

Before each role dispatch, run `python3 "$RUNTIME" agent record "$SLUG/<id>" <role> <harness> <via> <model> [<effort>] --session <tag>`.
After hold, record the orchestrator once when no orchestrator row carries this session.
`<harness>` is the route provider or the runtime name. Leave out `<effort>` when the
dispatch set none. `<via>` names the launch path:

| via | Launch path |
| --- | --- |
| `native` | the harness's own subagent tool |
| `paseo` | a Paseo agent |
| `launcher` | a pickup launcher other than Paseo |
| `general` | the general-worker fallback |
| `direct` | a session a person started |

The orchestrator alone holds the factory claim and writes the board. Role workers
write only their assigned files and the builder's branch. Never give them the
orchestrator's factory session tag. Choose each builder per SKILL.md's builder
selection rule before every dispatch. Resume a reachable builder through the
native prompt tool only when the actual model need not change. A model change
stops the old worker, then launches a fresh builder against the same files.

### Routing sources

- Start with the project file's Orchestrator section, subject to any configured
  restriction. Then resolve each role through the runtime's active routing configuration
  or Paseo profile and its native agent definitions. The factory keeps no model
  table of its own and copies no provider IDs or prices.
- Use only user-configured routes available in the active runtime. The factory
  selects no paid fallback and requires no external skill, model sheet or checkout.
  Discover actual IDs through native tools and honor native pins and permissions.
- A runtime-specific restriction beats a stale shared route and a skill default.
  Read the sources at dispatch time and discover available IDs with the runtime's
  own tools. Report an absent, conflicting, or unavailable mapping. Never guess an
  ID, edit configuration, or silently pick a paid fallback.
- Prefer native delegation to the `factory-*` roles after the activation check.
  Respect installed OpenCode model pins: do not rewrite them or pretend a prompt
  changes a role's model. If a pin blocks the selection SKILL.md requires, report
  the conflict and stop at `needs-human`. Do not route around it with a general
  worker. Do not shell out to a headless runtime as a routine dispatch path.
- Reviewers always get fresh context, never the builder's session. Prefer a
  configured model family different from the builder's. An inherited model is a
  configured route only when the user's policy permits it, not evidence of
  independent review. Select another configured family or report the limitation.

The general-worker fallback applies only when a runtime cannot register named
roles, or during an explicit bootstrap before native role installation. Then pass
the canonical role file to an available general worker instead of skipping that
role. Never use it to bypass a missing or invalid installed role, a permission
denial, a model pin, or configured role restrictions. Four reviewer files and
their first line verdicts remain mandatory. A failed worker gets one retry, then
needs-human. Do not run a failing provider repeatedly or silently switch to a paid
fallback.

The old orchestrator owns new-role dispatch until a whole-job handoff releases its
claim. A delegated builder or reviewer is not a whole-job handoff.

## Handoff

Hand off only at a quiescent checkpoint in spec, build, review, or approve.

1. Stop or wait for every worker and dev server. Confirm no old agent can still
   write to the branch or job files. If the outgoing session is unreachable,
   inspect its Paseo status and pending workers. A rate-limit message alone does
   not prove it stopped. A live claim still needs explicit user-authorized takeover.
2. Commit the current code on `factory/<id>` and push it without force. Even a
   spec-stage handoff needs the branch on origin. If the work is incomplete, record
   that fact in the checkpoint and commit it without claiming verification passed.
3. Write or update `<job folder>/handoff.md`. Include the source runtime/model and
   Paseo link when known, reason for transfer, current stage/round and branch SHA,
   completed work, checks and actual results, unfinished files/findings, next exact
   action, review-cycle counters, and any human hold or merge restrictions. Append
   a dated section for each transfer so earlier handoffs remain visible. No code,
   credentials, or machine-local secrets belong in this file.
4. Run `python3 "$RUNTIME" handoff "$SLUG/<id>" --session <outgoing-tag>`.
   It verifies current ownership, an active stage, a nonempty checkpoint, real
   job paths (no symlinks or junctions), a clean worktree, and a matching
   published branch. It uploads job files before atomically releasing the claim.
   Failure keeps the claim; inspect the error rather than taking or forcing
   anything. The command does not stop workers for you.
5. Only after success, send the receiver `/factory resume <id>` and the
   dashboard link. In Paseo, create or prompt the desired runtime/model on the
   destination host with the discovered profile settings. Do not send the old
   factory session tag; the receiver must obtain its own. The outgoing agent must
   now stop work on this job. If dispatch fails, leave the published handoff ready.

If Claude is rate-limited and cannot checkpoint, a human can authorize a new agent
to take over with `hold <project>/<id> take` after old workers are confirmed stopped.
Read the old session and inspect uncommitted work before recovery. Preserve edits;
never reset the branch. Normal resume does not use `take`.

## Resume

Run `job <id>` first and check the stage before claiming. If it is a human or PR
gate (`needs-human`, `pr-open`, `merged`, `closed`), report the hold and stop; do
not claim or approve it. Only for an in-flight stage (`spec`, `build`, `review`,
`approve`) run `where <project>`, `hold <project>/<id> released --session <new-tag>`,
then `worktree <project>/<id>`. Use the tag printed by hold on subsequent calls.
The `released` mode refuses another owner's claim even when stale, and refuses
human and PR gates outright. It also compares a ready handoff packet's recorded
stage and round against the live board and refuses a mismatch before claiming,
so check `job <id>` for both before and after the hold. Ordinary hold retains
its existing stale-claim recovery behavior. Before reading the manifest or any
artifact, any hold that pulls refuses a job whose paths include symlinks or
Windows junctions, or a directory it cannot fully enumerate: a link at the jobs
root, the project folder, the job folder, a round directory, an artifact file,
or the `.sync.json`/`.remote` metadata — or an unreadable job path — exits 6,
naming the link or the path it could not inspect. Links are never followed,
replaced, or deleted, so their targets and your local edits stay untouched;
inspect and remove the link or fix the permissions yourself before retrying.
After the released pull, hold re-verifies every local markdown and
image against the sync manifest, R2's files, and their current ETags. A
local-only file, a local edit, a symlink or junction, or a file missing from or
newer in R2 exits 6, naming the offender. The claim is not rolled back: it
may already be held by this session. Reconcile the named artifacts locally
(merge or delete, then push), or stop at needs-human. Do not dispatch any work
while artifacts are out of sync; hold never uploads or deletes for you, and it
keeps your local edits.
Resolve all paths locally; a previous device's absolute paths are not portable.
Hold records acceptance of a ready handoff and stamps the new Paseo link. The
board note is never rewritten, so any human restriction on it stays visible.

Read the board, project, checkpoint, spec, build log, all existing reviews, decision,
and latest rework. Verify actual git status, branch head, PR state, and recorded
checks before dispatch. A checkpoint is context, not authority to bypass a gate.
Compare the handoff packet's recorded stage, round, and HEAD against the current
board and restored branch; any mismatch means the packet no longer describes this
job. Explain it and stop at needs-human rather than trusting stale verdicts.

| Stage | Continue |
| --- | --- |
| spec | Finish or verify spec.md, then continue at loop step 3, which applies the spec-approval gate. |
| build | Finish the current build/fix using the latest review or rework. Preserve inherited edits. Reassess the builder tier per SKILL.md. Resume the old builder only if the model need not change; otherwise dispatch a fresh one. |
| review | Validate completed reviewer files. Dispatch only missing or malformed reviews. Wait for all four before acting. |
| approve | Verify the passing round, then dispatch a fresh approver against the current HEAD and round. A decision.md left by the previous session is context only; never ship on it. Approver and retry budgets carry over unchanged. |
| needs-human | Report the hold. Do not interpret resume as approve. |
| pr-open | Check the PR and existing merge restrictions. Previously held or escalated PRs stay held. |
| merged or closed | Do not reopen. Further work needs a new referenced job. |

A runtime switch never resets round numbers, fix-cycle caps, approver counts, or
human restrictions. If files cannot establish the counters, stop at needs-human.
Push changed artifacts and patch the board at every transition as usual.

## Limits and scheduling

New factory work and resumed work use the same dashboard. `next` consumes the
existing shared backlog on any supported runtime. Invoke the skill through the
runtime's skill interface or provide its installed SKILL.md path when slash
commands are unavailable. `opencode run` can submit that instruction headlessly.

Changing runtimes does not create, modify, or enable Paseo schedules. Existing
Claude schedules stay as configured. A scheduler may prompt an existing session
to resume, or start a new session for a released checkpoint. It must not take a
live claim merely because the provider is rate-limited. Unavailable tools/models
are reported, not replaced with guessed commands or skipped review gates.
