# Integrations

All integrations are optional. The complete installed-only guide is
[skills/factory/references/integrations.md](../skills/factory/references/integrations.md);
dispatch, handoff and resume tables are in the adjacent execution.md. Users of a
copied bundle read those installed references without opening this repository.

## Native roles and activation

Explicit `init --agents claude|opencode|both` installs only selected native
destinations; default is `none`. No runtime binary is needed for installation.
Optional absolute device config fields `claude_agents_dir` and
`opencode_agents_dir` override native defaults. All nine sources and selected
destinations are preflighted before any role write. Later disk failures can leave
some bodies refreshed; they report the destination and stop, not a false
multi-file-transaction success.

Existing complete headers are opaque and byte-preserved, including CRLF,
comments, quotes, multiline/unknown/invalid YAML, model pins and permissions.
Only exact opening/closing `---` lines are validated; intervening `...` is refused.
Fresh Claude roles preserve source name/description/tools, without a model pin;
fresh OpenCode roles get only description and `mode: all`. Native loader
diagnostics own YAML validation. Unrelated roles are never modified.

Run `opencode api get /api/agent` and manually confirm all nine `factory-*` IDs
before native OpenCode dispatch. For Claude confirm installed role availability
through its native agent interface; no activation CLI is assumed. Missing or
invalid roles stop dispatch. Never bypass model pins, permissions, interactive
installation policies or invalid metadata with general workers or auto-approval.

## Configured model selection

Discover available IDs with the active runtime's tools. Use only user-configured
available routes, never a selected paid fallback, copied price table or private
model sheet. Honor native pins and restrictions. Report the role and missing or
conflicting configuration and stop when it cannot dispatch. General workers may
read canonical role bodies only when a runtime cannot register named roles or
during explicit bootstrap, not to evade restrictions. Four independent fresh
reviews and a fresh approver remain required; a runtime change resets no budgets.

## Paseo and explicit schedules

Optional `PASEO_HOME`, `PASEO_AGENT_ID` and valid v1 `paseo` board metadata remain
supported. Local dead-session detection is optional; core operations need no
daemon or executable. Agent app/web links are hosted deployment configuration,
disabled unless configured, not device defaults.

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
workspace. Without `--check`, `archive-workspace` also archives the session's agent when
the workspace archive left it running.
Without `PASEO_AGENT_ID`, a Paseo server id or `paseo` on PATH, the command prints a skip and
exits 0.

Cleanup rule: on a merged or closed job, the pipeline runs
`factory.py archive-workspace <slug>/<id>` and `factory.py cleanup <slug>/<id>`
without asking. They remove only worktrees this pipeline created: a `factory-<id>`
Paseo workspace, or the job's worktree under the factory home, and that job's private prompt folder under
`prompts/<slug>/<id>`, refusing a link. `archive-workspace` archives
a merged or closed job's own workspace whatever its git state and reports leftovers
first. `cleanup` refuses a dirty worktree and keeps a branch that is not merged. Neither
forces. A host owner who runs Claude Code in auto mode can allow them with this sample
entry under `autoMode.allow` in their settings:

```json
"Run factory.py archive-workspace <slug>/<id> or factory.py cleanup <slug>/<id> on a merged or closed factory job. These remove only worktrees the factory pipeline created (a factory-<id> Paseo workspace or the job's worktree under the factory home, and that job's private prompt folder under prompts/<slug>/<id>, refusing a link). archive-workspace archives a merged or closed job's own factory-<id> workspace whatever its git state and reports leftovers first, cleanup refuses a dirty worktree and keeps unmerged branches, and neither forces."
```

