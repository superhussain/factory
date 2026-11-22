# Dashboard ideas and backlog decisions

A backlog card opens a decision page in the drawer that shows the full item.
Approve adds an `idea` job that pickup starts. Deny adds a `closed` job with the note
`denied from the backlog`. Both carry a `capture_id`.
Rework needs notes. It adds an `idea` job with the note `rework requested from the backlog`.
Its pickup rewrites the line through `factory.py backlog rework`, and the new line comes
back to Backlog with `(rework-of <id>)`. The decision page lists each earlier wording and
its notes by following those links.
When the item has context, the page shows the reasons, a Recommended badge and the agent's extra questions. Approve and Deny need every question answered. Answers go to `backlog.answers`, and on Approve also to the idea.
`backlog decide --chat-file` makes the same writes from a person's chat message and adds `backlog.via`, `quote` and `by`.

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

## Babysit PR

The drawer's top bar and job actions menu show Babysit PR on a `pr-open` job with a
valid PR that is not a batch rider, paused, closing or already babysat. After a
confirm it sets `babysit` and `priority` to `{at}` (an earlier `priority` stays),
deletes `needs` and `escalated`, and sets the note `babysitting PR #<n>`. The card
leaves needs you for pr-open with a Babysitting chip, and pickup launches
`/factory babysit <id>`, as [pickup](pickup.md) describes. Clearing babysit drops that priority again unless it was earlier.

## Approve spec

The drawer's top bar, the job actions menu, the card menu and the inbox show Approve
spec on a `needs-human` job with the note `gate: spec-approval` that is not a batch
rider, paused or closing. After a confirm it moves the job to `build`, sets the note
`spec approved on the dashboard`, deletes `needs` and stamps `updated`, in one board
write. A row that changed meanwhile is checked again and never forced. Pickup then
launches `/factory recover <id>`, which starts the builder, as [pickup](pickup.md)
describes. The Worker refuses that note from a device token, and `factory.py board`
refuses both approval notes, so only a signed-in person sets it.

## Rework and Approve as is

The drawer's top bar, the job actions menu, the card menu and the inbox show Rework and
Approve as is on a `needs-human` job held for a person, such as a review round cap or an
agent failure, that has no `resolve` and is not a batch rider, paused or closing. Rework
asks for a note of 1 to 2000 characters; Approve as is asks for a confirm. Either press
deletes `needs`, sets `resolve` to `{action: "rework", note, at}` or `{action: "approve",
at}`, and sets the note `rework requested on the dashboard` or `approval requested on the
dashboard`, in one board write. A row that changed meanwhile is never forced. The card
stays in needs you until pickup launches `/factory rework <id> --session <tag>` or
`/factory approve <id> --session <tag>`, as [pickup](pickup.md) describes. The Worker
refuses `resolve` from a device token, and `factory.py board` refuses any `resolve` but
null, so only a signed-in person sets it.

## Steering notes

The job actions menu and the card menu show Add steering note on a job at `spec`, `build` or
`review` that is not a batch rider, paused or closing and holds fewer than 10 notes. The dialog
asks for a note of 1 to 2000 characters and appends `{note, at}` to `steer`, stamping `updated`,
in one board write. It changes no other field. A row that changed meanwhile is never forced, and a
press that repeats the last note adds nothing. The job keeps running. The next builder dispatch
gets every note from `factory.py prompt ... build`, wrapped in an operator-preferences envelope
with a `steer:` source, and records each under `### Steering notes` in build.md. The drawer lists
the notes under `Steering notes: <n>`. Each line says `Sent to the builder <time>.` once a `build`
row in `agents` starts at or after it, `Waiting for the builder's next round.` while the job is
still at spec, build or review, and `Not sent: no builder round ran after it.` otherwise. The
Worker refuses `steer` changes from a device token, and `factory.py board` refuses a `steer`
patch, so only a signed-in person adds a note.

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
| needs-human | the file that explains the hold: the latest CHANGES review at the round cap, both repeated reviews for a repeat stop, or the failed agent's file. Use `#<id>` plus the note when only the note says why (a git conflict, say). |
