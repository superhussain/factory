# `/factory <feature>`

In a registered repo, `/factory <feature>` finds the project by its GitHub
remote, then its checkout path. `/factory <slug>: <feature>` works from any
directory. If the text starts with `<word>:`, try `where <word>`. On success the rest
is the feature. On exit 4, run `where` without a slug and treat the whole text as the
feature.

1. Run `sync "$SLUG"`. Claim the id, add the job to the board at stage `spec`,
   round 0, rounds `[]`, `project`, and `idea` (the feature text), and create the worktree with
   `git -C "$REPO_PATH" worktree add "$WT" -b factory/<id> "$BASE"`.
   Then batch related work into this job as `/factory idea` step 5 describes.
   `/factory idea` step 4 skips this sentence, since its own step 5 batches.
2. Spawn `factory-spec-writer` with the feature text, job folder, worktree, and
   project file, so the spec describes the same `$BASE` the builder starts from. It
   writes `spec.md`.
   When step 1 attached riders, build the feature text as `/factory idea` step 6
   describes, with this job's feature in place of its `idea`.
3. Apply the spec-approval gate as Gate stages describes.
   When `spec.md` has a `## Stacked PR` section, run
   `python3 "$RUNTIME" stack "$SLUG/<id>" <rider-id> --session <tag>` with the id on its
   line, then `git -C "$REPO_PATH" worktree add "$WORKTREES/<rider-id>" -b factory/<rider-id> factory/<id>`.
   Every builder dispatch for this job, fix rounds and recovery included, also gets that
   rider worktree. Exit 2, 4 or 6 writes nothing: report the line, create no rider
   worktree, and tell the builder to build every task on `factory/<id>`.
   Set stage `build`, then
   spawn `factory-builder` as `builder-<id>` at the
   tier Rules selects from the spec, with its assigned dev-server port if the job
   needs one. It commits on
   the branch and writes `build.md`. Push the branch.
4. Review round N: create `round-N/`, set stage `review`, `round` N, and append an
   all-`pending` entry to `rounds`. Before spawning anyone, run
   `python3 "$RUNTIME" review-plan "$SLUG/<id>" <N>` and patch the board entry with
   `"SKIPPED"` for each reviewer in its `skipped` list. Spawn only the reviewers in its
   `run` list, from `factory-review-security`, `factory-review-ux`,
   `factory-review-ui`, and `factory-review-code`, with the job folder, round folder,
   worktree, branch, and project file. A non-zero exit spawns all four and puts its
   stderr line in the board `note`.
   With a stacked rider, every reviewer and the approver get `$WORKTREES/<rider-id>`
   and `factory/<rider-id>` in place of the job's worktree and branch, since that
   branch holds the whole stack.
5. Once all four are in, any CHANGES first checks the round cap. At the cap, the job
   goes to `needs-human`. Below the cap, run
   `python3 "$RUNTIME" escalation "$SLUG/<id>" <N>`. When its decision is `escalate`,
   run `push "$SLUG/<id>"`, patch
   `{"stage": "needs-human", "note": "review panel: round <N> findings need a person (clef-flash <confidence>%)", "needs": "decide on the round-<N> findings at $DASHBOARD#<id>/round-<N>/review-<r>, then run /factory rework <id> <note> or /factory approve <id>"}`,
   with `<r>` the first CHANGES reviewer in the order security, ux, ui, code, and
   dispatch nothing. On any other decision, set stage `build` and dispatch the
   builder per Rules (builder selection; fresh worker on a model change), then
   repeat step 4 as round N+1. If the builder blocks, follow Rules instead of
   starting a review round.
   If the builder only disputed findings and the branch head didn't move (same
   `git rev-parse factory/<id>` before and after its reply), round N+1
   re-runs only the reviewers that asked for changes. Copy the others' review files
   into the new round folder and keep their verdict in the new board entry (PASS stays
   `PASS` and SKIPPED stays `SKIPPED`); only the re-run reviewers start `pending`.
6. Every verdict PASS or SKIPPED: set stage `approve` and spawn `factory-approver`
   with `Rounds left: <n>`, the review rounds this build cycle still allows, or 0 when the approver already
   returned CHANGES this cycle. It writes `decision.md`.
   APPROVE ships the PR and merges it as Rules describes. ESCALATE ships a held PR
   at `pr-open` with `escalated` true and the board note "escalated:" plus the
   decision's reason. The PR URL goes in `pr`, not the note. CHANGES is a normal fix
   round: send the builder `decision.md` as in step 5 and run the next review round,
   still under the round cap. One approver CHANGES per build cycle. A second one moves
   the job to `needs-human`.
7. After a merge, queue the job for a device build if the project file's Device
   builds section says its diff needs one: patch `{"build": {"state": "queued"}}`.
   Then check the build queue as Builds describes.
8. If `spec.md` lists sub-jobs, queue each one with
   `factory.py backlog add "<slug>: <feature> (sub-job-of <id>)" --context <file>`.
   Write the file first, in a temporary location outside the job folder. Its first
   line is `Recommend: approve` or `Recommend: deny`. Then come the reasons, from
   the spec, for the item and for your recommendation. Then add up to three
   `## Question:` sections in the needs.md format, only for choices the person
   should settle before the item is built. The `next` that runs one sets its `refs`.
