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
2. Spawn `factory-spec-writer` with the pointer to its `prompt "$SLUG/<id>" spec --out`
   file, built as SKILL.md Rules describes, so the spec describes the same `$BASE` the
   builder starts from. It writes `spec.md`.
   When step 1 attached riders, build the feature text as `/factory idea` step 6
   describes, with this job's feature in place of its `idea`.
3. Before the gate, run `batch "$SLUG/<id>"`. When `spec.md`
   names a batched id missing from its `riders`, a person used Remove from batch.
   Respawn the spec writer with a fresh `prompt` file.
   Apply the spec-approval gate as [gates](../gates.md) describes.
   When `spec.md` has a `## Stacked PR` section, run
   `"$RUNTIME" stack "$SLUG/<id>" <rider-id>` with the id on its
   line, then `git -C "$REPO_PATH" worktree add "$WORKTREES/<rider-id>" -b factory/<rider-id> factory/<id>`.
   Every builder dispatch for this job, fix rounds and recovery included, also gets that
   rider worktree. Exit 2, 4 or 6 writes nothing: report the line, create no rider
   worktree, and tell the builder to build every task on `factory/<id>`.
   Run `"$RUNTIME" review-floor "$SLUG/<id>"`. A non-zero exit puts its stderr
   line in the board `note`, and the build goes ahead.
   Set stage `build`, then
   spawn `factory-builder` as `builder-<id>` at the
   tier Rules selects from the spec, with its assigned dev-server port if the job
   needs one. It commits on
   the branch and writes `build.md`.
   After every builder run, run `"$RUNTIME" build-landed "$SLUG/<id>"` and read the
   run line under `== build.md`. Exit 4 with `no run section` resumes the builder to write its run
   section. Any other exit 4 puts its stderr line in the board `note` and stops.
   Exit 5 runs it once more, then puts its stderr line in the board `note` and stops.
   When the run line
   starts `BLOCKED-ENV:`, start no pre-review or review round and keep the tier.
   Before either patch below, reduce the line. `<kind>` is the builder's kind when it
   is `secret`, `tool`, `network` or `service`, else `unknown`. `<what is missing>` is
   only the name of the missing piece: the first env var, command, host or service the
   line names, without quotes or backticks and cut at its first `=`. Write `unnamed`
   instead when that name is not 1 to 64 characters of letters, digits, `_`, `.` or
   `-`, or holds a run of 16 or more letters and digits that mixes both. Never copy
   any other word of the line, a value, a token or a URL into `note` or `needs`. Count
   the run sections that start `BLOCKED-ENV:` since the latest rework. On one, patch
   `{"note": "env block: <kind>: <what is missing>; one retry on the same tier"}` and
   dispatch a fresh builder on the same tier with the line on a short line. On two, run
   `push "$SLUG/<id>"` and patch
   `{"stage": "needs-human", "note": "env block: <kind>: <what is missing> (builder blocked twice)", "needs": "provide the missing <kind>: <what is missing>, then run /factory rework <id> <note>"}`
   from the latest line.
