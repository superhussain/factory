# `/factory babysit <job-id> --session <tag>`

Takes a `pr-open` job whose PR a person handed over with the dashboard's Babysit PR
button, and drives the PR to merge. `factory.py pickup` spawns it with the tag it
claimed the job under, so pass `--session <tag>` to `hold` and every `board` call.

The click counts as the person's merge go-ahead, so pipeline `merge: human` does not
hold a PR that GitHub shows as approved. It does not lift any other gate.

## Step zero

Run step zero for an existing job: `factory.py job <id>` for its `project`, then
`where <project>`, `hold <project>/<id>` and `worktree <project>/<id>`. If `hold`
exits 6, stop. If the job is not at `pr-open` or has no `babysit`, report that and
stop.

## Loop

Run `python3 "$RUNTIME" pr-state "$SLUG/<id>"`. It prints one JSON line,
`{"pr","state","review","checks","queued","conflict","next"}`. Exit 4 (bad `pr`) or 5
(gh failed): report the stderr line and stop. Otherwise act on `next`, then read again.

| `next` | Means | Action |
| --- | --- | --- |
| `merged` / `closed` | the PR state | close out, below |
| `fix-reviews` | a latest human review is `CHANGES_REQUESTED` or `COMMENTED` after the head commit | fix in `$WT`, push the branch, reply as the project file's PRs section says |
| `needs-review` | GitHub shows no approval | patch `{"needs": "get a human approval on PR #<n>, then press Babysit PR again", "babysit": null, "claim": null}` and stop |
| `rebase` | `mergeStateStatus` is `DIRTY` | merge `origin/$BASE` into `$WT`, fix the conflicts, push |
| `fix-ci` | a check is `fail` or `cancel` | fix from `gh run view --log-failed`, push |
| `wait` | checks pending, or the PR is queued | run `pr-state "$SLUG/<id>" --wait 540`, then `hold`, then act on the new reading |
| `merge` | otherwise | `gh pr merge <n> -R "$REPO" --merge --delete-branch`; if that fails, `gh pr merge <n> -R "$REPO" --auto --merge` once |

- Approved means `reviewDecision` is `APPROVED`, or it is empty and a latest human
  review is `APPROVED`. A human is any author whose login does not end in `[bot]`.
- At most 3 pushes per session. When a fourth would be needed, patch
  `{"stage": "needs-human", "babysit": null, "note": "babysit: 3 fix pushes did not get PR #<n> to merge"}`
  and stop.
- When a merge queue drops the PR, the next reading is `rebase`, `fix-ci` or
  `merge`, so the loop queues it again.

## Close-out

- `merged`: patch `{"stage": "merged", "babysit": null, "needs": null}`, then run
  [feature](feature.md) steps 7, 9 and 10.
- `closed`: close the job as feature.md's "close a job without shipping" says, with
  the reason `PR #<n> was closed on GitHub`, and patch `babysit: null` with the stage.