`schedules/resume.md`, `schedules/explore.md` and `schedules/retro.md` are prompt templates, not executables. Ideas are picked up by `factory.py pickup` instead (see Automatic pickup below and `references/pickup.md` in the installed skill).
Using the scheduler's native tools, discover existing IDs and configured routes.
Materialize `<installed-skill>`, `<python-launcher>` and the available dispatch
route before explicitly creating or refreshing a stored prompt. Unresolved
values stop before claiming. Refresh using the existing schedule ID, never a
duplicate. Inspect the stored prompt, then enable only explicitly. Each prompt
claims once, never retries and dispatches at most two sessions using printed tags;
preserve `needs-human` summaries. Pass `--project <slug>` to claim only that
project's ideas; with it, the `--max-inflight N` cap counts and applies only to
that project's jobs. The cap counts fresh in-flight jobs (spec/build/review/approve,
and unescalated open PRs) and freshly claimed ideas — scoped to the selected
project with `--project`, across all projects without it; closed jobs,
unclaimed ideas and jobs waiting on a human never count. The slug
must be a registered project with an existing local checkout (see paths.md); the
run exits 3 without claiming once the cap is reached. Init/install/adoption never
manage schedules.
Reinstalling the skill does not deploy the Worker or refresh stored scheduler prompts.

### Dispatch routes and usage limits

Each host lists its own dispatch routes in `<home>/routes.md`, primary first, one `<name>: <provider> <model> [<effort>]` frontmatter line each, where the optional effort token is passed to launchers that use `{effort}`. Copy the provider and model from your own routing configuration; the repository ships none. `routes pick` prints the first route not marked limited, with `runtime`, its provider's base runtime: the end of the provider's `extends` chain in `$PASEO_HOME/config.json` (default `~/.paseo/config.json`), else the provider itself. `routes limit <name> [--minutes N]` marks one limited for N minutes (default 60, at most 1440) in `<home>/route-limits.json`, `routes clear <name>` lifts the mark, and `routes list` shows every route with its limit. When `routes.md` carries the optional `experiment:` line, `routes list` ends with an experiment row (`experiment`, `provider`, `model`, optional `effort`, `percent`, `limited_until`), `routes limit` and `routes clear` also accept the experiment name, and an arm `b` launch that hits a usage limit prints `limited experiment <name> until <ts>`. Without that line the output is unchanged. Both files stay on their host; never copy them, or any credential, to another host. A host picks ideas only for projects in its own `paths.md`, and the board claim stops two hosts taking one idea. The `--max-inflight` cap counts the whole board, so every host's pickup shares one cap. When no route is available the run claims nothing. `unclaim <id> --session <tag>` returns an idea that a failed dispatch left claimed.

### Automatic pickup

Instead of a stored scheduler prompt, a host can run `python3 <installed-skill>/runtime/factory.py pickup` on a timer every 5 minutes. A registered host's `max_inflight`, set in Settings, Hosts, caps its in-flight claims across projects. A `max_inflight` of 0 pauses pickup on that host, stall takeover included; jobs already in flight and deny-notes launches keep running. The lower of it and `--max-inflight` wins. Pickup is model-free, silent when there is nothing to do, claims one idea at a time through the board compare-and-swap, and launches each through a host-local `<home>/launchers.json`. Enable it per project or globally with the pipeline `pickup` key, which lists the host names allowed to pick up. The installed reference has the launcher examples, cron, launchd and Task Scheduler setup and the steps for moving off a stored scheduler prompt: see `references/pickup.md` in the installed skill. When `<home>/stalls.json` names an adapter, pickup also takes over a session stalled on a usage limit whose reset is more than `takeover_minutes` (default 30) away, launching the recover flow on the next route. Resets 30 minutes away or closer are left to the host's limit-resume sweep, so the two never act on the same session; the installed reference has the setup. The dashboard shows the same scheduler and launcher setup under Settings, Scheduler.

### Operating schedules

Cadence is the operator's choice: set the cron in the scheduler's own tooling when
creating the stored schedule; no default cadence is shipped. To pause, disable the
stored schedule in the scheduler — disable, do not delete, so the existing ID stays
reusable for the next refresh. Run results are read in the scheduler's run history,
matched to dispatched sessions by the tags printed in each prompt. When a run finds
nothing to do it exits 3 (`no ideas`) without retrying, as specified above.

## Private evidence and images

Private build-tab evidence is the default for reviews and PRs. Relative job images
and immutable idea-image imports work without publication. External image origins
and agent app/web links belong to hosted deployment configuration, not fresh
device config. A project may opt into its own image publisher; absence never
blocks ordinary review/PR evidence. Never publish secrets or user data.

