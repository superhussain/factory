# `/factory build [<slug>] [force]`

Checks the build queue now, as Builds describes, and triggers the batched build if
it is due. With `force`, triggers it even while jobs are in flight.

## Builds

1. Run `python3 "$RUNTIME" builds "$SLUG"`. It prints the queued job ids, the jobs
   still in flight (at spec, build, review, or approve, or an unescalated PR not yet
   merged; ideas don't count), the jobs whose build is running, and `due`. A job not
   updated within `claim_stale_minutes` doesn't count as in flight, so a dead session
   can't hold the queue forever. `builds` lists those as `stale`.
2. Trigger a build only when `due` is true: something is queued, nothing is in
   flight, and no build is running. Run the commands in the project file's Device
   builds section, from `$BASE`.
3. Patch every queued job to `{"build": {"state": "running", "runs": [<run URLs>]}}`
   and send one line naming the runs and the jobs that rode them.
4. When the runs finish, set each job's state to `done` or `failed`. On `done`, send
   the user the install link and one checklist of every job's device checks from its
   `decision.md`. A failed device check becomes a `rework-of` job, not a reopened
   one. On `failed`, read the log and fix the build as a new job, or move to
   `needs-human` if the cause isn't in the repo.

Check the queue after every merge, after `next` finds nothing to run, and at the end
of any command that leaves no job in flight.
