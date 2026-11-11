# `/factory recover <job-id> --session <tag>`

Picks up a job that a dead session left mid-loop. Unlike `resume`, which continues a
handed-off job and refuses stale claims, `recover` runs under the claim `stuck` took. The resume schedule runs it after
`factory.py stuck`, and `factory.py pickup` runs it after a stalled-session takeover and for a job Tracer scope approval moved to `build`, and for a job moved to, or rerun on, this host from the dashboard, has claimed the job under `<tag>`, so pass `--session <tag>` to
`hold` and every `board` call, and the claim stays this session's. Run step zero for
an existing job, but run `worktree` only where the table says. If `hold` exits 6,
stop. If `worktree` exits 6 with a line starting `unsafe checkout:`, patch
`{"stage": "needs-human", "note": "<that line>"}` and stop. Never fall back to cutting
from `$BASE`. If the job isn't at spec, build, review, or approve, or at `pr-open` with
`automerge` true, report its stage and stop.

| Stage | Action |
| --- | --- |
| spec | Run `worktree`. On exit 4 (no branch here or on origin), run `git -C "$REPO_PATH" worktree add "$WT" -b factory/<id> "$BASE"`. If `spec.md` exists, go to loop step 3. Otherwise spawn the spec writer with the job's `idea` field, else its `title`, as in loop step 2. If the job has neither, move it to `needs-human` with that reason and stop. When `batch <project>/<id>` prints any `riders`, append them to the feature text as `/factory idea` step 6 describes. |
| build, note `spec approved in Tracer` | Run `worktree`, with the spec row's exit 4 fallback. If `spec.md` is missing or `build.md` exists, move the job to `needs-human` with that reason and stop. Patch `{"note": "building after Tracer approval"}`, then spawn the builder as loop step 3 describes without its gate check, and continue the loop. |
| build | Run `worktree`, then rework's dead-session path. Run `python3 "$RUNTIME" stop "$WT"` and write `rework-N.md`. It says this is a recovery after the session stopped (scheduled resume, attempt `resumes.count`) and lists the subjects from `git -C "$WT" log --oneline $BASE..HEAD` and the paths from `git -C "$WT" status --porcelain`. Run `push "$SLUG/<id>"`, set stage `build`, reset the round cap, dispatch `builder-<id>` per Rules on the rework file, and push the branch when it finishes. Continue from loop step 4 with the next round. |
| review | Run `worktree`. If `git -C "$WT" status --porcelain` is empty and `HEAD` equals `origin/factory/<id>`, spawn only the reviewers whose file is missing from the current round folder, then continue at loop step 5. Otherwise take the build path. |
| approve | Run `worktree`. Verify the passing round, then dispatch a fresh approver against the current HEAD and round. An existing decision.md is context only; never ship on it. Approver and retry budgets carry over unchanged. |
| pr-open, `automerge` true | No worktree. Run `gh pr view <n> -R "$REPO" --json state,baseRefName`. `MERGED` moves the job to `merged`, then run loop step 7. `CLOSED` moves it to `closed` with a note. `OPEN`: first check that the first line of `decision.md` is `APPROVE` and `baseRefName` is `$BASE`. If either fails, move the job to `needs-human` with a note naming which, and stop. Otherwise merge as Rules describes, re-running `factory.py pipeline "$SLUG"` right before `gh pr merge`: a non-zero exit goes to `needs-human`, and `merge` `human` holds the PR for a person with `automerge` false and the hold `needs` instead of merging, and a failing check or conflict runs `worktree` before the rework. |

Then continue the loop to the end and finish with the usual summary.

**Resume schedule.** An explicitly enabled schedule follows `schedules/resume.md`.
It runs `factory.py stuck --limit 2` and dispatches one configured `/factory recover` session per
job it prints. A session stalled on a usage limit whose reset is more than `takeover_minutes` away is taken over by `pickup` when `<home>/stalls.json` is set; see references/pickup.md.
`stuck` claims only jobs whose project has a checkout in this host's
`paths.md`. It picks a job at spec, build, review, or approve whose claim (or, with
no claim, `updated`) is older than `claim_stale_minutes`, or whose claim is this
device's and whose Paseo agent on this host is closed or gone. It also picks an
`automerge` PR at `pr-open`, not escalated, that is that old or whose agent is gone.
It skips any job whose `needs` is set, since that job waits on the user.
It also drops any stub whose claim is older than `claim_stale_minutes`.
An idle Paseo agent counts as alive, so `claim_stale_minutes` decides how long an
idle Paseo session, or a plain native session, keeps its job. Schedule setup and
refresh are explicit, using the user's chosen route, never a paid fallback.

`stuck` records each resume in the job's `resumes`. When a job comes back a fourth
time with the same stage, round, rounds, PR, and worktree `HEAD`, `stuck` moves it
to `needs-human` instead, with a note starting `resume: no progress after 3 resumes`.

Migration: nothing changes until a human creates a schedule. Jobs at `pr-open` from
before `automerge` lack the flag, so `stuck` never resumes them.
