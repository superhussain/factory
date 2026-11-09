# Automatic pickup

`factory.py pickup` is a model-free command that a host timer runs every 5
minutes. It claims runnable ideas for the projects whose pipeline lists this
host and starts one session per idea through `launchers.json`.

## What it does

Each run, in order:

- Reads the board and lists the `stage: idea` jobs whose project has a checkout
  in `paths.md` on this host.
- Also lists `build` jobs with note `spec approved in Tracer`, which Tracer scope
  approval released, and launches `/factory recover <id>` for them instead of
  `/factory idea <id>`. They follow the same skips, claim and caps as ideas.
- Skips ideas that are gated, held by a live claim, or waiting on a human, and
  projects whose effective pipeline has pickup off, does not list this host, or
  cannot be read. Each skip reason is printed once and the run goes on.
- Deny notes: before ideas, launches at most one `/factory deny-notes <id>` session
  for a `closed` job denied from the backlog whose `backlog.notes` are unread (no
  `backlog.notes_read`). The session gets the notes from `factory.py deny-notes <id>`
  inside a `backlog-notes` envelope, queues the follow-ups they ask for with
  `backlog add`, then runs `deny-notes <id> done`. Notes that fail admission are
  skipped. This launch does not count against `--limit` or the in-flight cap.
- Picks the first route in `routes.md` that is not marked limited, then resolves
  the launcher for it. A config error exits 2 before anything is claimed.
- Claims one idea at a time — the board compare-and-swap stops two hosts taking
  one idea — launches it detached with its output in a private log, and waits up
  to 30 seconds. Still running or exit 0 counts as launched.
- Stops after `--limit` launches (default 1) or when a full pass claims nothing.
  It is silent when there is nothing to do, and it never calls a model.
- Caps in-flight work. On a host with a registry record, the cap is the host's
  `max_inflight` from Settings, Hosts, counted over this host's fresh claims in
  every project. A `max_inflight` of 0 pauses pickup on that host; jobs already
  in flight keep running. Jobs waiting on a human never count. `--max-inflight N`
  lowers it further. A host with no record caps each project at `--max-inflight`
  (default 1), as before.
- On a host paired with `factory.py host pair`, sends the host's status to
  the Worker after every run but `--dry-run`, failed runs included. It carries the OS,
  scheduler, run time, bundle stamp, doctor result, routes and checkout names.
  It is silent, calls no model, and never changes the run's output or exit code.

Pickup skips an idea held by a live claim. The board `needs` field is only for
input from a human: a coordinator that wants to park an idea must hold it with a
claim (`hold`), never by setting `needs`.

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
  Pass `{prompt}` as one whole argument, as the
  examples below do. Its line breaks would split a shell command string or a
  Windows `.cmd` line. Unknown braces stay literal. The launcher also
  gets `FACTORY_JOB_ID`, `FACTORY_PROJECT`, `FACTORY_REPO_PATH`,
  `FACTORY_SESSION`, `FACTORY_PROVIDER`, `FACTORY_MODEL`, `FACTORY_EFFORT` and
  `FACTORY_PROMPT` in its environment, and runs in the project's checkout. Idea text never reaches a launcher.

Work projects list host `work-laptop`, personal projects list `home-pc`, and a
shared project lists both.

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
  ["claude", "-p", "{prompt}", "--model", "{model}", "--effort", "{effort}"]
  ```

  Tool permissions come from the user's Claude Code settings. Never add
  `--dangerously-skip-permissions` or another auto-approval flag.

Core needs neither. Any program that accepts these values works.

## Standing instruction for unattended runs

Every prompt `pickup` launches, and every `/factory recover` the resume schedule
dispatches, ends with this paragraph after a blank line. End a headless prompt
started by hand the same way. It keeps a session working where it would
otherwise end its turn on a status note, since nothing wakes an unattended
session. Keep it out of interactive use.

A standing instruction from the user, the person you are working for. It is about how your turns end. This factory session runs unattended: a message with no tool call in it ends your turn, nothing wakes you afterwards, and the job stalls until a sweep finds it. The user has seen you end turns in four ways while factory work was still owed, and does not want any of them. One: a summary that closes by announcing the next step, such as reviewers running and verdicts to be recorded as they land, with no tool call, so the next thing never starts. Two: an offer to carry on unless the user would prefer otherwise. Three: a list of decisions for the user when, by your own account, none of them blocks the rest of the work. Four: deciding that this is a good place to report, because the turn has been long or a stage is done. Status notes and recommendations are welcome, but put them in the same message as your next tool call. While an agent or background command you started is still running, wait for it and handle its result instead of ending the turn. Stop only at a real stop: the job is at needs-human, merged or closed, its PR waits at pr-open for a person, a step in these instructions says stop, or nothing can move without the user. This does not override the security floors, the confirmation a risky or destructive action needs, or any rule that holds work for a human.

## Run it every 5 minutes

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

## Moving a job to another host

- The dashboard's move button replaces a job's `claim` and `paseo` with `move = {from, session, to, at}`. Only the dashboard and `pickup` write `move`.
- The old session is fenced: its next `board` or `hold` exits 6 with `<id> was moved to <host> from the dashboard`. `hold <project>/<id> take` still claims the job.
- The old host's `pickup` stops the job's worktree processes first on every run, whatever the project's pickup settings, then sets `move.stopped`. While that host is offline the stop waits on the board. Survivors are printed on stderr and retried next run.
- When stalls.json is set and the job's Paseo session ran on this host's server, the old host's pickup archives it with the adapter's archive <session> --force first. A failed archive is printed on stderr and retried next run.
- The target host's `pickup` claims the job once, sets `move.claimed` and launches `/factory recover`. The usual gates apply: pickup on, host listed, not gated, the cap and a route.
- `factory.py stuck` leaves a move with no `claimed` to its target.

## Prioritizing a job

- The dashboard's Prioritize item sets `priority = {at}` on a job at `idea`, `spec`, `build`, `review` or `approve`.
- `pickup` claims prioritized ideas before older ones, across projects too. Gates, caps and routes still apply.
- When the job's session on this host was claimed before `priority.at` and its Paseo agent is not closed, `pickup` sends the agent one fixed nudge with `paseo send --no-wait`, then sets `priority.nudged`. A failed send is printed on stderr and retried next run.
- Only the dashboard and `pickup` write `priority`. `board` refuses a `priority` patch with exit 2.

## Pausing a job

- The dashboard's Pause pickup item sets `paused = {at}` on a job with a project that is not merged or closed. Resume pickup deletes it.
- `pickup` does not claim, take over or nudge a paused job, and its dry run prints `skip <id> <project>: paused` or `skip stall <agent> <id>: paused`. `stuck` does not resume it.
- A session already running on a paused job keeps running. Pausing stops new starts only.
- Only the dashboard writes `paused`. `board` refuses a `paused` patch with exit 2.

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
