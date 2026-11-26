# Automatic pickup

`factory pickup` is a model-free command that a host timer runs every 5
minutes. It claims runnable ideas for the projects whose pipeline lists this
host and starts one session per idea through `launchers.json`.

## Agent setup prompt

Paste this prompt into a coding agent running on the host, such as Paseo, Claude Code or OpenCode. The agent drives `pickup setup` until every check is ok. The dashboard's Settings, Scheduler, Agent tab shows the same text; a test keeps the two equal.

```text
Set up automatic factory pickup on this host. You are a coding agent on the machine that should pick up ideas. Work through these steps in order.

1. Find the installed factory skill: the folder that holds SKILL.md and runtime/factory, often ~/.agents/skills/factory or ~/.claude/skills/factory. Below, SKILL means that folder. Run: SKILL/runtime/factory pickup setup On Windows the binary is SKILL\runtime\factory.exe; use that spelling in every command below.
   If there is no such folder, or pickup setup prints a usage error instead of checks, the skill is missing or older than this setup; install or update it with sh tools/install.sh <skills folder> (tools\install.ps1 on Windows) from a clone of the factory repo, then start again.
2. It prints 7 numbered checks, each ok or missing, with a fix under each missing one. Fix checks 1 to 6 in order. After each fix, run pickup setup again.
   - home: run the doctor line it prints. If the factory home or its credentials are missing, stop and tell me. Never print or copy values from the .env file.
   - device: add a device: line inside the frontmatter of config.md in the factory home, with a short name for this host.
   - checkout: run the init line it prints with the path of this host's clone of each project. That adds it to paths.md.
   - routes: write routes.md from providers and models that exist on this host. Find them first, for example with paseo provider ls and paseo provider models <provider>, opencode models, or the model aliases claude --help lists. Never copy route, provider or model names from another person's setup.
   - launchers: write launchers.json with one launcher per route, copied from the matching example in SKILL/references/pickup.md. Keep every argument the example shows, including its permission mode. Never add --dangerously-skip-permissions or any flag that approves every action.
   - pipeline: pickup must be on for the project and list this host exactly as pickup setup prints it. That is a dashboard setting under Settings, Pipeline, or Settings, Projects, then the project. If you cannot change it yourself, tell me exactly what to click and type, and wait until I say it is done.
3. Run SKILL/runtime/factory pickup --dry-run and show me what it would launch.
4. Check 7 is the timer. Show me the command SKILL/runtime/factory pickup setup --install and say that it writes and loads a 5 minute timer for this host. Run it only after I answer yes. Otherwise I run it myself.
5. Run pickup setup again until it prints 7 of 7 ok. Then tell me which program the timer runs and any note it printed.

A stored Paseo or Claude schedule prompt does not pick up ideas, so do not create one. Only the daemon service or the host timer runs pickup.
```

When the installed skill is older than this setup, run
`<installed-skill>/runtime/factory catch-up`, or install with
`tools/install.sh` if no skill exists yet. Catch-up backs up the skill, timer and
home config, installs origin's skill and rewrites the pickup timer through it.
On a host with the daemon service installed, it removes a leftover pickup timer
instead and prints `pickup timer: replaced by the daemon`. It never installs the
daemon itself.

## Set up and check

Run this on each host. It checks every piece pickup needs, in order, and prints
the exact fix for each missing one with this host's own paths:

```
<installed-skill>/runtime/factory pickup setup
```

| # | Check | What it reads |
| --- | --- | --- |
| 1 | `home` | `factory doctor`: config, credentials, projects and board |
| 2 | `device` | `device:` in `config.md`; without it the host name is the hostname |
| 3 | `checkout` | `paths.md`, a checkout that exists on this host |
| 4 | `routes` | `routes.md` |
| 5 | `launchers` | `launchers.json`, a launcher per route |
| 6 | `pipeline` | each project's effective pipeline: pickup on and lists this exact host |
| 7 | `timer` | the daemon service when it is installed (`scheduler: daemon`), else the 5 minute timer in launchd, cron or Task Scheduler |

It exits 0 when all 7 are ok, 3 when any is missing, and 2 on bad arguments.
It writes no factory file. Then install the 5 minute timer for this host's
scheduler. Running it again changes nothing:

```
<installed-skill>/runtime/factory pickup setup --install
```

| Scheduler | File | Load | Already installed when |
| --- | --- | --- | --- |
| launchd | `~/Library/LaunchAgents/factory.pickup.plist` | `launchctl bootout`, then `bootstrap gui/<uid>` | same bytes and `launchctl print` exits 0 |
| cron | user crontab | `crontab -` | the exact line is present, and no other pickup line |
| schtasks | `~/.config/factory-pickup.cmd` | `schtasks /Create ... /F` | always re-created (`/F`) |

The timer runs the installed `factory` binary directly with `--home <home> pickup`, no interpreter. The second line of the output names the timer program. When a timer already exists but runs a different program or skill path than `--install` would write, check 7 stays ok and prints a `note:` line. On cron, `--install` replaces any old line that runs
`factory pickup` and keeps every other line. The timer `PATH` holds the launcher folders, gh's folder when `gh` is on the installing shell's `PATH`, and the system folders. On launchd, a plist whose `PATH` differs from what `--install` would write gets the same `note:` line.

A stored Paseo or Claude schedule prompt does not pick up ideas. Only the daemon service or the host timer runs pickup.

## What it does

Each run, in order:

- Reads the board and lists the `stage: idea` jobs whose project has a checkout
  in `paths.md` on this host.
- Also lists `build` jobs with note `spec approved in Tracer`, which Tracer scope
  approval released, or `spec approved on the dashboard`, which Approve spec released, and launches `/factory recover <id>` for them instead of
  `/factory idea <id>`. They follow the same skips, claim and caps as ideas.
- Skips ideas that are gated, held by a live claim, waiting on a human, or whose
  `not_before` is still in the future (`skip <id> <project>: not before <time>`), or
  that depend on a job still open on the board (`skip <id> <project>: waits on <dep-id>`),
  except a `pr-open` job with `draft` true, and
  projects whose effective pipeline has pickup off, does not list this host, or
  cannot be read. Each skip reason is printed once and the run goes on.
- Holds back batchable ideas: an idea with a `follow-up-of` ref, or one backlog
  triage queued (`backlog.by` is `triage`). An idea a person added or approved
  from the dashboard (it has `capture_id`) or prioritized is never held. A
  project's held ideas wait until it has 5 of them or the oldest has gone 12 hours
  without an update, printing
  `waits for a batch (<n> of <min> held, oldest <h>h of <max>h)`. Then pickup
  launches the first in board order and skips the rest with `batches behind <id>`.
  That session's `batch` step picks them up, since `batch` marks them
  `"hold": true`. When the effective pipeline carries `hold` (`min_ideas` 1 to 20,
  `max_wait_hours` 1 to 168), it replaces 5 and 12. `factory ideas` does not
  hold. A dry run prints a project's pickup off, host or pipeline skip line for
  its held ideas instead of the batch line. Set them in the dashboard under
  Settings, Pipeline, Batch follow-ups. The board shows the same state on each
  held idea's chip: "Waits for a batch (n of min, hh of maxh)", "Batches behind
  <id>", or "Batch ready" on the one pickup launches next.
