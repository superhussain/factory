# `/factory resume <job-id>`

Run step zero for an existing job, using `hold <project>/<id> released`, not `take`:
`released` refuses another owner's claim even when stale, while `take` can seize a
live claim. Read `handoff.md` if present,
inspect the worktree and board, and continue at the recorded stage using the
[resume table](../execution.md#resume). Do not claim a new id, write a
rework file, reset review caps, or approve it just because the runtime changed.
