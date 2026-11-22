# Automatic pickup

`factory.py pickup` is a model-free command that a host timer runs every 5
minutes. It claims runnable ideas for the projects whose pipeline lists this
host and starts one session per idea through `launchers.json`.

## Agent setup prompt

Paste this prompt into a coding agent running on the host, such as Paseo, Claude Code or OpenCode. The agent drives `pickup setup` until every check is ok. The dashboard's Settings, Scheduler, Agent tab shows the same text; a test keeps the two equal.

```text
Set up automatic factory pickup on this host. You are a coding agent on the machine that should pick up ideas. Work through these steps in order.

1. Find the installed factory skill: the folder that holds SKILL.md and runtime/factory.py, often ~/.agents/skills/factory or ~/.claude/skills/factory. Below, SKILL means that folder. Run: python3 SKILL/runtime/factory.py pickup setup
   If there is no such folder, or pickup setup prints a usage error instead of checks, the skill is missing or older than this setup; install or update it with python3 tools/install.py --destination <skills folder> --replace from a clone of the factory repo, then start again.
2. It prints 7 numbered checks, each ok or missing, with a fix under each missing one. Fix checks 1 to 6 in order. After each fix, run pickup setup again.
   - home: run the doctor line it prints. If the factory home or its credentials are missing, stop and tell me. Never print or copy values from the .env file.
   - device: add a device: line inside the frontmatter of config.md in the factory home, with a short name for this host.
   - checkout: run the init line it prints with the path of this host's clone of each project. That adds it to paths.md.
   - routes: write routes.md from providers and models that exist on this host. Find them first, for example with paseo provider ls and paseo provider models <provider>, opencode models, or the model aliases claude --help lists. Never copy route, provider or model names from another person's setup.
   - launchers: write launchers.json with one launcher per route, copied from the matching example in SKILL/references/pickup.md. Keep every argument the example shows, including its permission mode. Never add --dangerously-skip-permissions or any flag that approves every action.
   - pipeline: pickup must be on for the project and list this host exactly as pickup setup prints it. That is a dashboard setting under Settings, Pipeline, or Settings, Projects, then the project. If you cannot change it yourself, tell me exactly what to click and type, and wait until I say it is done.
3. Run python3 SKILL/runtime/factory.py pickup --dry-run and show me what it would launch.
4. Check 7 is the timer. Show me the command python3 SKILL/runtime/factory.py pickup setup --install and say that it writes and loads a 5 minute timer for this host. Run it only after I answer yes. Otherwise I run it myself.
5. Run pickup setup again until it prints 7 of 7 ok. Then tell me which python the timer runs and any note it printed.

A stored Paseo or Claude schedule prompt does not pick up ideas, so do not create one. Only the host timer runs pickup.
```

## Set up and check

Run this on each host. It checks every piece pickup needs, in order, and prints
the exact fix for each missing one with this host's own paths:

```
python3 <installed-skill>/runtime/factory.py pickup setup
```

| # | Check | What it reads |
| --- | --- | --- |
| 1 | `home` | `factory.py doctor`: config, credentials, projects and board |
| 2 | `device` | `device:` in `config.md`; without it the host name is the hostname |
| 3 | `checkout` | `paths.md`, a checkout that exists on this host |
| 4 | `routes` | `routes.md` |
| 5 | `launchers` | `launchers.json`, a launcher per route |
| 6 | `pipeline` | each project's effective pipeline: pickup on and lists this exact host |
| 7 | `timer` | the 5 minute timer in launchd, cron or Task Scheduler |

It exits 0 when all 7 are ok, 3 when any is missing, and 2 on bad arguments.
It writes no factory file. Then install the 5 minute timer for this host's
scheduler. Running it again changes nothing:

```
python3 <installed-skill>/runtime/factory.py pickup setup --install
```

| Scheduler | File | Load | Already installed when |
| --- | --- | --- | --- |
| launchd | `~/Library/LaunchAgents/factory.pickup.plist` | `launchctl bootout`, then `bootstrap gui/<uid>` | same bytes and `launchctl print` exits 0 |
| cron | user crontab | `crontab -` | the exact line is present, and no other pickup line |
| schtasks | `~/.config/factory-pickup.cmd` | `schtasks /Create ... /F` | always re-created (`/F`) |

