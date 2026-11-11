# Optional integrations

Core CLI, board, claims, sync and relative job images require no agent daemon,
scheduler, external image host or source checkout. Load the installed SKILL.md
through your runtime. Follow [execution](execution.md) for every dispatch and
handoff; no repository guide is needed.

## Native roles

`<python-launcher> <installed-skill>/runtime/factory.py init --agents none` is the
default. Explicitly select `claude`, `opencode` or `both` instead of `none` to
refresh eight native roles. Init never invokes a runtime binary.

Claude defaults to `Path.home()/.claude/agents`; OpenCode defaults to
`<config_root>/opencode/agents`. Optional absolute `claude_agents_dir` and
`opencode_agents_dir` in device config select custom directories. Links,
junctions, overlapping directories and malformed role delimiters are refused
before any selected role write. All eight sources and all selected destinations
are preflighted together. A later disk failure may leave some bodies refreshed;
resolve the reported destination error and explicitly rerun.

Existing complete native headers remain opaque byte-for-byte, including CRLF,
comments, quoted/multiline/unknown or invalid YAML, model pins and permissions.
Only exact `---` opening/closing lines are validated; intervening `...` is refused.
The native loader owns YAML diagnostics. Fresh Claude headers use source name,
description and tools without a model pin. Fresh OpenCode headers contain only
description and `mode: all`, never copied Claude tools or model fields.

For OpenCode run `opencode api get /api/agent` and manually confirm all eight IDs:
`factory-spec-writer`, `factory-builder`, `factory-explorer`, `factory-review-code`,
`factory-review-security`, `factory-review-ui`, `factory-review-ux`,
`factory-approver`. For Claude confirm these roles through its native agent
interface. Missing or invalid roles stop native dispatch; fix metadata manually
with native diagnostics. Never bypass a permission, model pin or invalid role
using a general worker or auto-approval flags. Existing installer/security
policies remain in force.

## Model routing

Only select available user-configured runtime/model routes. Discover actual IDs
through native tools; respect pins, permissions and restrictions. Report a
missing/conflicting route with the role and configuration needing attention,
then stop. No paid fallback, price table or external model sheet is supplied.
General workers may read canonical role bodies only where the runtime cannot
register named roles or during explicit bootstrap, never to evade restrictions.
Reviewers use fresh context and preferably a configured different model family.
Four reviews, fresh approval and existing budgets remain mandatory.

## Paseo and schedules

Paseo is optional. `PASEO_HOME` selects its local data directory and
`PASEO_AGENT_ID` identifies an existing session. Valid v1 `paseo` board metadata
is retained, and local dead-session detection remains available when configured.
Without it, claims and stale-minute checks still work. The bundle also ships
`runtime/paseo_stalls.py`, the optional Paseo stall adapter that `factory.py pickup`
uses to take over sessions stalled on a Claude usage limit; see references/pickup.md.

`factory.py archive-workspace <slug>/<id>` runs as the last step of a merged or closed job.
It archives the session's Paseo workspace only when the job is at `merged` or `closed`, the
workspace's `isolation` is `worktree`, and its folder is named `factory-<id>` or
`factory-<id>-<n>`. Any other workspace is skipped. Git state never skips the archive.
The JSON lists leftovers first: `uncommitted: <porcelain line>` for each uncommitted file,
`commit: <hash> <subject>` for each commit that no remote and no other local branch holds
(so inherited commits are not listed, such as those a stale local `main` holds or the
history the branch was created from while its reflog keeps the creation entry), and
`git <verb> failed: <reason>` when a git call fails
or runs longer than 20 seconds. It lists at most 20, then `and <n> more`. Ignored
files such as `.context/` scratch are not listed. `--check` runs the guards and prints
`would_archive` with the leftovers, and never archives, so the session can queue a follow-up
first. When that follow-up cannot be queued, the pipeline skips the archive and keeps the
workspace. After a real archive, `archive-workspace` also archives the session's agent when
the workspace archive left it running.
Without `PASEO_AGENT_ID`, a Paseo server id or `paseo` on PATH, the command prints a skip and
exits 0.