4. Pre-review: once `build-landed` exits 0, run
   `"$RUNTIME" pre-review "$SLUG/<id>"` as a background command, and act on its
   completion notification. Exit 1 means a check failed: patch
   `{"stage": "build", "note": "pre-review: <failed check names> failed"}`, resume the
   builder with `<job folder>/pre-review.md` on a short line, and create no round
   folder. When its JSON `repeats` list is not empty, run `push "$SLUG/<id>"`, patch
   `{"stage": "needs-human", "note": "repeat: pre-review <names> failed deterministically again in round <round>", "needs": "decide on the repeated check failure at $DASHBOARD#<id>/pre-review, then run /factory rework <id> <note> or /factory approve <id>"}`
   and do not resume the builder. Otherwise, when any failed check's `class` is
   `structural` or `canceled`, run `push "$SLUG/<id>"`, patch
   `{"stage": "needs-human", "note": "pre-review: <name> could not finish (<class>, exit <exit>)", "needs": "fix what stopped <name> at $DASHBOARD#<id>/pre-review, then run /factory rework <id> <note>"}`
   from the first such check, and do not resume the builder. A second exit 1 in one builder run counts as the Rules row for verification
   failing twice. Any other nonzero exit puts its stderr line in the board `note`, and
   the round goes ahead. A disputed-only round skips pre-review, since the head did not
   move. Its JSON `budget` never changes the exit code or the stage. The code reviewer
   reads it from `pre-review.md`.
   Review round N: create `round-N/`, set stage `review`, `round` N, and append an
   all-`pending` entry to `rounds`. Before spawning anyone, run
   `"$RUNTIME" review-plan "$SLUG/<id>" <N>` and run
   `"$RUNTIME" verdict "$SLUG/<id>" <N> <r> SKIPPED` for each reviewer `<r>` in its
   `skipped` list. Its `protected` list stays in `round-N/review-plan.json` for the
   approver. Spawn only the reviewers in its
   `run` list, from `factory-review-security`, `factory-review-ux`,
   `factory-review-ui`, and `factory-review-code`, each with the pointer to its own
   `prompt "$SLUG/<id>" review-<r> --round <N> --out` file.
   Dispatch each reviewer `<r>` on `routes.<r>` from the plan: null keeps the
   default reviewer selection, and a route runs on its `provider`, `model` and
   `effort` through the runtime's own model selection, recorded with `agent record`.
   When the runtime cannot select that model, dispatch on the default and record the
   model that ran. A disputed-only round re-runs reviewers on the default.
   A non-zero exit spawns all four and puts its stderr line in the board `note`.
   With a stacked rider, every reviewer and the approver get `$WORKTREES/<rider-id>`
   and `factory/<rider-id>` in place of the job's worktree and branch, since that
   branch holds the whole stack.
   Then run `"$RUNTIME" round-wait "$SLUG/<id>" <N>` with the Bash tool timeout
   at 600000 ms and no `--timeout`, never inside a shell loop. It records each
   verdict on the board. On `timeout`, run it again. On
   `malformed`, read the review file whose `files` entry is not a valid `VERDICT:`
   line, resume its agent, then run `round-wait` again.
5. Once `round-wait` prints `round`, run `"$RUNTIME" grounding "$SLUG/<id>" <N>`.
   It flags citations and never changes a verdict. A non-zero exit puts its stderr line in
   the board `note`, and the loop goes on. When it prints `"result": "unverified"`, the
   builder dispatch below and the approver in step 6 get `round-<N>/grounding.md` on a
   short line. Then any CHANGES first checks the round cap. At the cap, the job
   goes to `needs-human`. Below the cap, first run
   `"$RUNTIME" repeats "$SLUG/<id>" <N>`. When its decision is `repeat`, run
   `push "$SLUG/<id>"`, patch
   `{"stage": "needs-human", "note": "repeat: round <first> and round <N> both flag <signature> (<reviewer>)", "needs": "decide on the repeated finding at $DASHBOARD#<id>/round-<first>/review-<r> and $DASHBOARD#<id>/round-<N>/review-<r>, then run /factory rework <id> <note> or /factory approve <id>"}`
   from its first `repeats` entry, with ` and <k> more` after the reviewer when it lists
   k more, and dispatch nothing. When its decision is `churn`, run `push "$SLUG/<id>"`,
   patch
   `{"stage": "needs-human", "note": "churn: <path> changed in rounds <a>, <b> and <c>", "needs": "decide on the churned file at $DASHBOARD#<id>/build, then run /factory rework <id> <note> or /factory approve <id>"}`
   from its first `churn` entry, with ` and <k> more files` after `<c>` when it lists
   k more, and dispatch nothing. A non-zero exit puts its stderr line in the board
   `note` and the step goes on. Otherwise, run
   `"$RUNTIME" escalation "$SLUG/<id>" <N>`. When its decision is `escalate`,
   run `push "$SLUG/<id>"`, patch
   `{"stage": "needs-human", "note": "review panel: round <N> findings need a person (clef-flash <confidence>%)", "needs": "decide on the round-<N> findings at $DASHBOARD#<id>/round-<N>/review-<r>, then run /factory rework <id> <note> or /factory approve <id>"}`,
   with `<r>` the first CHANGES reviewer in the order security, ux, ui, code, and
   dispatch nothing. On any other decision, set stage `build` and dispatch the
   builder per [builder selection](../execution.md#builder-selection) (fresh worker on a model change), then
   repeat step 4 as round N+1. If the builder blocks, follow Rules instead of
   starting a review round.
   If the builder only disputed findings and the branch head didn't move (same
   `git rev-parse factory/<id>` before and after its reply), round N+1
   re-runs only the reviewers that asked for changes. Copy the others' review files
   into the new round folder and keep their verdict in the new board entry (PASS stays
   `PASS` and SKIPPED stays `SKIPPED`); only the re-run reviewers start `pending`.
   A disputed-only round skips `review-plan`; the copied files carry their verdicts.