The timer runs `factory.py --home <home> pickup` with `/usr/bin/python3` when that python runs the runtime, else with the python that ran `--install`. The second line of the output names the timer python and why. When a timer already exists but runs a different python or skill path than `--install` would write, check 7 stays ok and prints a `note:` line. On cron, `--install` replaces any old line that runs
`factory.py pickup` and keeps every other line. The timer `PATH` holds the launcher folders, the timer python's folder, gh's folder when `gh` is on the installing shell's `PATH`, and the system folders. On launchd, a plist whose `PATH` differs from what `--install` would write gets the same `note:` line.

A stored Paseo or Claude schedule prompt does not pick up ideas. Only the host timer runs pickup.

## What it does

Each run, in order:

- Reads the board and lists the `stage: idea` jobs whose project has a checkout
  in `paths.md` on this host.
- Also lists `build` jobs with note `spec approved in Tracer`, which Tracer scope
  approval released, or `spec approved on the dashboard`, which Approve spec released, and launches `/factory recover <id>` for them instead of
  `/factory idea <id>`. They follow the same skips, claim and caps as ideas.
- Skips ideas that are gated, held by a live claim, waiting on a human, or whose
  `not_before` is still in the future (`skip <id> <project>: not before <time>`), or
  that depend on a job still open on the board (`skip <id> <project>: waits on <dep-id>`), and
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
  `max_wait_hours` 1 to 168), it replaces 5 and 12. `factory.py ideas` does not
  hold. A dry run prints a project's pickup off, host or pipeline skip line for
  its held ideas instead of the batch line. Set them in the dashboard under
  Settings, Pipeline, Batch follow-ups. The board shows the same state on each
  held idea's chip: "Waits for a batch (n of min, hh of maxh)", "Batches behind
  <id>", or "Batch ready" on the one pickup launches next.
- Holds back plain ideas while review falls behind. Pickup counts the jobs that
  wait on you across every project, as the dashboard's Needs you column does:
  `needs-human`, a set `needs`, or an escalated `pr-open` PR. At or above the
  effective pipeline's `backpressure.needs_you` (a whole number from 1 to 50,
  default 5), it skips each idea with
  `skip <id> <project>: backpressure (<n> waiting on you, pauses at <limit>)`.
  Prioritized ideas, approved specs and moved jobs still launch. This line
  replaces a batch line for the same idea. Set it in the dashboard under
  Settings, Pipeline, Review backpressure.
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
- Deny notes: before ideas, launches at most one `/factory deny-notes <id>` session
  for a `closed` job denied from the backlog whose `backlog.notes` are unread (no
  `backlog.notes_read`). The session gets the notes from `factory.py deny-notes <id>`
  inside a `backlog-notes` envelope, queues the follow-ups they ask for with
  `backlog add`, then runs `deny-notes <id> done`. Notes that fail admission are
  skipped. This launch does not count against `--limit` or the in-flight cap.
- Resolve: right after deny notes, launches at most one `/factory rework <id> --session <tag>`
  or `/factory approve <id> --session <tag>` session for a `needs-human` job whose
  `resolve` the dashboard's Rework or Approve as is set. It claims the job and changes
  nothing else. The note never enters argv; the session reads it with `factory.py job`
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
  with the same tag. A new babysit session gets workspace slug `babysit-<id>` and title
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
- Stops after `--limit` launches (default 1) or when a full pass claims nothing.
  It is silent when there is nothing to do, and it never calls a model.
- Caps in-flight work. On a host with a registry record, the cap is the host's
  `max_inflight` from Settings, Hosts, counted over this host's fresh claims in
  every project. A `max_inflight` of 0 pauses pickup on that host, stall
  takeover included; jobs already in flight and deny-notes launches keep
  running. Jobs waiting on a human never count. `--max-inflight N` lowers it
  further. A host with no record caps each project at `--max-inflight`
  (default 1), as before.