- Holds back plain ideas while review falls behind. Pickup counts, per project,
  the jobs that wait on you, as the dashboard's Needs you column does:
  `needs-human`, a set `needs`, or an escalated `pr-open` PR, except a `pr-open`
  job with `draft` true. Only the idea's own
  project counts, so one project's review queue never pauses another. At or
  above that project's effective pipeline `backpressure.needs_you` (a whole number from 1 to 50,
  default 5), it skips each idea with
  `skip <id> <project>: backpressure (<n> waiting on you, pauses at <limit>)`.
  Prioritized ideas, approved specs and moved jobs still launch. This line
  replaces a batch line for the same idea. Set it in the dashboard under
  Settings, Pipeline, Review backpressure. The board shows
  "Paused: <n> waiting on you" on each idea card pickup holds this way, in
  place of its batch chip. Until the board has read the project's effective
  pipeline, it shows no Paused chip, and a held idea reads
  "Held for batching". A failed read falls back to the defaults.
- Skips a project whose checkout fails the `unsafe checkout` check, when pickup
  is on and lists this host. It fetches `origin/<base>` (with
  `GIT_TERMINAL_PROMPT=0` and a 60 s limit, so a remote that asks for
  credentials or stops answering is a skip, not a stall) and refuses a local
  `<base>` with a root commit that `origin/<base>` lacks, so a host still on
  archive history claims and launches nothing for that project. `worktree` runs
  the same check on the job branch and exits 6.
