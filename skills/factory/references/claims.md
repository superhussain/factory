# Claims

- Run `hold` before spawning each agent and after each one finishes, so a long
  build keeps the claim fresh. A claim goes stale after `claim_stale_minutes` in
  `config.md` (default 120). When this session refreshed its own claim in the last 10 minutes and nothing else changes, including no unlinked agents row from this session, `hold` skips the board write and prints the same lines.
- `hold` prints `elapsed <N>s`, the seconds since the job's first claim. Time matters: a correct result sooner is better. Use it to pace the job and run independent agents in parallel. It never cuts a review round, skips a reviewer, shortens WAIT FOR ALL, or relaxes a security floor.
- Moving a job to `needs-human`, `pr-open`, `merged`, or `closed` releases the claim.
- On `babysit requested` from `hold`, finish the board write in progress, then follow
  [babysit](commands/babysit.md) from Step zero in SKILL.md with the named tag.
  A session already running babysit.md is the babysitter: it ignores the line and goes
  on with its loop.
- Claims prevent accidents between sessions, not abuse. Anyone with the Access
  token can take one.

Exit 6 also protects local project edits, missing project baselines, and unresolved
project `.remote` copies. For jobs, it means another session has the job, R2 or
origin moved, or a job path is unsafe to inspect (a symlink or junction, or a
listing the runtime cannot read).
Its message says which. Never retry, `take`, force, or push blind unless the user
says to.

| Command | Message | Next step |
| --- | --- | --- |
| `hold`, `board` | `<id> is held by <device> session <tag>` | Tell the user who holds the job and when it was last seen, then stop. Take over only when the user says so, with `hold <project>/<id> take`. |
| `hold`, `board` | `<id> was moved to <host>` | Stop. Do not retry or take it. |
| `hold`, `pull`, `push` | `conflict in <files>` | Settle each `.remote` as [Where things live](../SKILL.md#where-things-live) says. Else set `needs-human` with the message as note; `hold` holds it. |
| `sync` | `conflict in <project>/<id>/<file>, ...` | Tell the user which jobs conflict. They don't block a new job. |
| `push`, `pull`, `sync`, `hold`, `handoff` | `<path> is a symlink or junction...` or `cannot inspect job paths under <folder>...` | Report the stop and do nothing to the paths: never remove links, change permissions, or `take` the job automatically. Mark the job `needs-human` with the message as the note only if this session owns the claim (`hold` may have already claimed it). Links, their targets, and local edits are left untouched. |
| `worktree` | `<wt> is N ahead and M behind` | Set the job to `needs-human` with the message as the note, and stop. |
| project `push`, `pull`, `where`, `init` | project file messages | See the table in [project settings](project-settings.md). |

`worktree` exits 5 when origin can't be reached. Report it and stop.