6. Every verdict PASS or SKIPPED: set stage `approve` and spawn `factory-approver`
   with `Rounds left: <n>`, the review rounds this build cycle still allows, or 0 when the approver already
   returned CHANGES this cycle. It writes `decision.md`.
   APPROVE ships the PR and merges it as Rules describes. ESCALATE ships a held PR
   at `pr-open` with `escalated` true and the board note "escalated:" plus the
   decision's reason. The PR URL goes in `pr`, not the note. CHANGES is a normal fix
   round: run `"$RUNTIME" review-floor "$SLUG/<id>"` before sending the
   builder `decision.md`. Send `decision.md` as in step 5 and run the next review
   round, still under the round cap. One approver CHANGES per build cycle. A second
   one moves the job to `needs-human`.
7. After a merge, queue the job for a device build if the project file's Device
   builds section says its diff needs one: patch `{"build": {"state": "queued"}}`.
   Then check the build queue as [build](build.md) describes.
8. If `spec.md` lists sub-jobs, queue each one in list order with
   `factory backlog add "<slug>: <feature> (sub-job-of <id>)" --context <file>`.
   Write the file first, in a temporary location outside the job folder. Its first
   line is `Recommend: approve` or `Recommend: deny`. Right after `Recommend: approve`,
   write one `Depends on: <slug>: <feature> (sub-job-of <id>)` line per prerequisite
   the sub-job's `Depends on:` bullet names, matching that prerequisite's backlog line
   after `- [ ] `. Then come the reasons, from
   the spec, for the item and for your recommendation. Then add up to three
   `## Question:` sections in the needs.md format, only for choices the person
   should settle before the item is built. The `next` that runs one sets its `refs`.
9. After a merge, queue the job's open items. If `decision.md` already has a
   `## Follow-ups` heading, this step is done, so skip it. Open items are what
   `build.md`'s last run lists under `### Left open` (or, in an older build.md, as
   unresolved or left out), and final-round review nits under `### Fix now`,
   `### Follow-up` or an older nits heading that the merged diff did not apply. Items
   under `### Absorbed` are done, not open. Leave out spec sub-jobs (step 8 queues
   them), `## Device check` items, and anything already on `$BASE`. Drop cosmetic
   nits (line wrapping, comment placement, punctuation, wording) instead of
   queueing them. Queue each agent-doable item with
   `factory backlog add "<slug>: <feature> (follow-up-of <id>)" --context <file>`,
   writing the file first as in step 8: first line `Recommend: approve`, then
   `Left open by <id> (PR #<n>): <source file>`. List person-only items but never
   queue them and never set `needs` for them. Then append `## Follow-ups` to
   `decision.md` as a table with Item, Source and Outcome (`queued`,
   `already queued` or `for a person`) columns, or `None.` when there are none, and run
   `push "$SLUG/<id>"`.