- On a host paired with `factory.py host pair`, sends the host's status to
  the Worker after every run but `--dry-run`, failed runs included. It carries the OS,
  scheduler, run time, bundle stamp, doctor result, routes and checkout names.
  It is silent, calls no model, and never changes the run's output or exit code.
- Pushes this host's retro digest on its first run of each local date that
  `<home>/retro-push.json` does not record; a failed push prints one warning and
  the run goes on. On the host that keeps `<home>/retro-schedule-prompt.md`,
  pickup catches up a missed weekday retro: from 09:30 local, when no retro run
  folder carries today's date and `<home>/retro-catchup.json` does not record
  today, it launches that prompt once through the first route's launcher as
  `retro-<YYYYMMDD>`. While no route or checkout is available, it prints why it
  waits once per local date and reason, recorded in
  `<home>/retro-catchup-wait.json`. `pickup --dry-run` prints it every time and
  records nothing.
- Pushes this host's audit rows for each job at spec, build, review, approve or
  pr-open whose transcripts here changed since `<home>/audit-push.json` records;
  a failed push prints one warning and the run goes on. When the push succeeds
  and this host's token totals for the job changed, it also writes them to
  `tokens.<device>` and prints `recorded <device> tokens on <job-id>`.
- Runs the read-only orphan scan on its first run of each local date that
  `<home>/orphan-scan.json` does not record, and prints one line such as
  `orphan scan: 4 orphaned (1 branch, 1 open PR, 1 worktree, 1 Paseo workspace); run factory.py orphans to list them`.
  The date is recorded before the scan, so a stopped scan prints one warning and
  waits for the next local date. `pickup --dry-run` skips it. Run
  `factory.py orphans` for the full list; nothing is removed.

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
- `factory.py experiment report <name>` reads the board and the local job folders
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
   "--model", "{model}", "--effort", "{effort}", "--permission-mode", "auto"]
  ```

  `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0` keeps a headless run waiting on its own
  background work, as SKILL.md requires. `--permission-mode auto` lets Claude Code
  approve routine actions without a prompt, since nobody answers one. Where auto mode
  is not available, use `dontAsk` with an allow list in your Claude Code settings.
  Never use `bypassPermissions`, `--dangerously-skip-permissions` or another flag that
  approves every action. On Windows, set `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0` as a
  user environment variable and drop the first two items.

Core needs neither. Any program that accepts these values works.

## Standing instruction for unattended runs

Every prompt `pickup` launches, and every `/factory recover` the resume schedule
dispatches, ends with this paragraph after a blank line. End a headless prompt
started by hand the same way. It keeps a session working where it would
otherwise end its turn on a status note, since nothing wakes an unattended
session. Keep it out of interactive use.

A standing instruction from the user, the person you are working for. It is about how your turns end. This factory session runs unattended: a message with no tool call in it ends your turn, nothing wakes you afterwards, and the job stalls until a sweep finds it. The user has seen you end turns in four ways while factory work was still owed, and does not want any of them. One: a summary that closes by announcing the next step, such as reviewers running and verdicts to be recorded as they land, with no tool call, so the next thing never starts. Two: an offer to carry on unless the user would prefer otherwise. Three: a list of decisions for the user when, by your own account, none of them blocks the rest of the work. Four: deciding that this is a good place to report, because the turn has been long or a stage is done. Status notes and recommendations are welcome, but put them in the same message as your next tool call. While an agent or background command you started is still running, wait for it and handle its result instead of ending the turn. Stop only at a real stop: the job is at needs-human, merged or closed, its PR waits at pr-open for a person, a step in these instructions says stop, or nothing can move without the user. This does not override the security floors, the confirmation a risky or destructive action needs, or any rule that holds work for a human.

## Run it every 5 minutes

`pickup setup --install` writes and loads this timer for you. The blocks below are what it writes.

Run `factory.py pickup --dry-run` first: it prints what it would launch and
changes nothing. Then install one timer per host.

- cron (Linux, WSL):

  ```
  */5 * * * * PATH=<launcher bin dir>:/usr/bin:/bin python3 <installed-skill>/runtime/factory.py pickup >> <home>/logs/pickup.log 2>&1
  ```

  WSL without systemd needs `service cron start`, plus the `/etc/wsl.conf`
  `[boot]` line.

- launchd (macOS), `~/Library/LaunchAgents/factory.pickup.plist` with
  `ProgramArguments` `/usr/bin/python3`, `<installed-skill>/runtime/factory.py`,
  `pickup`; `EnvironmentVariables` `PATH`; `StartInterval` 300;
  `StandardOutPath` and `StandardErrorPath` `<home>/logs/pickup.log`. Load with
  `launchctl bootstrap gui/$(id -u) <plist>`.

- Task Scheduler (Windows): wrapper `%USERPROFILE%\.config\factory-pickup.cmd`
  that sets `PATH` and runs
  `py -3 <installed-skill>\runtime\factory.py pickup >> <home>\logs\pickup.log 2>&1`,
  then
  `schtasks /Create /TN factory-pickup /SC MINUTE /MO 5 /TR "%USERPROFILE%\.config\factory-pickup.cmd" /F`.
  Python there has no zone data, so `py -3 -m pip install --user tzdata` lets
  reset times parse; without it limits default to 60 minutes.

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
  `factory.py stuck` recovers after `claim_stale_minutes`.
- The adapter is an argv command with `list` and `archive <session>` verbs and JSON output;
  a stdlib Paseo adapter ships in the bundle:
  `{"version": 1, "adapter": ["python3", "<installed-skill>/runtime/paseo_stalls.py"], "takeover_minutes": 30}`.
  It reports only idle, unarchived Claude limit stalls and re-checks a session before
  archiving it, so a session a human or the sweep has woken is kept.
  `archive <session> --force` archives without the stall re-check, for a moved job.
- `routes.md` order sets the fallback: pickup limits the stalled route and launches on the
  first route that is not limited, so put the route you want after a limit second.
- A host whose `max_inflight` is 0 takes over nothing. Dry-run prints
  `skip stall <session> <id>: host <host> paused (max in flight 0)`.

## Moving a job to another host

- The dashboard's move button replaces a job's `claim` and `paseo` with `move = {from, session, to, at}`. Only the dashboard, `pickup` and `factory.py control <job-id> move <host>|rerun --chat-file <file>` write `move`; `control` adds the chat record `via`, `quote` and `by`.
- A second move keeps each earlier holder in `move.earlier`. Those sessions stay fenced, and each earlier host's `pickup` still stops its worktree.
- The old session is fenced: its next `board` or `hold` exits 6 with `<id> was moved to <host>`. `hold <project>/<id> take` still claims the job.
- The old host's `pickup` stops the job's worktree processes first on every run, then sets `move.stopped`.
  - It stops them only when the project's pickup is on and lists that host. Otherwise the stop stays queued.
  - Each run prints `factory.py: pickup skips the stop of <id> <project>: <reason>` on stderr, and a dry run prints `skip stop <id> <project>: <reason>` on stdout instead.
  - No command or dashboard item clears a queued stop: `board` refuses a `move` patch with exit 2, and moving the job again keeps the stop queued.
  - To clear it, fix the printed reason on that host:
    - `pickup off` or `pickup does not list host <host>`: turn the project's pickup on or add that host in Settings: Pipeline or Projects.
    - `no checkout on this host`: add the project's checkout to that host's `paths.md`.
    - Any other reason, such as an `unsafe checkout`: fix what it names.
  - The next `pickup` run on that host then stops the job and sets `move.stopped`.
  - Only a string `move.stopped` counts as done. Pickup and the dashboard treat `null` or any other value as a stop still queued.
  - While that host is offline the stop waits on the board. Survivors are printed on stderr and retried next run.
- Under the same gate, when `stalls.json` is set and the job's Paseo session ran on this host's server, the old host's `pickup` archives it with the adapter's `archive <session> --force` first. While the gate skips the stop, the session stays unarchived too. A failed archive is printed on stderr and retried next run. It archives only a session whose `<home>/paseo-claims/<job-id>/<agent>` file shows this host recorded it claiming that job; otherwise it only stops the worktree and prints a "kept session" hint on stderr. Pickup never archives an agent id the adapter would refuse. It stops the worktree and prints a "skipped archive" hint on stderr instead.
- The "kept session" hint reads `factory.py: pickup kept session <agent> of <id>: no claim recorded on this host, archive it by hand if it still runs: paseo archive --force <agent>`. The "skipped archive" hint ends with the same command. Both put `--home <PASEO_HOME>` after `paseo` when `PASEO_HOME` is set. When `PASEO_HOME` holds an unprintable character, both leave `--home` out and put `(left out --home because PASEO_HOME has unprintable characters)` before the colon that starts the command, so the command still ends the line. Both leave the command off when the id starts with `-` or holds a character outside `A-Z`, `a-z`, `0-9`, `_` and `-`.
- To archive a kept session, run the `adapter` argv from `stalls.json` followed by `archive <agent> --force` on the host that printed the hint. With the Paseo adapter that is `python3 <installed-skill>/runtime/paseo_stalls.py archive <agent> --force`. It accepts only ids that match `[0-9a-fA-F][0-9a-fA-F-]{7,63}`, checks the session with `paseo inspect` first, and does nothing if it is already archived. If `paseo inspect` fails or prints bad JSON, it archives anyway.
- The adapter argv only archives kept sessions. A "skipped archive" id fails the adapter's id check, so run the printed `paseo archive --force <agent>` command instead. That command calls Paseo directly, with no id check and no already-archived check. `--force` archives the session even while it is working, so check first that its work is saved or no longer needed.
- The target host's `pickup` claims the job once, sets `move.claimed` and launches `/factory recover`. The usual gates apply: pickup on, host listed, not gated, the cap and a route.
- `factory.py stuck` leaves a move with no `claimed` to its target.

## Rerunning a stage

- The dashboard's Rerun stage item writes a self-move: `move` with `to` equal to `from`, the job's own host. It shows only for a claimed job at spec, build, review or approve that is not paused and has no `needs`.
- That host's `pickup` claims the rerun only after its own stop is recorded. While a stop from this host is still queued, the job waits.
- When the stop succeeds, the same run claims the job, sets `move.claimed` and launches `/factory recover`, which restarts the current stage in a fresh session. Pickup prints `stopped <id> <project>: rerun on <host>` before `launched ...`. If the stop fails, nothing is claimed and the next run retries both.
- The dashboard refuses a rerun when the host does not run pickup for the project, since nothing would restart the job. Use Move to another host instead.
- `factory.py control <job-id> rerun --chat-file <file>` writes the same self-move from a person's chat message, with the same checks.

## Prioritizing a job

- The dashboard's Prioritize item sets `priority = {at}` on a job at `idea`, `spec`, `build`, `review` or `approve`. Remove priority deletes it.
- Run batch now on a held idea sets `priority.batch`. Pickup starts it next, and while it is at `idea` and not paused, closing or waiting on a human, the project's other held ideas print `batches behind <id>`. Start alone sets `priority.alone`, and `batch` then lists no candidates and refuses riders.
- `pickup` claims prioritized ideas before older ones, across projects too. Gates, caps and routes still apply.
- The dashboard shows a prioritized job at the top of its column in idea, spec, build, review and approve.
- When the job's session on this host was claimed before `priority.at` and its Paseo agent is not closed, `pickup` sends the agent one fixed nudge with `paseo send --no-wait`, then sets `priority.nudged`. A failed send is printed on stderr and retried next run.
- Only the dashboard, `pickup` and `factory.py control` write `priority`. `board` refuses a `priority` patch with exit 2.

## Stopping the whole factory

- The dashboard's Stop factory form in Settings > Hosts sets the global stop flag, and the banner's Resume factory clears it. Only a signed-in person can clear it.
- An agent that sees an incident needing every host stopped runs `factory.py halt set --reason <text>`. `factory.py halt status` prints the record.
- While the flag is set, `pickup` prints `factory.py: pickup launches nothing; factory stopped by ...` on stderr and runs no pass: no launch, takeover, triage or Tracer pass. Its dry run prints `skip all: ...`.
- `stuck` exits 3, so the resume schedule resumes nothing.
- Sessions already running keep running. A stop blocks new starts only.
- A Worker without the `/api/halt` route answers 404, which means not stopped.

## Nudging an idle job

- `pickup` watches each job at `spec`, `build`, `review` or `approve` that this device claimed and whose Paseo agent runs on this host's server. It skips jobs with an ask, a pause, a queued close or a batch lead.
- The quiet signature is the agent, the worktree's `HEAD`, a hash of `git status --porcelain` and the job's `updated`. It lives in `<home>/idle-watch.json`, never on the board. Any commit, file edit or board write starts the window again.
- Only an `idle` agent counts. A `running` agent keeps its entry and is skipped that run.
- After 20 quiet minutes, `pickup` sends the agent one fixed nudge with `paseo send --no-wait` and prints `nudged idle <id> <project>: agent <agent>`. `--dry-run` prints `would nudge idle ...` and writes nothing. A failed send prints `factory.py: pickup could not nudge idle <id>: paseo send failed` on stderr and retries next run.
- The nudge reads: "Factory job <id> shows no commit, file or board change for 20 minutes while this session sits idle. If work on it is still owed, continue with the next step now. If you are waiting on an agent or command that is still running, run factory.py hold for the job with your --session tag, then keep waiting."
- After 20 more quiet minutes, `pickup` moves the job to `needs-human`, releases the claim and prints `needs-human <id> <project>: idle: no progress 20 minutes after a nudge to agent <agent>`. A board change since the read wins, so a fresh `hold` keeps the job.
- `--dry-run` prints `would move needs-human <id> <project>: <note>` for that move and writes neither the board nor `idle-watch.json`.
- When paseo does not answer, `pickup` prints `factory.py: pickup skips idle checks: paseo did not answer` on stderr.

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
| bad config | `factory.py: pickup ignores job_token_cap in {config}: must be a whole number of at least 1` |
| scan error | `factory.py: pickup skips the token cap of {id}: scan failed ({Type})` |
| stop failed | `factory.py: pickup could not stop agent {agent} of {id}: paseo stop failed` |

Numbers use `,` thousands separators, and `pct` is `used * 100 // cap`. The last three lines go to stderr. `--dry-run` writes nothing and never stops an agent.