Cleanup rule: on a merged or closed job, the pipeline runs
`factory.py archive-workspace <slug>/<id>` and `factory.py cleanup <slug>/<id>`
without asking. They remove only worktrees this pipeline created: a `factory-<id>`
Paseo workspace, or the job's worktree under the factory home. `archive-workspace` archives
a merged or closed job's own workspace whatever its git state and reports leftovers
first. `cleanup` refuses a dirty worktree and keeps a branch that is not merged. Neither
forces. A host owner who runs Claude Code in auto mode can allow them with this sample
entry under `autoMode.allow` in their settings:

```json
"Run factory.py archive-workspace <slug>/<id> or factory.py cleanup <slug>/<id> on a merged or closed factory job. These remove only worktrees the factory pipeline created (a factory-<id> Paseo workspace or the job's worktree under the factory home). archive-workspace archives a merged or closed job's own factory-<id> workspace whatever its git state and reports leftovers first, cleanup refuses a dirty worktree and keeps unmerged branches, and neither forces."
```

`runtime/pr_screenshots.py` uploads PR screenshots to the configured R2 bucket:
`python3 <installed-skill>/runtime/pr_screenshots.py --home <factory home> [--project <slug>] <label> <file>...`.
It reads `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `PR_SCREENSHOTS_BUCKET` and
`PR_SCREENSHOTS_ORIGIN` from `<home>/.env` and prints one URL on the configured public
origin per file. Use it only when the project's PRs section names it;
a project's own screenshot rule always wins.

Schedule files in `schedules/` are optional prompt templates. Discover configured
native scheduler APIs and existing schedule IDs first. Explicitly materialize
`<installed-skill>`, `<python-launcher>` (python3 or py -3), and an available
configured dispatch route. Unresolved values stop before claiming work. Create
only when no matching schedule exists. Refresh using the existing schedule ID,
not a duplicate. Review the resulting stored prompt and enable only explicitly.
Each prompt claims once, never retries and dispatches at most two sessions with
the printed tags. Init, install and adoption never create, update or enable them.
Reinstalling the skill does not refresh stored scheduler prompts or deploy the
Worker. Disabled schedules can remain disabled.
Idea pickup is `factory.py pickup`, a model-free host timer with no stored prompt; see [pickup](pickup.md).

## Links, images and evidence

Agent app/web links and external image origins belong to hosted deployment
configuration, not device defaults. Relative job images and immutable idea-image
imports need no public publisher. Private build-tab evidence is the default for
reviews and PRs. A project may opt into its own image publisher; absence never
blocks ordinary review evidence. Never publish secrets or user data.

## Tracer mirror

An optional, one-way mirror shows a job's tasks, progress and review outcomes in a Tracer instance. It is off until `<home>/tracer.json` lists the project. Delete the entry to turn it off.

```json
{"version": 1, "projects": {"sample": {"url": "http://127.0.0.1:8000",
  "token_file": "/abs/path/to/tracer-token", "service": "sample", "language": "Python"}}}
```

| Key | Rule |
| --- | --- |
| `url` | Required. `https`, or `http` only for `localhost` or `127.0.0.1`. No credentials, query or fragment. A path prefix is allowed |
| `token_file` | Required absolute path to the file Tracer wrote, a regular file holding one token. Read on every call, sent only as `Authorization: Bearer` to `url` |
| `service` | Required Tracer service name, `[a-z0-9][a-z0-9._-]{0,39}` |
| `language` | Optional, at most 40 characters |

### Where the token file is

Tracer writes its token to `TRACER_DATA/token`, and only when it starts with `TRACER_DATA` set, for example `TRACER_DATA=/abs/path/tracer-data uv run uvicorn app.main:app`. Point `token_file` at that file. On Daisy the path is `~/persist/tracer-data/token`, written out as an absolute path in the config.

### First run

1. Set the board `jira` field before the first mirror, because it picks the ticket and the feature id: `echo '{"jira":"PLAN-1234"}' | python3 "$RUNTIME" board "$FACTORY" <job-id>`. Setting it later creates a second feature.
2. Run `factory.py tracer sync <slug>/<id>` once. A good run prints one line, `<slug>/<id>: PLAN-1234 as Tracer feature plan-1234: stage build, 3 tasks, 0 review rounds posted`.
3. Open Tracer and find the feature under the lower-cased key (`plan-1234`). Its tasks `t1` to `tN` and the `factory-builder` agent appear in the Build view.
4. A mirror that stopped partway is fixed by running the same command again. Every write is idempotent.

`factory.py tracer sync [<project-slug>[/<job-id>]]` mirrors on demand: every configured project, one project, or one job. A bulk run that finds nothing to mirror prints one `nothing to mirror` line. A bulk run skips `idea`, `closed` and `merged` jobs and jobs with no `spec.md`. A board patch that sets `stage` or `rounds` mirrors that job on its own. Every write is idempotent, so `tracer sync` is also the recovery.

| Factory | Tracer |
| --- | --- |
| Board `jira` key | Ticket is the upper-cased key, feature id the lower-cased key. Without `jira`, `FACTORY-<n>` from the job id's digits |
| Board `title`, else the spec heading | Feature title |
| Config `service`, `language` | Feature service |
| Builder | Agent `factory-builder` |
| Spec `## Tasks` item N | Task `tN`, chained by `depends_on`, with the first backticked `- Files:` path |
| Complete `round-N/` (four verdicts) | One test run on the last task, with a deep link per `CHANGES` review |

