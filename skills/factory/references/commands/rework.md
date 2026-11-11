# `/factory rework <job-id> <note>`

A merged or closed job never reopens. Rework on one runs as a new job: claim an id,
patch `refs` to `[{"id": "<job-id>", "kind": "rework-of"}]`, and run the loop with
the note as the feature. Give the spec writer the original's `spec.md` and PR as
context. The rest of this section is for a job that hasn't merged.

Rework also recovers a job stuck at `build` or `review` after a session died. Write
the note to `$JOBS/<id>/rework-N.md` (the next free N), set stage `build`, and
reset the round cap. Step zero's `worktree` recreates a missing worktree. Dispatch
`builder-<id>` per Rules, reassessing the builder tier before resuming or respawning it.
Point it at the rework file, then continue the loop from step 4 with the next round
number. When recovering from a dead session, say so
in the rework file and list what the worktree holds (commits since `$BASE`,
uncommitted files), so the builder knows to finish rather than fix findings.
Before resuming the builder, run `python3 "$RUNTIME" stop "$WT"`,
since a dead builder's dev server may still hold the job's port. Don't remove the
worktree.
