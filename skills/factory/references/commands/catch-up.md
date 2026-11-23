# `/factory catch-up`

Brings this host up to date with the factory repository's main. Run
`<installed-skill>/runtime/factory catch-up` (`factory.exe` on Windows) and
report each line it prints.

One run fetches the factory checkout's origin, backs up the installed skill, this
host's timers and the home config to `<home>/backups/catch-up-<stamp>/`, installs
the skill with origin's own `tools/install.py` from a temporary detached worktree,
lets the new bundle rewrite the pickup timer, and checks the Worker's `deploy-main`
run. The checkout is fast-forwarded only when it is clean and on its base branch.
A run with nothing new writes nothing and ends `catch-up: nothing new`.

Exit 0 means done or nothing new, 1 a step failed, 2 bad usage or a run from the
checkout itself, 5 the fetch failed.

If the harness blocks the crontab, launchctl or `gh run rerun` step, do not work
around it. Hand the person the exact command to run themselves.