| Stage | Task status |
| --- | --- |
| `spec` | `not_started` |
| `build`, no `round-*` folder | Tasks already `done` stay `done`, the first other task `in_progress`, the rest `not_started` |
| `build`, with a `round-*` folder | `in_progress` |
| `review`, `approve` | `verifying` |
| `needs-human` | `blocked` |
| `pr-open` | `blocked` when `escalated`, else `verifying` |
| `merged` | `done` |

`idea` and `closed` jobs are not mirrored.

A Tracer outage never fails a board patch. The change is saved, `board` prints the same job and exits 0, and stderr gets one warning line. It names the step that failed: `mirror`, `gather`, `scope poll` or `build stop`. A failed mirror names `factory.py tracer sync <slug>/<id>` to retry. The other steps say the next pickup retries, and a device with no scheduled pickup can run `factory.py pickup` itself. Each request times out after 5 seconds, and the first failure stops the mirror. `tracer sync` exits 2 on bad config or usage, 4 when the named job is not on the board, and 5 on a Tracer failure.

Tracer locks task assignment until scope is approved in Tracer. The mirror treats that refusal as normal: tasks still sync, and the summary says `tasks unassigned until scope is approved in Tracer`. The mirror also leaves task assignment alone while a Tracer build agent other than `factory-builder` holds the feature, by the rule under Building in Tracer. Tasks still sync, and the summary says `tasks left with the Tracer build agent`. While that agent holds the feature, the mirror also keeps each task's current Tracer status, and a task Tracer does not have yet gets the status from the table above.

### Follow in Tracer

The job drawer's **Follow in Tracer** button puts a job into Tracer from the dashboard. It shows for a job with a project at `idea` or a mirrored stage, and it sets the board field `tracer` to `{"follow": true}`. A board patch can do the same: `echo '{"tracer":{"follow":true}}' | python3 "$RUNTIME" board "$FACTORY" <job-id>` mirrors at once on a device with `tracer.json`. `board` refuses any other `tracer` value except `null`, `{"follow": null}` and the gather request below, exit 2.

The hosted dashboard never calls Tracer. The next `factory.py pickup` run (not `--dry-run`) on a device with `tracer.json` mirrors each followed job that has no link yet, pulling its `spec.md` first when the device lacks it. After any mirror, from that pass, a board patch or `tracer sync`, factory.py saves `tracer.feature` and `tracer.url` (`<url>/#/f/<feature>`) on the board. The drawer then shows a **Tracer** link to that page. Until then it shows `Tracer: waiting for a device with tracer.json`. Past `spec`, the feature is created without `start_gather`, and its reading says approving scope in Tracer starts a second build, so leave scope unapproved there.
### Building in Tracer

When Tracer's own build agent builds a job's feature, the factory must not build it too. Each `factory.py pickup` run (not `--dry-run`) on a device with `tracer.json` asks Tracer for the feature of each job at `idea` or an in-flight stage whose project is configured. The feature counts as building when it is at stage `build`, not closed, on the job's ticket, and some agent other than `factory-builder` is at stage `build` with kind `session`. Pickup asks only for the feature id the job maps to, the Jira key or else `factory-<n>`. It skips a job whose board `tracer.feature` names another feature, and a feature on another ticket fails the check with the warning below. The warning names the job and Tracer's ticket, with control characters removed, or `no ticket` when the feature has none. Pickup then saves `tracer.building: true` on the board. `board` refuses to set it.

