# Dashboard ideas and backlog decisions

A backlog card opens a decision page in the drawer that shows the full item.
Approve adds an `idea` job that pickup starts. Deny adds a `closed` job with the note
`denied from the backlog`. Both carry a `capture_id`.
Rework needs notes. It adds an `idea` job with the note `rework requested from the backlog`.
Its pickup rewrites the line through `factory.py backlog rework`, and the new line comes
back to Backlog with `(rework-of <id>)`. The decision page lists each earlier wording and
its notes by following those links.
When the item has context, the page shows the reasons, a Recommended badge and the agent's extra questions. Approve and Deny need every question answered. Answers go to `backlog.answers`, and on Approve also to the idea.
`backlog decide --chat` makes the same writes from a person's chat message and adds `backlog.via`, `quote` and `by`.

`idea` is the first column. the user adds one from the dashboard's New idea button,
which writes `id`, `title` (the first line), `idea`, `stage`, and `updated`, plus
`project` only when selected. Missing, null, or empty `project` means unassigned.
Save without a project even on an empty factory. Register a project separately
with `/factory init` in its checkout, refresh the page, then use `Assign project`
in the idea drawer. Assignment keeps the id and text and does not start the job.
An idea carries no claim, branch, or files, and `builds` doesn't count it
as in flight. A pickup keeps its id and moves it to `spec`. Discarding it from the
dashboard moves it to `closed` with the note `discarded from the dashboard`.

For optional-project ideas, redeploy the Worker that bundles the dashboard first,
then reinstall the skill on devices to refresh the runtime and promotion guard.
Existing assigned jobs need no data migration. Existing schedules need no prompt
migration; their command and JSON output contract are unchanged.

## Stage-change deep links

| Event | Link to |
| --- | --- |
| Spec written | `#<id>/spec` |
| Build done (first build, fix round, rework) | `#<id>/build` |
| Waiting on his answers (`needs.md` written) | `#<id>/needs` |
| Review round N done | the first `round-N/review-<r>` with CHANGES, else `round-N/review-security` |
| PR open, merged, or escalated | `#<id>/decision`, plus the PR URL |
| Closed without shipping | `#<id>/decision` |
| Batched build triggered or landed | the run URLs and the jobs that rode it |
| needs-human | the file that explains the hold: the latest CHANGES review at the round cap, or the failed agent's file. Use `#<id>` plus the note when only the note says why (a git conflict, say). |
