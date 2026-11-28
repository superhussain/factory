# `/factory catch-up`

Brings this host up to date with the factory repository's main. Run
`<installed-skill>/runtime/factory catch-up` (`factory.exe` on Windows) and
report each line it prints.

One run fetches the factory checkout's origin, backs up the installed skill, this
host's timers and the home config to `<home>/backups/catch-up-<stamp>/`, builds
origin's binary in a temporary detached worktree and installs the skill with it,
lets the new bundle rewrite the pickup timer, and checks the Worker's `deploy-main`
run. Color codes in gh's output, from a forced tty such as GH_FORCE_TTY or
CLICOLOR_FORCE, are stripped before the run JSON is read. A factory timer (crontab line, `factory.*.plist`, `factory-*.service` or
`factory-*.cmd`) or a `stalls.json` adapter that still runs `runtime/factory.py` or
`runtime/paseo_stalls.py` is rewritten to run the binary, with one `timers:` line each. The checkout is fast-forwarded only when it is clean and on its base branch.
A run with nothing new writes nothing and ends `catch-up: nothing new`.

On macOS (launchd) and cron hosts the daemon runs catch-up on its own. After a pass that launched nothing, it starts `factory daemon catch-up` when the installed bundle has lacked a merge on origin's base for 30 minutes and no `factory <id>` or `babysit <id>` agent runs. It first records that origin commit in `<home>/logs/daemon/catch-up.json`, so each commit gets one try; delete the file to allow another. The run is detached, so catch-up's own daemon restart does not end it. Its output goes to the day log, and it appends `catch-up exited <code>: <last line>` to `<home>/logs/daemon.log`. On systemd and Windows hosts, run catch-up by hand after a merge to main.

Exit 0 means done or nothing new, 1 a step failed, 2 bad usage or a run from the
checkout itself, 5 the fetch failed.

If the harness blocks the crontab, launchctl or `gh run rerun` step, do not work
around it. Hand the person the exact command to run themselves.