A job with that mark is skipped: pickup, `ideas` and stall takeover all read the board flag only and make no Tracer call, so `--dry-run` prints `skip <id> <project>: building in Tracer`. The dashboard card shows `Building in Tracer`. The next pickup run removes the mark when the Tracer build ends or the feature closes. It also removes the mark, with no Tracer call, from a job that has left `idea` and the in-flight stages or whose project is no longer configured in `tracer.json`. If Tracer is unreachable, pickup prints one stderr warning, `Tracer build check failed`, and unchecked jobs keep their mark. An existing claim is never released.

### Tracer task statuses

Once a job's feature counts as building, the same pickup check saves the feature's tasks as `tracer.tasks` on the board: up to 30 `{id, title, status}` rows in Tracer's order. A task without a valid id or status is skipped, and a title loses its bidi control characters and is cut to 80 characters, or replaced by the id when blank. While the job stays at `idea` or in flight, each pickup run refreshes the list even after the Tracer build ends, so the final `done` states land. A 404 for the feature removes `tracer.tasks`, an unchanged list writes nothing, and a Tracer failure keeps the old list. `board` refuses to set it.

The job drawer shows a collapsed `Tracer tasks: 1 of 3 done` line under the header that opens to one row per task, such as `t2 Write CSV encoder: in progress`. An empty list reads `Tracer tasks: none yet`. The line is hidden for jobs without `tracer.tasks`, for backlog items and in compact mode.

### Gather in Tracer

Following a job at `idea` or `spec` asks Tracer to gather scope before the factory builds. The drawer then writes `{"follow": true, "gather": true}`. A board patch can do the same: `echo '{"tracer":{"follow":true,"gather":true}}' | python3 "$RUNTIME" board "$FACTORY" <job-id>`. `board` refuses `"gather": false` and any `scope`, exit 2. Only factory.py sets `tracer.scope`.

| `tracer` key | Writer | Example |
| --- | --- | --- |
| `follow` | dashboard or patch | `true` |
| `gather` | dashboard at `idea` or `spec`, or patch | `true` |
| `feature`, `url` | factory.py | `factory-42`, `<url>/#/f/factory-42` |
| `scope` | factory.py only | `gathering`, then `approved` or `declined` |
| `stopped` | factory.py only | `true` |

On a device with `tracer.json`, the board patch or the next `factory.py pickup` creates the feature with `start_gather: true`. No `spec.md` is needed. The ticket is the Jira key or `FACTORY-<n>`. The feature text is the board `idea`. The reading asks you to approve or decline scope in Tracer and says the factory stops any Tracer build that approval starts. factory.py records `tracer.scope`:

- `gathering` until Tracer decides.
- `approved` once Tracer sets `scope_approved_at`.
- `declined` once the feature is `closed`.

Approval also queues a Tracer build when a companion listens. Right after it records `approved`, factory.py sends `POST api/features/<feature>/build-runs/stop` once and saves `tracer.stopped: true`. While the companion is still starting the run, the next pickup retries. A board patch that finds a stop still due runs it first and then mirrors that patch's stage. A stopped session holds the job as Building in Tracer until it ends. Builds a person starts later in Tracer are left alone.

While the scope is `gathering`, each pickup sends one GET for the feature. The task mirror is skipped, and `tracer sync` prints `skipped: Tracer is gathering scope`. The drawer shows `Tracer: gathering scope`, then `Tracer: scope approved` or `Tracer: scope declined`.

The build waits on Tracer only when the project's pipeline has the `spec-approval` gate. A job that reaches the gate while the scope is `gathering` stops at `needs-human` with note `gate: tracer-scope`. When Tracer approves, factory.py moves the job to `build` with note `spec approved in Tracer`, and pickup launches `/factory recover <job-id>`, which starts the build. When Tracer declines, factory.py moves the note to `gate: spec-approval` and sets the declined `needs` line, and `/factory approve-spec <job-id>` builds anyway. Only a person passes the gate, by approving scope in Tracer or by `/factory approve-spec`. With the gate off, gathering is advisory and the build starts as usual. Without a Tracer companion listening, `start_gather` queues nothing, so the hold lasts until a person decides in Tracer. A job whose scope Tracer already approved skips the hold: factory.py moves it to `build` with that note when it reaches the gate.