9. After a merge, queue the job's open items. If `decision.md` already has a
   `## Follow-ups` heading, this step is done, so skip it. Open items are what
   `build.md`'s last run lists under `### Left open` (or, in an older build.md, as
   unresolved or left out), and final-round review nits under `### Fix now`,
   `### Follow-up` or an older nits heading that the merged diff did not apply. Items
   under `### Absorbed` are done, not open. Leave out spec sub-jobs (step 8 queues
   them), `## Device check` items, and anything already on `$BASE`. Queue each
   agent-doable item with
   `factory.py backlog add "<slug>: <feature> (follow-up-of <id>)" --context <file>`,
   writing the file first as in step 8: first line `Recommend: approve`, then
   `Left open by <id> (PR #<n>): <source file>`. List person-only items but never
   queue them and never set `needs` for them. Then append `## Follow-ups` to
   `decision.md` as a table with Item, Source and Outcome (`queued`,
   `already queued` or `for a person`) columns, or `None.` when there are none, and run
   `push "$SLUG/<id>"`.
10. After the `merged:` line and the final summary are sent, run
    `python3 "$RUNTIME" archive-workspace "$SLUG/<id>" --check`. If the output has a
    `reason`, stop. If `leftovers` is not empty, write a file that starts with
    `Recommend: approve`, then `Left in Paseo workspace <would_archive> when <id> merged:`,
    then one leftover per line, and run
    `python3 "$RUNTIME" backlog add "<slug>: Recover or drop what <id> left in Paseo workspace <would_archive> (follow-up-of <id>)" --context <file>`.
    If that exits non-zero, skip the archive: patch
    `{"note": "workspace kept: leftovers follow-up not queued: <stderr line>"}` and stop.
    An `already in backlog:` line exits 0 and counts as queued.
    As the session's last command, run `python3 "$RUNTIME" archive-workspace "$SLUG/<id>"`.
    It archives this session's Paseo workspace when it is the job's own worktree workspace,
    whatever its git state, and can end the session. When the workspace archive leaves
    this session's agent running, it archives the agent too. On exit 1, a stderr line holding
    `agent archive failed` is patched as `{"note": "<stderr line minus its factory.py: prefix>"}`.
    Any other stderr line is patched as
    `{"note": "workspace archive failed: <stderr line minus its factory.py: prefix>"}`.

**Reworking an existing PR.** When the feature is "rework PR #N", the spec writer
reads that PR's description and diff (`gh pr view N`, `gh pr diff N`) as the statement
of intent, and the builder may consult the old diff but builds fresh. Once the new
PR is open, point the job's `pr` at it and close #N with a one-line comment linking
its replacement. If #N belongs to a factory job, give the new job `refs` to it. If #N was stacked on another PR, cut the worktree from the
replacement of that base and open the new PR against it.

- When you close a job without shipping it (already fixed, duplicate, out of
scope), write `decision.md` first. Its first line is `CLOSED: <reason>`, such as
`CLOSED: already fixed on main, no change needed` or
`CLOSED: duplicate of <job-id>`. Then a `**Summary.**` of at most 120 words: why
it closed, the stage it stopped at, and what a person still owes, if anything.
Then `## Evidence`, with commits, commands and results a reader can check, and
`## Left out`, with anything not done and whether a job was queued for it. If
`decision.md` already holds an approver verdict, keep its full text under a
final `## Earlier decision` heading. Then queue its left-out items as
**Closing without shipping** step 1 describes. Run `python3 "$RUNTIME" push "$SLUG/<id>"`.
Only when it exits 0, patch `{"stage": "closed", "note": "<reason>", "needs": null}`,
run `cleanup` on it, and send `closed: <reason> · $DASHBOARD#<id>/decision`.
Then give the final summary and finish with **Closing without shipping** step 2.
A non-zero push moves the job to `needs-human` with the push error as the note.
Approver verdicts never start with `CLOSED:`, and no reader treats it as one.

**Closing without shipping.** When a stage shows the job should not ship (the
fix is already on `$BASE`, another job carries the same work, or the request is
out of scope), stop the loop and close it as Rules describes: `decision.md` with
first line `CLOSED: <reason>`, pushed before the stage moves to `closed`.

1. If `decision.md` already has a `## Follow-ups` heading, skip this step. Open items
   are each `## Left out` bullet and, when `build.md` exists, what its last run lists
   under `### Left open` or as unresolved. Leave out anything already on `$BASE` and
   any item whose bullet names an existing job. Queue each agent-doable item with
   `factory.py backlog add "<slug>: <feature> (follow-up-of <id>)" --context <file>`,
   writing the file first as in step 8: first line `Recommend: approve`, then
   `Left out by <id> (closed at <stage>): <source file>`. An `already in backlog:` line
   means it is queued. List person-only items but never queue them or set `needs`.
   Append `## Follow-ups` as step 9 describes.
2. After the `closed:` line and the final summary are sent, run step 10 with `closed`
   in place of `merged`, so the archive is the session's last command.

End with a short summary: the job's final stage, its rounds, what a human needs to
do, if anything (usually nothing), and the final stage's deep link from the table
in Rules. If that is a decision
for him, ask it through `needs.md` as Rules describes and link `#<id>/needs`.
For a merged or closed job, give that summary before step 10, since archiving can end the session.
