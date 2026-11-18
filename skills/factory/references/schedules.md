# Schedules

Schedules are optional prompt templates, not executables or automatically
installed automation. Materialize `<installed-skill>`, `<python-launcher>` and an
available user-configured dispatch route before creating or refreshing one.
Stop before claiming if any value is unresolved. See
[integrations](integrations.md) for setup and explicit refresh by the
existing schedule ID. Reinstalling the skill never refreshes stored prompts.
A host acts only on projects with local checkouts in `paths.md`. `no ideas`
means no runnable ideas here, not an empty board. Unassigned ideas stay put.
Ideas are picked up by `factory.py pickup`, a model-free host timer; see [references/pickup.md](pickup.md). `schedules/resume.md`, `schedules/explore.md` and `schedules/retro.md` are the shipped prompt templates.
A failed dispatch returns its idea with `unclaim`.

## Daily retro push

`schedules/retro.md` aggregates the last 3 days of every host's digest, so each host
pushes its own daily with a model-free timer, weekends included. Every host that runs
Claude Code sessions, the aggregating host included, installs it.

Pickup also pushes this host's retro digest on its first run of each local date that
`<home>/retro-push.json` does not record; a failed push prints one warning and the
run goes on. On the host that keeps `<home>/retro-schedule-prompt.md`, pickup
catches up a missed weekday retro: from 09:30 local, when no retro run folder
carries today's date and `<home>/retro-catchup.json` does not record today, it
launches that prompt once through the first route's launcher as `retro-<YYYYMMDD>`.
While no route or checkout is available, it prints why it waits once per local date
and reason, recorded in `<home>/retro-catchup-wait.json`.

- cron (Linux, WSL):

  ```
  30 8 * * * python3 <installed-skill>/runtime/factory.py retro push >> <home>/logs/retro-push.log 2>&1
  ```

- launchd (macOS), `~/Library/LaunchAgents/factory.retro-push.plist` with
  `ProgramArguments` `/usr/bin/python3`, `<installed-skill>/runtime/factory.py`,
  `retro`, `push`; `StartCalendarInterval` an array of three dicts,
  `Hour` 6, 7 and 8, each `Minute` 30; `StandardOutPath` and `StandardErrorPath`
  `<home>/logs/retro-push.log`. Load with `launchctl bootstrap gui/$(id -u) <plist>`.
  A sleeping Mac runs a missed calendar time once, at its next wake, so one 08:30
  entry can land after the 09:00 aggregate. Each push replaces that day's file.

- Task Scheduler (Windows): wrapper `%USERPROFILE%\.config\factory-retro-push.cmd`
  that runs
  `py -3 <installed-skill>\runtime\factory.py retro push >> <home>\logs\retro-push.log 2>&1`,
  then
  `schtasks /Create /TN factory-retro-push /SC DAILY /ST 08:30 /TR "%USERPROFILE%\.config\factory-retro-push.cmd" /F`.