## Pausing a job

- The dashboard's Pause pickup item sets `paused = {at}` on a job with a project that is not merged or closed. Resume pickup deletes it.
- `pickup` does not claim, take over or nudge a paused job, and its dry run prints `skip <id> <project>: paused` or `skip stall <agent> <id>: paused`. `stuck` does not resume it.
- A session already running on a paused job keeps running. Pausing stops new starts only.
- Only the dashboard and `factory.py control` write `paused`. `board` refuses a `paused` patch with exit 2.

## Closing a job

- The dashboard's Close job item sets `close = {reason, at}` on a job with a project that is past `idea` and not merged or closed. The reason is one line of 1 to 200 characters, with no control characters and no U+0085, U+2028 or U+2029.
- `pickup` runs its close pass right after the move stops. It writes `decision.md` with first line `CLOSED: <reason>`, keeping earlier text under `## Earlier decision`, and only then sets the stage to `closed`, the note to the reason and `close.closed`. Its dry run prints `would close <id> <project>`, or `skip close <id> <project>: claimed by <device> session <tag>` for a job with a live claim.
- When the job has a `pr` in the project's `repo`, the close pass runs `gh pr view`, and for an open PR `gh pr close --comment`, before it writes `decision.md`. It reports a merged or closed PR as such before it checks the PR's head branch or the job's branch. It closes an open PR only when the job has a branch, and the PR's head branch equals it and lives in the project's repo, not a fork. The comment opens with the agent disclosure marker and links `<api_url>/#<id>/decision`. When gh is missing, fails or times out, the job still closes and `decision.md` says why the PR stayed open.
- When the closed job has a stacked rider with an open PR, the close pass closes the rider's open PR with a disclosed comment after the board write, and rewrites every such rider's note in a single board write after the last PR closes.
- A session on a closing job is refused at its next `board` or `hold` call with exit 6. `board` first releases that session's own claim, so the next pickup can close the job.
- Only the dashboard, `pickup` and `factory.py control` write `close`. `board` refuses a `close` patch with exit 2.
- The next pickup run on a host that still holds a closed job's worktree or `factory/<id>` branch runs `cleanup` on it when the project's pickup is on and lists that host, once per host, and records `close.cleanup.<host> = {at, left}`. It does the same for a `merged` job with `github.merged`, which the PR sync sets, and records `github.cleanup.<host> = {at, left}`. It keeps a dirty worktree, and a branch that is not merged unless origin holds the same commit and no worktree has it checked out. It deletes that branch with `git update-ref -d` guarded by the branch's current commit, never `git branch -D`. For a `merged` job it also deletes a branch whose tip `github.head` contains, guarded by the branch's current commit. The kept branch's `left` entry ends with `and origin could not be reached`, `and origin has no such branch` or `and origin is at a different commit`. Its dry run prints `would clean up <id> <project>`. The close pass's `decision.md` names each host the board records as holding the job, from `claim` and every `move` holder, that the project's pickup is off for or does not list, and says which of the two it is. When it cannot read the pickup hosts, it names every recorded holder and says why it could not read them.