`<installed-skill>/runtime/pr_screenshots.py` uploads PR screenshots on any host with the installed skill:
`python3 <installed-skill>/runtime/pr_screenshots.py --home <factory home> [--project <slug>] <label> <file>...`.
It reads `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `PR_SCREENSHOTS_BUCKET` and
`PR_SCREENSHOTS_ORIGIN` (an `https://` origin) from `<home>/.env`, stores each image in that
bucket as `<project>/<label>/<12 hex>-<name>` (project `other` when `--project` is omitted)
and prints one `<file>` and URL on the configured public origin per line. Any failure exits 1,
and build-tab evidence stays the fallback.

`tools/copy-pr-screenshots.py` copies older screenshots from `<source>/factory-<job-id>/` into
their project folders, using the same four `<home>/.env` keys.
`plan --home <factory home> --source-project <source> --out <plan.tsv>` lists the old keys and
writes a `source`, `destination`, `reason` TSV. A key goes to `<project>/factory-<job-id>/...`
when exactly one `<home>/jobs/<project>/<job-id>/` folder holds a file. `<source>` jobs stay in
place, and no match or several matches go to `other`. Review the plan, then run
`apply --home <factory home> --source-project <source> --plan <plan.tsv>`. It only copies. It
never overwrites an existing key and never deletes, so old links keep working. Each copy is
checked byte for byte against its new URL on the configured public origin, and any `exists-different`, `mismatch` or `failed` row
exits 1. Each request waits up to 300 seconds. A `failed` row gets a fourth column naming the
step and cause, such as `source read: timed out after 300 s`. `plan` skips, and `apply`
rejects, any key with a `.` or `..` segment. Run it from the main checkout, never a job
worktree, so the token only runs merged code.

## Tracer mirror

An optional, one-way mirror shows a job's tasks, progress and review outcomes in a Tracer instance. It is off until Settings, Pipeline, Tracer integration is On and `<home>/tracer.json` lists the project. Delete the entry to turn it off.

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

### Turn Tracer on

Tracer is off until a person opens Settings, Pipeline, picks the all-projects scope, and sets **Tracer integration** to `On`. It saves `"tracer": {"enabled": true}` in the all-projects pipeline document, and a project scope refuses the key. A missing key or `false` means off.

While it is off:

| Reader | Off behavior |
| --- | --- |
| Dashboard | hides Follow in Tracer, the Tracer link, the task list, the scope and waiting lines, and `Building in Tracer` |
| Board patch, follow pass | return before any Tracer call |
| Build pass | treats every project as unconfigured, so it clears `building` marks with no Tracer call |
| `tracer sync` | prints `nothing to mirror: Tracer is off in Settings, Pipeline`, exit 0 |
| Archive pass | a pending Tracer mirror no longer keeps a job on the board |

Board fields and `tracer.json` stay as they are, so setting it back to `On` resumes where it stopped. When a device cannot read the setting, pickup and board patches print their usual Tracer warning and `tracer sync` exits 5. Devices on an older bundle treat a missing key as on and keep mirroring until they upgrade.

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

A Tracer outage never fails a board patch. The change is saved, `board` prints the same job and exits 0, and stderr gets one warning line. Pickup treats a Worker error inside its Tracer passes the same way. A 503 on `PUT /api/board`, for example, prints the error and "Tracer build pass stopped; Building in Tracer marks wait for the next pickup run" (or "Tracer follow pass stopped; followed jobs wait for the next pickup run"), and pickup still claims and launches ideas. The board patch warning line names the step that failed: `mirror`, `gather`, `scope poll` or `build stop`. A failed mirror names `factory.py tracer sync <slug>/<id>` to retry. The other steps say the next pickup retries, and a device with no scheduled pickup can run `factory.py pickup` itself. Each request times out after 5 seconds, and the first failure stops the mirror. `tracer sync` exits 2 on bad config or usage, 4 when the named job is not on the board, and 5 on a Tracer failure.

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
