# Dashboard ideas and backlog decisions

A backlog card opens a decision page in the drawer that shows the full item.
Approve adds an `idea` job that pickup starts. Deny adds a `closed` job with the note
`denied from the backlog`. Both carry a `capture_id`.
Rework needs notes. It adds an `idea` job with the note `rework requested from the backlog`.
Its pickup rewrites the line through `factory.py backlog rework`, and the new line comes
back to Backlog with `(rework-of <id>)`. The decision page lists each earlier wording and
its notes by following those links.
When the item has context, the page shows the reasons, a Recommended badge and the agent's extra questions. Approve and Deny need every question answered. Answers go to `backlog.answers`, and on Approve also to the idea.

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
