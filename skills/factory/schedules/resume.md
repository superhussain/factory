# Optional resume schedule prompt

Before enabling or refreshing this stored prompt, the user materializes
`<installed-skill>`, `<python-launcher>` and an available configured dispatch
route. Stop before claiming if any placeholder or route remains unresolved.
This is a prompt template, not an executable or an automatically enabled schedule.

Run exactly one command:

```text
<python-launcher> <installed-skill>/runtime/factory.py stuck --limit 2
```

Whatever the exit, append every stderr line beginning `needs-human` to the reply.
Exit 3 means reply `resume: nothing stuck` and stop. Any other nonzero exit means
reply `resume: stuck failed: <first stderr line>` and stop. Never retry `stuck`.

On exit 0, read the JSON array of `id,project,repo_path,session,stage`. Dispatch
at most two orchestrator sessions through the configured available route, in
each printed repo_path, with `/factory recover <id> --session <session>`, then a
blank line, then the standing instruction that ends this prompt, byte for byte.
Use session tags exactly as printed. Note `failed <id>` for a failed dispatch,
then continue to the next item without retrying. Reply with one line
`resume: spawned <id>, <id>` and stop. Do no factory work yourself, read no other
files and run no commands except the one claim call and at most two dispatches.
Never choose a paid fallback.

A standing instruction from the user, the person you are working for. It is about how your turns end. This factory session runs unattended: a message with no tool call in it ends your turn, nothing wakes you afterwards, and the job stalls until a sweep finds it. The user has seen you end turns in four ways while factory work was still owed, and does not want any of them. One: a summary that closes by announcing the next step, such as reviewers running and verdicts to be recorded as they land, with no tool call, so the next thing never starts. Two: an offer to carry on unless the user would prefer otherwise. Three: a list of decisions for the user when, by your own account, none of them blocks the rest of the work. Four: deciding that this is a good place to report, because the turn has been long or a stage is done. Status notes and recommendations are welcome, but put them in the same message as your next tool call. While an agent or background command you started is still running, wait for it and handle its result instead of ending the turn. Stop only at a real stop: the job is at needs-human, merged or closed, its PR waits at pr-open for a person, a step in these instructions says stop, or nothing can move without the user. This does not override the security floors, the confirmation a risky or destructive action needs, or any rule that holds work for a human.