## Syncing PR state

- `pickup` runs its PR sync pass right after the move stops and before the close pass. It never starts a launcher or calls a model.
- A job is a candidate when its stage is `pr-open` or `needs-human`, its `pr` is a `https://github.com/<owner>/<name>/pull/<n>` URL with `<n>` at most 9 digits, no close is queued, it is not a batched rider (riders follow their lead), no other session holds a live claim, the project file's `repo` equals the URL's owner and name (case-insensitive), and the project's pickup is on and lists this host.
- Candidates are grouped by repo. Each repo gets one `gh api graphql` call that reads `state` for up to 100 pull requests in board order. With more, it prints `factory.py: pickup syncs the first 100 of <n> pull requests in <repo>` on stderr.
- When the call fails, it prints `factory.py: pickup skips the PR sync for <repo>: <why>` on stderr and the run goes on: gh is not installed, timed out after 60 seconds, exited non-zero (check `gh auth status`), or returned no repository (check the project file's `repo` and `gh auth status`). A non-zero exit whose answer still holds `data.repository` syncs the pull requests that resolved.
- `MERGED`: the pass's board write moves the job to `merged`, sets the note to `merged on GitHub` and `github = {merged, head}`, with `github.head` the PR's `headRefOid`, and removes `needs`, `automerge`, `babysit` and `claim`. It prints `synced <id> <project>: merged on GitHub`. The close cleanup then runs `cleanup` on each host that holds the worktree or branch, as for a closed job.
- `CLOSED`: the pass's board write queues `close = {reason: "PR closed on GitHub", at, via: "github"}` and removes `babysit`. It prints `synced <id> <project>: PR closed on GitHub`. The close pass in the same run closes the job without calling `gh pr`, and `decision.md` starts `CLOSED: PR closed on GitHub` and says GitHub reported the close.
- The pass puts every row it syncs in one run into a single board write. That write re-checks every candidate condition and the same `pr` for each row, so a second host that read the same state writes nothing for it.
- An open PR, or one GitHub does not return, changes nothing and prints nothing.
- Its dry run makes the same read and writes nothing. It prints `would sync <id> <project>: merged on GitHub` or `would sync <id> <project>: PR closed on GitHub`, and `skip sync <id> <project>: claimed by <device> session <tag>` or `skip sync <id> <project>: <reason>` for a live claim or a project whose pickup is off or does not list this host.
- Only pickup writes `github` and `close.via = "github"`. `board` refuses a `github` patch with exit 2.
- A synced move does not mirror Tracer, archive a Paseo workspace or post a stage-change chat line. Agents still own those.

## Prebuilt dependencies

`factory.py worktree` installs the folders a device-local `<home>/prebuild.json`
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
| install exits non-zero, cannot start, or runs past 600 s | `factory.py: prebuild of <dir> failed (<why>); log: <log>` |
| success | `prebuilt <dir> with pnpm install --frozen-lockfile` |

A bad file prints `factory.py: cannot read <path>: use {"version": 1, "prebuild":
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

A stored Paseo or Claude schedule prompt does not pick up ideas. Only the host timer runs pickup.
