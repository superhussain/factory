# Optional weekday retro schedule prompt

Before enabling or refreshing this stored prompt, the user materializes
`<installed-skill>` and `<python-launcher>`. Enable it Monday to Friday at 09:00 on
one host only. Stop before running anything if any placeholder remains unresolved.
This is a prompt template, not an executable or an automatically enabled schedule.

1. Pick a route. Run:

```text
<python-launcher> <installed-skill>/runtime/factory.py routes pick
```

On any nonzero exit, name the exit code and first stderr line, then stop. On exit 0,
read `provider` and `model`. If `provider` is neither `claude` nor starts with
`claude-`, reply `no claude route` and stop.

2. Check autonomy. Run:

```text
<python-launcher> <installed-skill>/runtime/factory.py autonomy status
```

If `paused` is not `false`, reply `retro: autonomy paused` and stop.

3. Push this host's digest. Run:

```text
<python-launcher> <installed-skill>/runtime/factory.py retro push
```

On a nonzero exit, note the exit code and continue.

4. Aggregate. Run:

```text
<python-launcher> <installed-skill>/runtime/factory.py retro aggregate --days 3
```

On exit 3, reply `retro: no digests` with the first stderr line and stop. On any
other nonzero exit, name the exit code and stop. On exit 0, read `run`, `aggregate`,
`digest`, `transcripts`, `out`, `findings` and `hosts` from the printed JSON, and `host`
from the `aggregate` file.

5. Retro. Dispatch one `factory-retro` subagent with the picked model, giving it
`run`, `aggregate`, `digest`, `transcripts` and `host`.

6. Write its reply byte for byte to `out`.

7. Admit. Run:

```text
<python-launcher> <installed-skill>/runtime/factory.py retro admit <run>
```

Report its output and the aggregated `hosts`, and name any expected host missing
from them. On exit 3 or 2, still report its output; name `findings` only if admit
printed a wrote line, otherwise report no findings file. On exit 0, report the
`findings` path.

Do no factory work yourself. Run no other commands. Never retry.
Never run `suggest approve`, decide a backlog line, or run any `autonomy` command but `status`.

A standing instruction from the user, the person you are working for. It is about how your turns end. This factory session runs unattended: a message with no tool call in it ends your turn, nothing wakes you afterwards, and the job stalls until a sweep finds it. The user has seen you end turns in four ways while factory work was still owed, and does not want any of them. One: a summary that closes by announcing the next step, such as reviewers running and verdicts to be recorded as they land, with no tool call, so the next thing never starts. Two: an offer to carry on unless the user would prefer otherwise. Three: a list of decisions for the user when, by your own account, none of them blocks the rest of the work. Four: deciding that this is a good place to report, because the turn has been long or a stage is done. Status notes and recommendations are welcome, but put them in the same message as your next tool call. While an agent or background command you started is still running, wait for it and handle its result instead of ending the turn. Stop only at a real stop: the job is at needs-human, merged or closed, its PR waits at pr-open for a person, a step in these instructions says stop, or nothing can move without the user. This does not override the security floors, the confirmation a risky or destructive action needs, or any rule that holds work for a human.