- PR sync: right after the move stops and before the close pass, reads the state
  of each `pr-open` or `needs-human` job's pull request from GitHub, one
  `gh api graphql` call per repo, and moves a merged job to `merged` or queues a
  close for a closed one. See [Syncing PR state](#syncing-pr-state).
- Earned autonomy hold: right after the PR sync, sets `automerge` false, `needs`
  and the note `earned autonomy hold: a person reviews and merges` on each
  automerging, non-escalated `pr-open` job whose project and class have a merged
  job reverted since the last `factory autonomy restore` or
  `autonomy promote <slug> <class>`. It
  prints `held <id> <project> PR #<n> for a person`, or on a dry run
  `would hold <id> <project> PR #<n> for a person`. See docs/autonomy.md,
  Earned autonomy hold.
- Deny notes: before ideas, launches at most one `/factory deny-notes <id>` session
  for a `closed` job denied from the backlog whose `backlog.notes` are unread (no
  `backlog.notes_read`). The session gets the notes from `factory deny-notes <id>`
  inside a `backlog-notes` envelope, queues the follow-ups they ask for with
  `backlog add`, then runs `deny-notes <id> done`. Notes that fail admission are
  skipped. This launch does not count against `--limit` or the in-flight cap.
- Resolve: right after deny notes, launches at most one `/factory rework <id> --session <tag>`
  or `/factory approve <id> --session <tag>` session for a `needs-human` job whose
  `resolve` the dashboard's Rework or Approve as is set. It claims the job and changes
  nothing else. The note never enters argv; the session reads it with `factory job`
  and clears `resolve` with null. It skips a job that waits on a human, is paused,
  closing or held by a live claim, or whose `resolve` is malformed. A dry run prints
  `would launch resolve ...` and `skip resolve <id> <project>: <reason>`. A failed
  launch releases the claim. This launch does not count against `--limit` or the
  in-flight cap.
- Babysit: right after the priority nudges, launches at most one `/factory babysit <id>`
  session for a `pr-open` job with `babysit` set. It claims the job, records an agents
  row with role babysit when it launches a new session or sends to the job's agent, and
  changes nothing else. It skips a job that waits on a human, is paused, closing or held
  by a live claim, or whose project pickup does not list this host. A dry run prints `would launch babysit ...` and
  `skip babysit <id> <project>: <reason>`. A failed launch releases the claim. This
  launch and babysat jobs do not count against `--limit` or the in-flight cap. The
  session follows [babysit](commands/babysit.md).
  Every babysit goes to the job's own linked Paseo agent. On this host's Paseo server an
  `idle` or `closed` agent gets the command with `paseo send` (a closed agent resumes): a dry
  run prints `would send babysit <id> <project> to agent <agent>`, and a send prints
  `sent babysit <id> <project> to agent <agent> session <tag>`. A `running` agent skips the
  job this run (`skip babysit <id> <project>: agent <agent> is running`, likewise for any
  other status such as `initializing`). An agent on another
  server skips it (`agent <agent> lives on server <server>`), and so does a failed `paseo ls`
  (`paseo ls failed`). An agent absent from `paseo ls`, or no link, launches a new session.
  A failed send to an `idle` agent prints a stderr line, releases the claim and launches
  nothing. A failed send to a `closed` agent prints a stderr line and launches a new session
  with the same tag. Before it claims the job for an `idle` agent, pickup runs
  `paseo inspect <agent> --json`: a non-empty `PendingPermissions` list skips the job,
  dry run or not (`skip babysit <id> <project>: agent <agent> waits at a permission dialog`),
  and any answer but an empty list skips it with `permission state unknown` at the end. A new babysit session gets workspace slug `babysit-<id>` and title
  `babysit <id>`: pickup rewrites `--worktree-slug` and `--title` in the launcher, and inserts
  `--worktree-slug` after `run` for a `paseo` launcher with `--new-workspace` that lacks it.
  A live holder, such as a `running` agent skipped above, learns of the click from its
  next `hold`, which prints `babysit requested`.
- Auto-babysit: right before the Babysit step, in projects whose project file sets
  `auto_babysit: <1 to 9>`, sets `babysit` on a `pr-open` job whose PR has an open counted
  review thread, a merge conflict or a failing required check, so the Babysit step launches
  it in the same run. It reads every candidate PR in a repo with one `gh api graphql` call,
  only for opted-in projects, at most 20 PRs per repo, the oldest automatic babysit first, and
  prints to stderr how many candidates it checked when a repo has more. It skips a job that
  waits on a human, is paused, closing, escalated, batched into another, already babysat or
  held by a live claim, whose project has no Babysit note, or whose project pickup does not
  list this host. It reads the job's `handled` field as babysit's `pr-state` does: a handled
  review thread is no signal, and neither is a check or merge conflict babysit worked 3 times.
  One board write for the whole pass sets, on each job,
  `babysit = {at, via: "pickup", by, reason}`, `auto_babysit = {count, head, reason, at}` and
  the note, and prints `auto-babysat <id> <project>: PR #<n> <what it has> (<count> of <cap>)`.
  That write re-checks every job, so a job another host or the dashboard changed meanwhile is
  left out and the rest still land. A dry run prints
  `would auto-babysit ...` and `skip auto-babysit <id> <project>: <reason>`. A job gets at
  most `auto_babysit` automatic babysits (`<cap> of <cap> automatic babysits used`), and
  never a second one for the same head commit and reason (`already babysat <reason> at <head>`).
  A person's Babysit PR click does not count. Only pickup sets `auto_babysit`; `board`
  refuses it.
- Picks the first route in `routes.md` that is not marked limited, then resolves
  the launcher for it. A config error exits 2 before anything is claimed.
- Claims one idea at a time — the board compare-and-swap stops two hosts taking
  one idea — launches it detached with its output in a private log, and waits up
  to 30 seconds. Still running or exit 0 counts as launched.
- After each launch succeeds, appends an orchestrator row to the job's `agents`:
  the provider, model and effort it launched with, its session tag, and `via`
  `paseo` when the launcher program is `paseo`, else `launcher`. An arm `b` row
  carries the experiment's triple.
- After a launch fails for a reason other than a usage limit, drops the claim and
  parks the job in one board write: `not_before` 5 minutes out, doubling with each
  failure in a row up to 4 hours, and `launch_failure` with the `count`, a
  path-free `reason` and `at`. It prints `backed off <id> until <time> (launch
  failure <n>)` and exits 1. The next successful launch removes `launch_failure`.
- On the fifth failure in a row, the same write moves the job to `needs-human`
  instead of parking it, with no `not_before`, `launch_failure` kept, and `needs`
  `Pickup launch failed 5 times in a row on host <host>. Last reason: <reason>.
  Fix the launcher, then clear this ask and move the job back to <stage>.` It
  prints `needs-human <id> after 5 launch failures in a row` and exits 1.
- Stops after `--limit` launches (default 1) or when a full pass claims nothing.
  It is silent when there is nothing to do, and it never calls a model.
- Caps in-flight work. On a host with a registry record, the cap is the host's
  `max_inflight` from Settings, Hosts, counted over this host's fresh claims in
  every project. A `max_inflight` of 0 pauses pickup on that host, stall
  takeover included; jobs already in flight and deny-notes launches keep
  running. Jobs waiting on a human never count. `--max-inflight N` lowers it
  further. A host with no record caps each project at `--max-inflight`
  (default 1), as before.
- On a host paired with `factory host pair`, sends the host's status to
  the Worker after every run but `--dry-run`, failed runs included. It carries the OS,
  scheduler, run time, bundle stamp, doctor result, routes and checkout names.
  It is silent, calls no model, and never changes the run's output or exit code.
  Beside a running daemon, which sends the heartbeat itself, a run sends it only
  when the backlog triage failure changed.
- Pushes this host's retro digest on its first run of each local date that
  `<home>/retro-push.json` does not record; a failed push prints one warning and
  the run goes on. On the host that keeps `<home>/retro-schedule-prompt.md`,
  pickup catches up a missed weekday retro: from 09:30 local, when no retro run
  folder carries today's date and `<home>/retro-catchup.json` does not record
  today, it launches that prompt once through the first route's launcher as
  `retro-<YYYYMMDD>`, in the first paths.md checkout that holds the factory crate
  and skill, never another repo. While no route or factory checkout is available,
  it prints why it
  waits once per local date and reason, recorded in
  `<home>/retro-catchup-wait.json`. `pickup --dry-run` prints it every time and
  records nothing.
- Pushes this host's audit rows for each job at spec, build, review, approve or
  pr-open whose transcripts here changed since `<home>/audit-push.json` records;
  a failed push prints one warning and the run goes on. A transcript a flag
  pattern cannot finish reading pushes none of its rows for that job. The push
  keeps this host's rows from before and adds one row whose target reads
  `audit push skipped: a flag pattern hit its backtrack limit, so newer calls are not shown`,
  and `skipped: true`, which the job drawer's `audit` tab shows but does not
  count as a call. Pickup prints
  `audit push skipped <job-id> until its transcripts change: ...`, and the run
  goes on. When the push succeeds or is skipped this way
  and this host's token totals for the job changed, it also writes them to
  `tokens.<device>` and prints `recorded <device> tokens on <job-id>`.
- Runs the read-only orphan scan on its first run of each local date that
  `<home>/orphan-scan.json` does not record, and prints one line such as
  `orphan scan: 4 orphaned (1 branch, 1 open PR, 1 worktree, 1 Paseo workspace); run factory orphans to list them`.
  The date is recorded before the scan, so a stopped scan prints one warning and
  waits for the next local date. `pickup --dry-run` skips it. Run
  `factory orphans` for the full list; nothing is removed.
- Runs `outcomes record <slug>` on its first run of each local date that
  `<home>/outcomes-record.json` does not record, for each project whose
  `paths.md` checkout folder exists, and prints one line such as
  `outcomes record alpha: recorded 1 outcomes on the board`. A stopped project
  prints one warning and waits for the next local date. `pickup --dry-run` skips it.

Pickup skips an idea held by a live claim, and any job whose `not_before` is still
in the future. The board `needs` field is only for input from a human. A session
that parks a job until a later time sets `not_before`, never `needs`, and does not
keep a claim on it.

## Configure

- Pipeline `pickup` is set per scope in the pipeline document: a project value
  replaces the global one whole, and no value anywhere means off.
  Edit it in the dashboard under Settings: Pipeline for any scope, or Projects for one project.
- The host name is the `device:` field of `<home>/config.md`, else the hostname.
  Host matching is exact and case-sensitive, so set `device:` on every host.
- `routes.md` lines may carry an optional effort token:
  `<name>: <provider> <model> [<effort>]`, for example
  `primary: provider-a model-a high`.
- `<home>/launchers.json` maps each route to a command template:

  ```json
  {"version": 1, "launchers": {"primary": ["<program>", "<arg>", "..."]}}
  ```

  Each `{placeholder}` is replaced in every argument: `{id}`, `{project}`,
  `{repo_path}`, `{session}`, `{provider}`, `{model}`, `{effort}` and `{prompt}`,
  where `{prompt}` is `/factory idea <id> --session <tag>`, a blank line, and the
  standing instruction below. A deny-notes launch sends
  `/factory deny-notes <id> --session <tag>` in place of the idea command.
  A babysit launch sends `/factory babysit <id> --session <tag>`.
  Pass `{prompt}` as one whole argument, as the
  examples below do. Its line breaks would split a shell command string or a
  Windows `.cmd` line. Unknown braces stay literal. The launcher also
  gets `FACTORY_JOB_ID`, `FACTORY_PROJECT`, `FACTORY_REPO_PATH`,
  `FACTORY_SESSION`, `FACTORY_PROVIDER`, `FACTORY_MODEL`, `FACTORY_EFFORT` and
  `FACTORY_PROMPT` in its environment, and runs in the project's checkout. Idea text never reaches a launcher.

Work projects list host `work-laptop`, personal projects list `home-pc`, and a
shared project lists both.

## Owner model experiment

- `routes.md` may carry one optional `experiment:` line in the frontmatter:
  `experiment: <name> <provider> <model> [<effort>] <percent>`, for example
  `experiment: pilot provider-b model-b high 50`. The name must differ from every
  route name. A malformed line exits 2 before anything is claimed.
- Pickup splits new ideas by a stable hash of the job id: arm `a` rides the picked
  route unchanged, arm `b` launches on the line's own provider, model and effort.
  The same id always lands in the same arm; percent 0 or 100 sends every idea to
  one arm. Route limits and fallback for the route itself are unchanged.
- At first claim pickup stamps the job's `experiment` field:
  `{name, arm, provider, model, effort}` — the arm and the values it launched
  with. Pickup never rewrites it. A relaunch (recover, rerun, move) of an arm `b`
  job launches on the recorded triple, even when the line changed or was
  removed; an arm `a` relaunch launches on the route's current values. The
  report attributes by the recorded name, so renaming the experiment starts a
  new population.
- An arm `b` launch that fails on a usage limit marks the experiment's own key in
  `route-limits.json`, not the route's, and prints `limited experiment <name> until
  <ts>`. While that key is limited, arm `b` jobs
  are skipped for the run (`skip <id> <project>: experiment <name> limited until
  <ts>`) and arm `a` jobs still launch; they never silently launch as arm `a`.
  An arm `a` failure limits the route name as today, which stops both arms since
  both ride its launcher, until it clears.
- `routes list` ends with an experiment row, `{"experiment": <name>, "provider",
  "model", ["effort"], "percent", "limited_until"}`, when the line is set. `routes
  limit <name>` and `routes clear <name>` accept the experiment name, and `limit`
  then prints `{"experiment": <name>, "limited_until": <ts>}`. Without the line the
  output is unchanged.
- Dry run prints ` arm <a|b> model <model>` on each `would launch` line when the
  line is set or the job carries a record; without the line the output is
  unchanged.
- `factory experiment report <name>` reads the board and the local job folders
  and prints per-arm metrics as JSON: job and stage counts, escalations, mean
  review rounds over jobs past review, CHANGES verdicts, stuck resumes, 7-day
  follow-ups split into `rework_of` (rework-of refs, the defect-fix signal) and
  `surfaced` (follow-up-of refs, no defect claim), `skipped_steps` found in local
  evidence (`missing_round_review`, `missing_decision`, `pr_before_approve`),
  and `coverage_unknown` where a job's local evidence is incomplete. It is
  read-only and writes nothing. Role dispatch does not read the field: only
  pickup's launch path and the report do. It also prints `comparison`:
  `inconclusive` until each arm has 5 finished (merged or closed) jobs, then for
  review rounds and tokens the median difference against a MAD noise figure,
  where a difference inside the noise reads `no difference`. It is advisory and
  nothing reads it.

## Launcher examples

Model ids are written as `<model>`; copy them from your own routing
configuration. The repository ships none.

- Paseo:

  ```json
  ["paseo", "run", "--no-wait", "--new-workspace", "worktree", "--cwd", "{repo_path}",
   "--provider", "{provider}", "--model", "{model}", "--thinking", "{effort}",
   "--title", "factory {id}", "--label", "factory-job={id}",
   "--label", "factory-session={session}", "{prompt}"]
  ```

- Claude Code headless:

  ```json
  ["env", "CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0", "claude", "-p", "{prompt}",
   "--model", "{model}", "--effort", "{effort}", "--permission-mode", "auto",
   "--strict-mcp-config"]
  ```

  `--strict-mcp-config` with no `--mcp-config` starts no MCP servers; the factory
  skill needs none.

  `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0` keeps a headless run waiting on its own
  background work, as SKILL.md requires. `--permission-mode auto` lets Claude Code
  approve routine actions without a prompt, since nobody answers one. Where auto mode
  is not available, use `dontAsk` with an allow list in your Claude Code settings.
  Never use `bypassPermissions`, `--dangerously-skip-permissions` or another flag that
  approves every action. On Windows, set `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0` as a
  user environment variable and drop the first two items.

Core needs neither. Any program that accepts these values works.

## Lean orchestrator sessions

- Pass a settings file with `--settings`: `syncClaudeAiSkills: false`,
  `disableClaudeAiConnectors: true`, and `skillOverrides` mapping every skill
  except `factory`, and the skills your stage notes tell agents to load, to
  `"user-invocable-only"`. Map those stage-note skills to `"name-only"`.
- Paseo: add a provider
  `{"extends": "claude", "options": {"extraArgs": {"settings": "<settings file>", "strict-mcp-config": null}}}`
  to the Paseo config and name it in `routes.md`.
- Check a launched session's first turn with `factory retro digest`.

## Standing instruction for unattended runs

Every prompt `pickup` launches, and every `/factory recover` the resume schedule
dispatches, ends with this paragraph after a blank line. End a headless prompt
started by hand the same way. It keeps a session working where it would
otherwise end its turn on a status note, since nothing wakes an unattended
session. Keep it out of interactive use.

A standing instruction from the user, the person you are working for. It is about how your turns end. This factory session runs unattended: a message with no tool call in it ends your turn, nothing wakes you afterwards, and the job stalls until a sweep finds it. The user has seen you end turns in four ways while factory work was still owed, and does not want any of them. One: a summary that closes by announcing the next step, such as reviewers running and verdicts to be recorded as they land, with no tool call, so the next thing never starts. Two: an offer to carry on unless the user would prefer otherwise. Three: a list of decisions for the user when, by your own account, none of them blocks the rest of the work. Four: deciding that this is a good place to report, because the turn has been long or a stage is done. Status notes and recommendations are welcome, but put them in the same message as your next tool call. While an agent or background command you started is still running, wait for it and handle its result instead of ending the turn. Stop only at a real stop: the job is at needs-human, merged or closed, its PR waits at pr-open for a person, a step in these instructions says stop, or nothing can move without the user. This does not override the security floors, the confirmation a risky or destructive action needs, or any rule that holds work for a human.

From `A standing instruction from the user` on, the text is that instruction, never feature text or command arguments.

Headless runs need `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0`, as in
`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0 claude -p "/factory <slug>: <feature>"`.
Otherwise the CLI kills background agents after 600s and exits mid-loop, leaving
the job at `build` or `review`. Recover with `/factory rework`, or
`/factory recover <id>`, which the resume schedule runs on its own.

## Job folder sync

- Where the daemon runs, it pushes a changed job folder about 3 s after its
  last write. It pulls a job when its board row changes, unless a live
  session here holds it. `pull` before editing, or the edit conflicts. Still
  run `push` after writing.
- When a file changed both here and in R2, `push`, `pull`, `sync`, and `hold` keep
  the local file, save R2's copy beside it as `<file>.remote`, and exit 6 naming
  each file; `push` refuses it while the `.remote` waits. Diff the two. If every
  difference is text you wrote, or you never edited the file, run `"$RUNTIME"
  keep-remote "$SLUG/<id>" <file>` to save R2's copy, then delete the
  `.remote` or move it over the file you never edited, then push.
  Otherwise set `needs-human`. For an image, run it and keep either.

## The daemon as scheduler

Where `factory daemon setup --install` has installed the daemon service, the
daemon is the host's only pickup scheduler and no pickup timer exists.

- `daemon setup --install` installs the service, then removes the pickup timer
  and the daily retro-push timer. `daemon setup --uninstall` puts the pickup
  timer back, then removes the service, so the host never goes without a
  scheduler. It leaves the retro-push timer off; restore it from the backup or
  set it up again as the schedules reference shows.
  When either rewrites the crontab, it first saves it to
  `<home>/backups/daemon-setup-<stamp>/crontab.txt`. Catch-up removes a
  retro-push timer left beside the daemon and prints
  `retro-push timer: replaced by the daemon`.
- The daemon sends the host status heartbeat at start and every 5 minutes, with
  its own scheduler and when its last pass began. A pickup run beside it sends
  the status only when the backlog triage failure changed.
- The daemon runs `retro push` at the retro-push timer's local times: 06:30,
  07:30 and 08:30 on macOS, 08:30 elsewhere. After a sleep it pushes once for the
  latest time missed. A time that passed before the daemon started is left to
  pickup's first-run-of-the-day push.
- The daemon starts a pass about 2 s after a board or backlog change, when a
  local Paseo agent titled `factory <id>` or `babysit <id>` stops running, right
  after a pass that launched a job, and after 5 minutes with no pass. Passes
  begin at least 30 s apart. It reads `paseo ls` at start and after a pass that
  launched, then every 15 s while a factory agent runs, backing off to every 60 s
  while nothing changes, and not at all once none runs.
- While the daemon cannot reach `/events/host`, it still runs a pass at start
  and then every 5 minutes. The 5-minute pass covers stale
  claims, takeover, `not before` schedules, retro and outcomes stamps and orphan
  scans.
- `pickup setup` reports check 7 as `scheduler: daemon` and passes it, and
  `--install` writes no timer. `daemon setup` and `doctor` at a terminal show the
  daemon state and when its last pass began.
- On a cron-only host the crontab's `@reboot` and `*/5` lines only start a dead
  daemon; they never run pickup. launchd, systemd and the Windows loop restart
  it themselves.
- The daemon also syncs the folders of active jobs (past idea, not merged or
  closed) that exist on this host. It scans them every 2 s and pushes a folder
  through `push`'s own code once it has been still for 3 s and differs from its
  last sync. It pulls a job when its board row changes, ignoring the claim
  heartbeat, but never while a live session on this host holds the job (claim
  device is this host and not stale). An automatic pull keeps each file's base,
  the version this host last took with `pull` or `push`. A later push sends
  `If-Match` on that base, so a write made from a copy read before the automatic
  pull meets a 412 and keeps both copies. Run `pull` before editing a file
  another host may have changed; that takes R2's version as the new base. It
  never follows a symlinked job folder, never uploads a file that looks like it
  holds a secret, and logs each file it pushes or pulls by name. A conflict
  keeps the local file, saves `<file>.remote` and goes to the daemon log. A
  failed push or pull retries after 60 s, doubling to 30 minutes, and stops once
  the job is merged or closed. The daemon and a manual `push` or `pull` take a
  lock on the job folder in turn. `factory push` on a folder that matches its
  last sync returns without a request while the daemon runs, so it no longer
  prints `remote newer`; the daemon pulls on the next row change. An old folder
  on a host for a job still active pushes its unpushed edits too.
- A manual `pickup` beside the daemon, or a leftover timer during migration, is
  safe: pickup takes no host lock, but each claim is a board compare-and-swap
  under a fresh session tag, so two runs never claim the same idea.

Everywhere else, the 5-minute timer below runs pickup.

## Run it every 5 minutes

`pickup setup --install` writes and loads this timer for you. The blocks below are what it writes.

Run `factory pickup --dry-run` first: it prints what it would launch and
changes nothing. Then install one timer per host.

- cron (Linux, WSL):

  ```
  */5 * * * * PATH=<launcher bin dir>:/usr/bin:/bin <installed-skill>/runtime/factory --home <home> pickup >> <home>/logs/pickup.log 2>&1
  ```

  WSL without systemd needs `service cron start`, plus the `/etc/wsl.conf`
  `[boot]` line.

- launchd (macOS), `~/Library/LaunchAgents/factory.pickup.plist` with
  `ProgramArguments` `<installed-skill>/runtime/factory`, `--home`, `<home>`,
  `pickup`; `EnvironmentVariables` `PATH`; `StartInterval` 300;
  `StandardOutPath` and `StandardErrorPath` `<home>/logs/pickup.log`. Load with
  `launchctl bootstrap gui/$(id -u) <plist>`.

- Task Scheduler (Windows): wrapper `%USERPROFILE%\.config\factory-pickup.cmd`
  that sets `PATH` and runs
  `<installed-skill>\runtime\factory.exe --home <home> pickup >> "<home>\logs\pickup.log" 2>&1`,
  then
  `schtasks /Create /TN factory-pickup /SC MINUTE /MO 5 /TR "%USERPROFILE%\.config\factory-pickup.cmd" /F`.
  On Windows the binary reads no time zone data, so usage limits default to 60 minutes.

## Stalled-session takeover

- When `<home>/stalls.json` names an adapter, `pickup` asks it for sessions stalled on a
  usage limit. A stall whose reset is more than `takeover_minutes` away (default 30) gets
  its route marked limited, its board claim replaced with a fresh tag and its session
  archived, then `pickup` relaunches `/factory recover` (or `/factory idea` for an idea
  job) on the next route; at most one takeover per run, counted against `--limit`.
  A launched takeover appends an orchestrator row on the fallback route to `agents`.
- The two time windows never overlap: pickup acts only while the reset is more than
  `takeover_minutes` away, the limit-resume sweep only after the reset, and the archive is
  the marker that stops the sweep reviving a taken session.

  | Time left until reset | Who acts |
  | --- | --- |
  | more than `takeover_minutes` | pickup takeover only |
  | 0 to `takeover_minutes` | nobody; the session waits |
  | reset passed | the sweep only |

- `stalls.json` is device config written by hand and never synced:
  `{"version": 1, "adapter": ["<program>", "<arg>", ...], "takeover_minutes": 30}`.
  Deleting the file turns takeover off. A failed takeover launch leaves a claim that
  `factory stuck` recovers after `claim_stale_minutes`.
- The adapter is an argv command with `list` and `archive <session>` verbs and JSON output;
  the binary's `stalls` command is the Paseo adapter:
  `{"version": 1, "adapter": ["<installed-skill>/runtime/factory", "stalls"], "takeover_minutes": 30}`
  (`factory.exe` on Windows).
  It reports only idle, unarchived Claude limit stalls and re-checks a session before
  archiving it, so a session a human or the sweep has woken is kept.
  `archive <session> --force` archives without the stall re-check, for a moved job.
- `routes.md` order sets the fallback: pickup limits the stalled route and launches on the
  first route that is not limited, so put the route you want after a limit second.
- A host whose `max_inflight` is 0 takes over nothing. Dry-run prints
  `skip stall <session> <id>: host <host> paused (max in flight 0)`.

## Moving a job to another host

- The dashboard's move button replaces a job's `claim` and `paseo` with `move = {from, session, to, at}`. Only the dashboard, `pickup` and `factory control <job-id> move <host>|rerun --chat-file <file>` write `move`; `control` adds the chat record `via`, `quote` and `by`.
- A second move keeps each earlier holder in `move.earlier`. Those sessions stay fenced, and each earlier host's `pickup` still stops its worktree.
- The old session is fenced: its next `board` or `hold` exits 6 with `<id> was moved to <host>`. `hold <project>/<id> take` still claims the job.
- The old host's `pickup` stops the job's worktree processes first on every run, then sets `move.stopped`.
  - It stops them only when the project's pickup is on and lists that host. Otherwise the stop stays queued.
  - Each run prints `pickup skips the stop of <id> <project>: <reason>` on stderr, and a dry run prints `skip stop <id> <project>: <reason>` on stdout instead.
  - No command or dashboard item clears a queued stop: `board` refuses a `move` patch with exit 2, and moving the job again keeps the stop queued.
  - To clear it, fix the printed reason on that host:
    - `pickup off` or `pickup does not list host <host>`: turn the project's pickup on or add that host in Settings: Pipeline or Projects.
    - `no checkout on this host`: add the project's checkout to that host's `paths.md`.
    - Any other reason, such as an `unsafe checkout`: fix what it names.
  - The next `pickup` run on that host then stops the job and sets `move.stopped`.
  - Only a string `move.stopped` counts as done. Pickup and the dashboard treat `null` or any other value as a stop still queued.
  - While that host is offline the stop waits on the board. Survivors are printed on stderr and retried next run.
- Under the same gate, when `stalls.json` is set and the job's Paseo session ran on this host's server, the old host's `pickup` archives it with the adapter's `archive <session> --force` first. While the gate skips the stop, the session stays unarchived too. A failed archive is printed on stderr and retried next run. It archives only a session whose `<home>/paseo-claims/<job-id>/<agent>` file shows this host recorded it claiming that job; otherwise it only stops the worktree and prints a "kept session" hint on stderr. Pickup never archives an agent id the adapter would refuse. It stops the worktree and prints a "skipped archive" hint on stderr instead.
- The "kept session" hint reads `pickup kept session <agent> of <id>: no claim recorded on this host, archive it by hand if it still runs: paseo archive --force <agent>`. The "skipped archive" hint ends with the same command. Both put `--home <PASEO_HOME>` after `paseo` when `PASEO_HOME` is set. When `PASEO_HOME` holds an unprintable character, both leave `--home` out and put `(left out --home because PASEO_HOME has unprintable characters)` before the colon that starts the command, so the command still ends the line. Both leave the command off when the id starts with `-` or holds a character outside `A-Z`, `a-z`, `0-9`, `_` and `-`.
- To archive a kept session, run the `adapter` argv from `stalls.json` followed by `archive <agent> --force` on the host that printed the hint. With the Paseo adapter that is `<installed-skill>/runtime/factory stalls archive <agent> --force`. It accepts only ids that match `[0-9a-fA-F][0-9a-fA-F-]{7,63}`, checks the session with `paseo inspect` first, and does nothing if it is already archived. If `paseo inspect` fails or prints bad JSON, it archives anyway.
- The adapter argv only archives kept sessions. A "skipped archive" id fails the adapter's id check, so run the printed `paseo archive --force <agent>` command instead. That command calls Paseo directly, with no id check and no already-archived check. `--force` archives the session even while it is working, so check first that its work is saved or no longer needed.
- The target host's `pickup` claims the job once, sets `move.claimed` and launches `/factory recover`. The usual gates apply: pickup on, host listed, not gated, the cap and a route.
- `factory stuck` leaves a move with no `claimed` to its target.

## Rerunning a stage

- The dashboard's Rerun stage item writes a self-move: `move` with `to` equal to `from`, the job's own host. It shows only for a claimed job at spec, build, review or approve that is not paused and has no `needs`.
- That host's `pickup` claims the rerun only after its own stop is recorded. While a stop from this host is still queued, the job waits.
- When the stop succeeds, the same run claims the job, sets `move.claimed` and launches `/factory recover`, which restarts the current stage in a fresh session. Pickup prints `stopped <id> <project>: rerun on <host>` before `launched ...`. If the stop fails, nothing is claimed and the next run retries both.
- The dashboard refuses a rerun when the host does not run pickup for the project, since nothing would restart the job. Use Move to another host instead.
- `factory control <job-id> rerun --chat-file <file>` writes the same self-move from a person's chat message, with the same checks.

## Prioritizing a job

- The dashboard's Prioritize item sets `priority = {at}` on a job at `idea`, `spec`, `build`, `review` or `approve`. Remove priority deletes it.
- Run batch now on a held idea sets `priority.batch`. Pickup starts it next, and while it is at `idea` and not paused, closing or waiting on a human, the project's other held ideas print `batches behind <id>`. Start alone sets `priority.alone`, and `batch` then lists no candidates and refuses riders.
- `pickup` claims prioritized ideas before older ones, across projects too. Gates, caps and routes still apply.
- The dashboard shows a prioritized job at the top of its column in idea, spec, build, review and approve.
- When the job's session on this host was claimed before `priority.at` and its Paseo agent is not closed, `pickup` sends the agent one fixed nudge with `paseo send --no-wait`, then sets `priority.nudged`. A failed send is printed on stderr and retried next run.
- An agent waiting at a permission dialog gets no nudge. `pickup` reads `PendingPermissions` from `paseo inspect <agent> --json` and prints `skip nudge <id> <project>: agent <agent> waits at a permission dialog`, dry run or not. Any answer but an empty list counts as unknown and prints `skip nudge <id> <project>: agent <agent> permission state unknown`.
- Only the dashboard, `pickup` and `factory control` write `priority`. `board` refuses a `priority` patch with exit 2.

## Stopping the whole factory

- The dashboard's Stop factory form in Settings > Hosts sets the global stop flag, and the banner's Resume factory clears it. Only a signed-in person can clear it.
- An agent that sees an incident needing every host stopped runs `factory halt set --reason <text>`. `factory halt status` prints the record.
- While the flag is set, `pickup` prints `pickup launches nothing; factory stopped by ...` on stderr and runs no pass: no launch, takeover, triage or Tracer pass. Its dry run prints `skip all: ...`.
- `stuck` exits 3, so the resume schedule resumes nothing.
- Sessions already running keep running. A stop blocks new starts only.
- A Worker without the `/api/halt` route answers 404, which means not stopped.

## Nudging an idle job

- `pickup` watches each job at `spec`, `build`, `review` or `approve` that this device claimed and whose Paseo agent runs on this host's server. It skips jobs with an ask, a pause, a queued close or a batch lead.
- The quiet signature is the agent, the worktree's `HEAD`, a hash of `git status --porcelain` and the job's `updated`. It lives in `<home>/idle-watch.json`, never on the board. Any commit, file edit or board write starts the window again.
- Only an `idle` agent counts. A `running` agent keeps its entry and is skipped that run.
- A claim the daemon's heartbeat refreshed counts as not idle. That means the claim's `seen` is under 20 minutes old and newer than the job's `updated`, which only the heartbeat leaves behind. The heartbeat refreshes an idle agent only while a Paseo subagent it launched runs, so the job keeps its entry and is skipped that run, as a `running` agent is.
- After 20 quiet minutes, `pickup` sends the agent one fixed nudge with `paseo send --no-wait` and prints `nudged idle <id> <project>: agent <agent>`. `--dry-run` prints `would nudge idle ...` and writes nothing. A failed send prints `pickup could not nudge idle <id>: paseo send failed` on stderr and retries next run.
- The idle nudge skips an agent waiting at a permission dialog, as the priority nudge does, and prints `skip idle nudge <id> <project>: agent <agent> waits at a permission dialog` or `... permission state unknown`. The entry gets no `nudged`, so the job never moves to `needs-human` while it waits.
- When the agent waits at a permission dialog, `pickup` sets the job's note once per wait to `permission: agent <agent> has waited at a permission dialog for 20 minutes; answer it in Paseo` and prints `surfaced <id> <project>: <note>`. The write needs the same stage and a claim by this host. It sets no `needs`, moves no stage and sends nothing. The entry records `surfaced`, so later runs write nothing until the quiet signature changes. `--dry-run` prints `would surface <id> <project>: <note>`. A `permission state unknown` answer writes no note.
- The nudge reads: "Factory job <id> shows no commit, file or board change for 20 minutes while this session sits idle. If work on it is still owed, continue with the next step now. If you are waiting on an agent or command that is still running, run factory hold for the job with your --session tag, then keep waiting."
- After 20 more quiet minutes, `pickup` moves the job to `needs-human`, releases the claim and prints `needs-human <id> <project>: idle: no progress 20 minutes after a nudge to agent <agent>`. A board change since the read wins, so a fresh `hold` keeps the job.
- `--dry-run` prints `would move needs-human <id> <project>: <note>` for that move and writes neither the board nor `idle-watch.json`.
- When paseo does not answer, `pickup` prints `pickup skips idle checks: paseo did not answer` on stderr.

## Token cap

- Set `job_token_cap: <n>` in `config.md` to cap the tokens each job may spend on this host. Without it the pass is off and `pickup` never scans transcripts for it.
- Right after the idle watch, `pickup` checks each job at `spec`, `build`, `review` or `approve` that this device claimed. It skips jobs with a queued close.
- A job's count is input, output and cache_write tokens, without cache_read. This host scans its own transcripts. Each other host counts through its `tokens.<host>` entry on the board. `used` is that total minus `token_cap.base`.
- When `used` reaches 80% of the cap and the job has no `token_cap.warned`, one write sets the warn note, `token_cap.warned` and this host's `tokens` entry.
- When `used` reaches the cap, one write moves the job to `needs-human`, releases the claim, sets `token_cap.base` to the total and this host's `tokens` entry, then `paseo stop` stops the agent when it runs on this host's server. A resumed session gets a full cap again.
- A board change to the stage or claim device since the read wins, and nothing is written. Each note replaces the job's `note`.

| Case | Line |
| --- | --- |
| warn note | `tokens: {used} of the {cap} job cap ({pct}%) on {device}` |
| park note | `tokens: {used} reached the {cap} job cap on {device}; a resumed session gets another {cap}` |
| warn print | `token warning {id} {project}: {note}` |
| park print | `needs-human {id} {project}: {note}` |
| dry warn | `would warn tokens {id} {project}: {note}` |
| dry park | `would move needs-human {id} {project}: {note}` |
| bad config | `pickup ignores job_token_cap in {config}: must be a whole number of at least 1` |
| scan error | `pickup skips the token cap of {id}: scan failed ({Type})` |
| stop failed | `pickup could not stop agent {agent} of {id}: paseo stop failed` |

Numbers use `,` thousands separators, and `pct` is `used * 100 // cap`. The last three lines go to stderr. `--dry-run` writes nothing and never stops an agent.

## Daily token cap

- Set `daily_token_cap: <n>` in `config.md` to stop new launches on this host for the rest of the local day once its job sessions have spent `n` tokens since local midnight. Without it `pickup` never runs the scan.
- The count reads this host's Claude Code transcripts, not the board `tokens` entries, which lag by whole jobs. It walks only folders whose name matches `factory-<digits>-`, the rule the retro digest uses to tag a job session.
- It reads each main `*.jsonl` and its `<stem>/subagents/*.jsonl` last written at or after local midnight, and keeps only assistant lines whose `timestamp` is at or after midnight. Each line counts input, output and cache_write tokens, as the job cap does.
- The scan runs only when the key is set and some idea or approved start is otherwise a launch candidate. At or over the cap, every such candidate is held, its skip line wins over batch or backpressure lines, and no claim is written.
- Takeovers, babysits, resolves and running sessions are not held. They can push spend past the cap; the cap stops new starts only. Sessions outside a `factory-<digits>-` folder are not counted.

| Case | Result |
| --- | --- |
| key absent | off, no scan |
| `daily_token_cap: lots` | bad config line, off |
| scan raises | scan error line, launches go on |
| after local midnight | the count restarts at 0 |

| Case | Line |
| --- | --- |
| dry skip | `skip {id} {project}: host {device} daily token cap reached ({used}/{cap} since local midnight)` |
| stop | `pickup launches no new job on {device}: daily token cap reached ({used}/{cap} since local midnight)` |
| bad config | `pickup ignores daily_token_cap in {config}: must be a whole number of at least 1` |
| scan error | `pickup skips the daily token cap: scan failed ({Type})` |

Numbers use `,` thousands separators. The last three lines go to stderr. A real run prints the stop line in place of the skip lines; `--dry-run` prints the skip lines only.

## Weekly expected spend

- The Worker, not pickup, compares finished-job tokens with a project's weekly_tokens and pushes nudges. It never holds a launch.

## Pausing a job

- The dashboard's Pause pickup item sets `paused = {at}` on a job with a project that is not merged or closed. Resume pickup deletes it.
- `pickup` does not claim, take over or nudge a paused job, and its dry run prints `skip <id> <project>: paused` or `skip stall <agent> <id>: paused`. `stuck` does not resume it.
- Right after the move stops, `pickup` runs `paseo stop` on a paused job's linked agent when it runs on this host's server, the claim is absent or this host's, and the agent is not `closed` or missing. It records `paused.session_stop.<host> = {agent, at}`, prints `stopped session <agent> of <id> <project>: the job is paused` and keeps the claim, so Resume lets `stuck` or the idle nudge restart the work. A pause after a Resume stops the session again.
- The job panel shows each recorded stop, paused or closing, as `Pickup stopped session <agent> on <host> <time> because the job was paused.` or `... closed.`
- `pickup` never stops the agent running it, a batched rider's agent or another harness's session. A failed stop prints `pickup could not stop agent <agent> of <id>: paseo stop failed` on stderr and records nothing, so the next run retries. `--dry-run` prints `would stop session <agent> of <id> <project>: <why>` and writes nothing. When paseo does not answer, it prints `pickup skips session stops: paseo did not answer` on stderr.
- Only the dashboard and `factory control` write `paused`. `board` refuses a `paused` patch with exit 2.

## Closing a job

- The dashboard's Close job item sets `close = {reason, at}` on a job with a project that is past `idea` and not merged or closed. The reason is one line of 1 to 200 characters, with no control characters and no U+0085, U+2028 or U+2029.
- `pickup` runs its close pass right after the move stops. It writes `decision.md` with first line `CLOSED: <reason>`, keeping earlier text under `## Earlier decision`, and only then sets the stage to `closed`, the note to the reason and `close.closed`. Its dry run prints `would close <id> <project>`, or `skip close <id> <project>: claimed by <device> session <tag>` for a job with a live claim.
- When the job has a `pr` in the project's `repo`, the close pass runs `gh pr view`, and for an open PR `gh pr close --comment`, before it writes `decision.md`. It reports a merged or closed PR as such before it checks the PR's head branch or the job's branch. It closes an open PR only when the job has a branch, and the PR's head branch equals it and lives in the project's repo, not a fork. The comment opens with the agent disclosure marker and links `<api_url>/#<id>/decision`. When gh is missing, fails or times out, the job still closes and `decision.md` says why the PR stayed open.
- When the closed job has a stacked rider with an open PR, the close pass closes the rider's open PR with a disclosed comment after the board write, and rewrites every such rider's note in a single board write after the last PR closes.
- Before the close pass, `pickup` stops this host's linked Paseo agent of a closing job, as it does for a paused one, and records `close.session_stop.<host> = {agent, at}`. The same write releases this host's claim, so the close pass closes the job in that run. It prints `stopped session <agent> of <id> <project>: the job is closing`. A job closed in the last 24 hours, on any host, gets the same stop with `the job is closed`.
- A session on a closing job is refused at its next `board` or `hold` call with exit 6. `board` first releases that session's own claim, so the next pickup can close the job.
- Only the dashboard, `pickup` and `factory control` write `close`. `board` refuses a `close` patch with exit 2.
- The next pickup run on a host that still holds a closed job's worktree or `factory/<id>` branch runs `cleanup` on it when the project's pickup is on and lists that host, once per host, and records `close.cleanup.<host> = {at, left}`. It does the same for a `merged` job with `github.merged`, which the PR sync sets, and records `github.cleanup.<host> = {at, left}`. It keeps a dirty worktree, and a branch that is not merged unless origin holds the same commit and no worktree has it checked out. It deletes that branch with `git update-ref -d` guarded by the branch's current commit, never `git branch -D`. For a `merged` job it also deletes a branch whose tip `github.head` contains, guarded by the branch's current commit. The kept branch's `left` entry ends with `and origin could not be reached`, `and origin has no such branch` or `and origin is at a different commit`. When `factory stop` refuses to check a worktree's processes (an empty, unresolvable or linked path, or, where there is no `/proc`, lsof is missing), it exits 6 and signals nothing. `cleanup` then keeps that worktree and its branch, records `<worktree>: processes were not checked; fix the cause factory stop printed (remove the link or install lsof), or stop them and run git worktree remove yourself` in `left`, and exits 1. Its dry run prints `would clean up <id> <project>`. The close pass's `decision.md` names each host the board records as holding the job, from `claim` and every `move` holder, that the project's pickup is off for or does not list, and says which of the two it is. When it cannot read the pickup hosts, it names every recorded holder and says why it could not read them.

## Syncing PR state

- `pickup` runs its PR sync pass right after the move stops and before the close pass. It never starts a launcher or calls a model.
- A job is a candidate when its stage is `pr-open` or `needs-human`, its `pr` is a `https://github.com/<owner>/<name>/pull/<n>` URL with `<n>` at most 9 digits, no close is queued, it is not a batched rider (riders follow their lead), no other session holds a live claim, the project file's `repo` equals the URL's owner and name (case-insensitive), and the project's pickup is on and lists this host.
- Candidates are grouped by repo. Each repo gets one `gh api graphql` call that reads `state` for up to 100 pull requests in board order. With more, it prints `pickup syncs the first 100 of <n> pull requests in <repo>` on stderr.
- When the call fails, it prints `pickup skips the PR sync for <repo>: <why>` on stderr and the run goes on: gh is not installed, timed out after 60 seconds, exited non-zero (check `gh auth status`), or returned no repository (check the project file's `repo` and `gh auth status`). A non-zero exit whose answer still holds `data.repository` syncs the pull requests that resolved.
- `MERGED`: the pass's board write moves the job to `merged`, sets the note to `merged on GitHub` and `github = {merged, head}`, with `github.head` the PR's `headRefOid`, and removes `needs`, `automerge`, `babysit` and `claim`. It prints `synced <id> <project>: merged on GitHub`. The close cleanup then runs `cleanup` on each host that holds the worktree or branch, as for a closed job.
- `CLOSED`: the pass's board write queues `close = {reason: "PR closed on GitHub", at, via: "github"}` and removes `babysit`. It prints `synced <id> <project>: PR closed on GitHub`. The close pass in the same run closes the job without calling `gh pr`, and `decision.md` starts `CLOSED: PR closed on GitHub` and says GitHub reported the close.
- The pass puts every row it syncs in one run into a single board write. That write re-checks every candidate condition and the same `pr` for each row, so a second host that read the same state writes nothing for it.
- An open PR, or one GitHub does not return, changes nothing and prints nothing.
- Its dry run makes the same read and writes nothing. It prints `would sync <id> <project>: merged on GitHub` or `would sync <id> <project>: PR closed on GitHub`, and `skip sync <id> <project>: claimed by <device> session <tag>` or `skip sync <id> <project>: <reason>` for a live claim or a project whose pickup is off or does not list this host.
- Only pickup writes `github` and `close.via = "github"`. `board` refuses a `github` patch with exit 2.
- A synced move does not mirror Tracer, archive a Paseo workspace or post a stage-change chat line. Agents still own those.

## Prebuilt dependencies

`factory worktree` installs the folders a device-local `<home>/prebuild.json`
lists for the project, so agents never run `pnpm install` themselves. The host owns
the file, and nothing syncs it:

```json
{"version": 1, "prebuild": {"factory": ["paseo-plugin"]}}
```

The file names folders only. The command is fixed: `mise exec -- pnpm install
--frozen-lockfile --ignore-pnpmfile`, or `pnpm install --frozen-lockfile
--ignore-pnpmfile` when `mise` is not on `PATH`.
Each install appends to `<home>/logs/prebuild-<job-id>.log`, and stdout still prints
only the worktree path. Every outcome keeps exit 0, and each folder reports on stderr:

| Condition | stderr line |
| --- | --- |
| `node_modules/.modules.yaml` exists | none, skipped |
| no `pnpm-lock.yaml`, or the path leaves the worktree | `prebuild skips <dir>: no pnpm-lock.yaml` / `prebuild skips <dir>: not inside the worktree` |
| the working tree, untracked and gitignored files included, differs from `origin/<base>` in the folder's `pnpm-lock.yaml`, `package.json`, `.pnpmfile.cjs` or `.pnpmfile.mjs`, or, at the worktree root, in every folder between it and the folder, or in the folder itself, any name with `mise` in it or under such a folder, at that level or in `.config/` (so `mise.toml`, `mise.local.toml`, `.miserc.toml`, `.config/miserc.toml`, `mise.lock`, `.mise/` and `.config/mise/conf.d/` all count), `.tool-versions`, `.npmrc` or `pnpm-workspace.yaml` | `prebuild skips <dir>: its dependency files differ from origin/<base>` |
| neither `mise` nor `pnpm` on `PATH` | `prebuild skips <dir>: pnpm is not on PATH` |
| install exits non-zero, cannot start, or runs past 600 s | `prebuild of <dir> failed (<why>); log: <log>` |
| success | `prebuilt <dir> with pnpm install --frozen-lockfile` |

A bad file prints `cannot read <path>: use {"version": 1, "prebuild":
{"<project>": ["<dir>", ...]}}`. pnpm writes `.modules.yaml` last, so a killed
install retries on the next `worktree` call. A cold install can take minutes, so give
the `worktree` call a 600000 ms timeout.

## Logs and limits

Each launch writes `<home>/logs/pickup/<id>-<tag>.log` (mode 0600, one file per
launch, delete freely). A usage-limit failure marks the route with `routes limit`
from the reset time in the log, else 60 minutes, and releases the idea so the
next run or another host can take it.

## Migrating from the ideas schedule

The bundle no longer ships `schedules/ideas.md`. A stored ideas schedule keeps its old prompt until you disable it.

Deploy the Worker, reinstall the skill on each host, write `launchers.json`, run
`pickup --dry-run`, enable pickup for the project, install the cron entry, then
pause (disable, do not delete) the stored ideas schedule. Run both together only
briefly: the board claim stops double pickup.

A stored Paseo or Claude schedule prompt does not pick up ideas. Only the daemon service or the host timer runs pickup.
