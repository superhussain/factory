# `/factory handoff <job-id>`

Checkpoint and release the current job using
[execution and handoff](../execution.md#handoff). Keep its stage and round.
Do not hand off while workers still run or human approval is pending.
There is no CLI note argument: write the reason for transfer into
`<job folder>/handoff.md` before invoking the runtime.