10. Run `"$RUNTIME" usage record "$SLUG/<id>"`, then `"$RUNTIME" audit push <id>`.
    A non-zero exit from either never stops close-out.
    After the `merged:` line and the final summary are sent, run
    `"$RUNTIME" archive-workspace "$SLUG/<id>" --check`. If the output has a
    `reason`, stop. If `leftovers` is not empty, write a file that starts with
    `Recommend: approve`, then `Left in Paseo workspace <would_archive> when <id> merged:`,
    then one leftover per line, and run
    `"$RUNTIME" backlog add "<slug>: Recover or drop what <id> left in Paseo workspace <would_archive> (follow-up-of <id>)" --context <file>`.
    If that exits non-zero, skip the archive: patch
    `{"note": "workspace kept: leftovers follow-up not queued: <stderr line>"}` and stop.
    An `already in backlog:` line exits 0 and counts as queued.
    As the session's last command, run `"$RUNTIME" archive-workspace "$SLUG/<id>"`.
    It archives this session's Paseo workspace when it is the job's own worktree workspace,
    whatever its git state, and can end the session. When the workspace archive leaves
    this session's agent running, it archives the agent too. On exit 1, a stderr line holding
    `agent archive failed` is patched as `{"note": "<stderr line>"}`.
    Any other stderr line is patched as
    `{"note": "workspace archive failed: <stderr line>"}`.

**Stacked riders.** These steps apply only when the job has a stacked rider.

- After every build stage, also run `git -C "$WORKTREES/<rider-id>" push -u origin factory/<rider-id>`.
- At shipping: open the job's PR first, then push `factory/<rider-id>` and run
  `gh pr create -R "$REPO" --head factory/<rider-id> --base factory/<id>`. Its
  description opens with `Stacked on #<n>.` and covers only the rider's tasks. Patch
  the rider `{"pr": "<its PR URL>"}`. Write `$JOBS/<rider-id>/decision.md` with the
  job's `decision.md` first line, then `Approved with <id>: $DASHBOARD#<id>/decision`,
  and run `push "$SLUG/<rider-id>"`. The job's `Batched ideas` list omits the rider
  and ends with `Stacked: #<rider PR number>`.
- At merge and close: `board` moves it out of the batch at `pr-open` with the job's
  `automerge` and `escalated`. Merge its PR the same way, move it to `merged`, and
  run `cleanup "$SLUG/<rider-id>"`. When a job with a stacked rider closes, `board`
  closes the rider's open PR with a disclosed comment, the same guards as a dashboard
  close, and the rider goes back to `idea`.

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
**Closing without shipping** step 1 describes. Run `"$RUNTIME" push "$SLUG/<id>"`.
Only when it exits 0, patch `{"stage": "closed", "note": "<reason>", "needs": null}`,
run `cleanup` on it, and send `closed: <reason> · $DASHBOARD#<id>/decision`.
Then give the final summary and finish with **Closing without shipping** step 3.
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
   `factory backlog add "<slug>: <feature> (follow-up-of <id>)" --context <file>`,
   writing the file first as in step 8: first line `Recommend: approve`, then
   `Left out by <id> (closed at <stage>): <source file>`. An `already in backlog:` line
   means it is queued. List person-only items but never queue them or set `needs`.
   Append `## Follow-ups` as step 9 describes.
2. If the close reason states a durable project fact, such as a command, a path, a naming
   rule, or why a kind of change was rejected, write it as one sentence of at most 200
   characters to a file and run `"$RUNTIME" lesson "$SLUG/<id>" --fact-file <file>`.
   Add `--why-file <file>` when a person should judge the fact first. Skip it when the
   reason only names an outcome, such as duplicate, already fixed, superseded or out of
   scope. Never infer a fact the reason does not state. Record at most one lesson per job.
3. After the `closed:` line and the final summary are sent, run step 10 with `closed`
   in place of `merged`, so the archive is the session's last command.

End with a short summary: the job's final stage, its rounds, what a human needs to
do, if anything (usually nothing), and the final stage's deep link from the table
in [dashboard](../dashboard.md#stage-change-deep-links). If that is a decision
for him, ask it through `needs.md` as Rules describes and link `#<id>/needs`.
For a merged or closed job, give that summary before step 10, since archiving can end the session.
