# Optional explorer schedule prompt

Before enabling or refreshing this stored prompt, the user materializes
`<installed-skill>`, `<python-launcher>`, `<project-slug>` and this host's
`routes.md`. Stop before exploring if any placeholder remains unresolved. This is a
prompt template, not an executable or an automatically enabled schedule.

1. Pick a route. Run:

```text
<python-launcher> <installed-skill>/runtime/factory.py routes pick
```

On any nonzero exit, name the exit code and first stderr line, then stop. On exit 0,
read `provider` and `model`. If `provider` is neither `claude` nor starts with
`claude-`, reply `no claude route` and stop.

2. Snapshot. Run:

```text
<python-launcher> <installed-skill>/runtime/factory.py explore <project-slug>
```

On exit 3, reply `explorer off` with the first stderr line and stop. On any other
nonzero exit, name the exit code and stop. On exit 0, read `project`, `run`,
`commit`, `checkout` and `out` from the printed JSON.

3. Explore. Dispatch one `factory-explorer` subagent with the picked model, giving it
the printed `checkout`, `project`, `run` and `commit`.

4. Admit. Unless the reply is `{"none": true}`, write it byte for byte to `out`, then
run:

```text
<python-launcher> <installed-skill>/runtime/factory.py suggest admit <out>
```

Report its JSON, or its exit code and first stderr line.

5. Clean up. Once step 2 succeeded, always run:

```text
<python-launcher> <installed-skill>/runtime/factory.py explore <project-slug> done <run>
```

Do no factory work yourself. Read no other files and run no other commands except
these. Never retry.
Never run `suggest approve` or any `autonomy` command; a human approves each card.

A standing instruction from the user, the person you are working for. It is about how your turns end. This factory session runs unattended: a message with no tool call in it ends your turn, nothing wakes you afterwards, and the job stalls until a sweep finds it. The user has seen you end turns in four ways while factory work was still owed, and does not want any of them. One: a summary that closes by announcing the next step, such as reviewers running and verdicts to be recorded as they land, with no tool call, so the next thing never starts. Two: an offer to carry on unless the user would prefer otherwise. Three: a list of decisions for the user when, by your own account, none of them blocks the rest of the work. Four: deciding that this is a good place to report, because the turn has been long or a stage is done. Status notes and recommendations are welcome, but put them in the same message as your next tool call. While an agent or background command you started is still running, wait for it and handle its result instead of ending the turn. Stop only at a real stop: the job is at needs-human, merged or closed, its PR waits at pr-open for a person, a step in these instructions says stop, or nothing can move without the user. This does not override the security floors, the confirmation a risky or destructive action needs, or any rule that holds work for a human.
