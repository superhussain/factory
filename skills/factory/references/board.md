# Board

## Patching the board

Several sessions can run at once, so every session edits the board through `factory`,
which retries on a conflicting write, creates a missing board, and stamps both
`updated` fields. Pipe a JSON merge patch for one job on stdin. It prints the job,
and a new id adds it. A new id must be digits, a dash and a lowercase slug, or the
command exits 2 and writes nothing. The patch cannot carry an `id` key, since the id
comes from the argument. Update the board at every stage change and as each
verdict lands.

```bash
export FACTORY_SESSION=<tag>
echo '{"stage":"review","round":1,"rounds":[{"security":"pending","ux":"pending","ui":"pending","code":"pending"}]}' \
  | "$RUNTIME" board "$FACTORY" 003-rate-limiting
```

`null` clears a key (`{"port": null}`), and `rounds` is always sent whole. To record one
reviewer's verdict, run `"$RUNTIME" verdict "$SLUG/<id>" <N> <reviewer> <verdict>`
instead of resending `rounds`. A `board` patch cannot rewrite a recorded round: it may only keep the
stored rounds and append all-pending ones. The job's first patch also sets `"project": "<slug>"`.

When `<home>/tracer.json` lists the project, a patch that sets `stage` or `rounds` also mirrors the job into Tracer, and a Tracer failure only prints one warning line. While Tracer builds a job, pickup saves its task statuses as `tracer.tasks`. All of this runs only while Settings, Pipeline, Tracer integration is On. See [integrations](integrations.md#tracer-mirror).

## Board fields

```json
{
  "id": "003-rate-limiting",
  "project": "the project slug",
  "title": "Rate limiting on the sync API",
  "branch": "factory/003-rate-limiting",
  "stage": "idea | spec | build | review | approve | needs-human | pr-open | merged | closed",
  "idea": "the feature as typed, set only on dashboard-added jobs",
  "goal": "the spec Goal paragraph, at most 300 characters; set and cleared only by factory push",
  "decision": { "verdict": "APPROVE | ESCALATE | CHANGES | CLOSED", "decider": "person | model", "at": "..." },
  "round": 2,
  "rounds": [{ "security": "CHANGES", "ux": "PASS", "ui": "PASS", "code": "PASS" },
             { "security": "pending", "ux": "PASS", "ui": "pending", "code": "PASS" }],
  "pr": "https://github.com/<owner>/<repo>/pull/44",
  "escalated": true,
  "automerge": true,
  "draft": true,
  "stack_base": "factory/<job-id> of the draft job this one stacks on; set by factory start",
  "checked_head": "40-hex head SHA; set by factory checks",
  "resumes": { "count": 1, "key": "000000000000" },
  "jira": "optional Jira key, read by the Tracer mirror",
  "tracer": { "follow": true, "gather": true, "feature": "factory-42", "url": "set only by factory after a Tracer mirror", "scope": "gathering, approved or declined; set only by factory", "building": "true only while a Tracer build agent holds the feature; set by pickup", "stopped": "true once factory asked Tracer to stop the build scope approval queued; set only by factory", "tasks": "up to 30 {id, title, status} from the Tracer feature; set by pickup once Tracer builds the job" },
  "note": "optional one-line status, e.g. escalation reason",
  "needs": "optional: what only a human can do, e.g. approve spec.md",
  "steer": [{ "note": "Keep the CSV header order the same as the old report.", "at": "2026-01-02T14:00:00Z" }],
  "spec_approval": { "decider": "person", "via": "dashboard | tracer | command", "at": "..." },
  "not_before": "optional UTC time; pickup and stuck leave the job alone until it, e.g. 2026-11-17T09:00:00Z",
  "port": "optional dev-server port or block the builder holds, e.g. 8130-8139",
  "refs": [{ "id": "029-mobile-offline-swr", "kind": "rework-of | follow-up-of | sub-job-of | depends-on | batched-into (set only by factory batch; dropped by the lead's close or merge, or the dashboard's Remove from batch while the lead is at spec)" }],
  "stacked": "true on the one rider that ships as a PR stacked on its lead's PR; set only by factory stack",
  "build": { "state": "queued | running | done | failed", "runs": ["https://github.com/<repo>/actions/runs/1"] },
  "pipeline": { "global": 3, "project": 2 },
  "backlog": { "line": "- [ ] <slug>: <feature>", "decision": "approved | denied | rework", "at": "...", "notes": "optional", "answers": "optional", "notes_read": "...", "decider": "person | model" },
  "experiment": { "name": "pilot", "arm": "a | b", "provider": "provider-b", "model": "model-b", "effort": "high" },
  "agents": [{ "at": "...", "stage": "spec", "role": "spec", "harness": "claude", "via": "native", "model": "...", "effort": "medium", "session": "a1b2c3", "paseo": {"server": "...", "agent": "..."} }],
  "claim": { "device": "device-one", "session": "a1b2c3", "since": "...", "seen": "..." },
  "paseo_writer": {"server": "...", "agent": "..."},
  "token_cap": { "base": 0, "warned": "..." },
  "launch_failure": { "count": 1, "reason": "exit 3", "at": "..." },
  "repeated": [{ "round": 2, "first": 1, "reviewer": "code", "signature": "worker/export.js#missing-person-check", "at": "..." }],
  "started": "2026-10-15T11:40:00Z",
  "updated": "2026-10-15T12:00:00Z",
  "done": "2026-10-15T12:30:00Z"
}
```

`pr` is set once the PR is open and kept when the job moves to `merged` or `closed`. The
dashboard links only exact `https://github.com/<repo>/pull/<n>` URLs for the
job's own `$REPO`.
`round` is 0 before the first review. `rounds[i]` is round i+1. Verdicts are
`pending`, `PASS`, `CHANGES`, or `SKIPPED` (ux and ui only, written by `review-plan`). Reviewer keys are fixed: security, ux, ui, code.
`factory verdict` sets one reviewer's verdict in one write and keeps the rest of `rounds`.
It refuses PASS, CHANGES or SKIPPED unless `round-<n>/review-<reviewer>.md` in the job folder starts with that `VERDICT:` line.
A `board` patch cannot rewrite a recorded round: it may only keep the stored rounds and append all-pending ones.
`factory grounding` may add `"unverified": ["review-code.md export.py:412 bad-line"]` to a round entry: the review citations whose path or line did not check out. It never changes a verdict.
Set `port` when you assign one and clear it when you run `cleanup`, so other sessions can see
which ports live jobs hold. Only `factory` writes `claim`.
A `board` patch that would change only `updated` stamps, or only this session's `claim.seen` while the stored one is under 10 minutes old, sends no PUT and prints the stored row.
`depends-on` holds an idea back from pickup, `ideas` and `batch` until the named job is merged, closed or off the board. Pickup and `ideas` also let it run when the named job is a `pr-open` job with `draft` true; `batch` still waits.
Approving a backlog line turns each `Depends on: <slug>: <feature>` context line into this ref, and is refused while that line is unchecked. The dashboard's refusal links to that prerequisite's backlog item. `next` and `batch` skip such a line while its prerequisite is unchecked or still open, and add this ref to the job they make. `batch` allows a prerequisite that is the lead or one of its riders.
`done` is when the job entered `merged` or `closed`, and only `factory` writes it.
`started` is when the job first got a claim, and only `factory` writes it.
`decision` copies decision.md's verdict. `factory push` writes it when it uploads decision.md, and the pickup close pass writes it for a person's close. `board` refuses a `decision` patch with exit 2. `factory job` infers it for older rows from the local decision.md and marks it `inferred`.
`token_cap` holds the job token cap state, and only `factory pickup` writes it. `base` is the token total at the last park, so a resumed session gets a full cap again, and `warned` is when pickup last warned at 80%. `board` refuses a `token_cap` patch with exit 2.
`launch_failure` records failed pickup launches in a row, and only `factory pickup` writes it. `count` sets the next `not_before` delay, `reason` is the exit code or a path-free start error, and `at` is the last failure. A successful launch removes it. The fifth failure in a row moves the job to `needs-human` with a `needs` ask naming the host and last reason, and keeps the record. `board` refuses a `launch_failure` patch with exit 2.
`repeated` holds the newest 20 review findings `factory repeats` saw again, and only it writes them. `board` refuses a `repeated` patch with exit 2.
`escalated` is set when an ESCALATE PR ships, and marks a PR that waits on a human.
`draft` (true, false or null) marks a `pr-open` job whose PR is a GitHub draft that is not meant to merge. Such a job never waits on you: pickup backpressure, the inbox and the Needs you column skip it. Whoever marks the PR ready patches `{"draft": null}`.
`stack_base` is `factory/<job-id>` or null. `factory start` sets it when an idea depends on a draft `pr-open` job, and cuts the new worktree from that branch. `board` refuses any other value with `stack_base must be factory/<job-id> or null` (exit 2).
`automerge` marks an APPROVE PR waiting to merge. `board` refuses `automerge` true
unless the local decision.md opens with APPROVE, and refuses stage `approve` unless
the last `rounds` entry is all PASS or SKIPPED; factory says it cannot move to
approve. `checked_head` is the PR head that `factory checks` last saw pass, or saw with no checks. `factory checks` writes it. `factory pr-merge` merges only that head, and exits 3 `unchecked` for any other. `resumes` counts consecutive
scheduled resumes without progress, and only `factory stuck` writes it.
`refs` links a job to the jobs it came from, and the dashboard shows them on the
card and as links in the job panel. The drawer's Add follow-up idea item saves a
new idea with a `follow-up-of` ref to the open job. `build` is set only on merged jobs that need a
device build. Jobs whose `runs` match rode the same build. Both are optional, and
`factory` rejects a malformed one.
`pipeline` holds the pipeline configuration versions the job last ran under, from
`factory pipeline <slug> --board`. `factory` rejects a malformed one.
`not_before` holds a job back from pickup, `ideas` and `stuck` until that UTC time, and the job
does not count toward `max_inflight`. `{"not_before": null}` clears it, and a past time has no
effect. Setting it on an open job releases the writer's own claim, so a session parks a job with
one board write and ends. `stuck` resumes it after that time.
`steer` holds up to 10 steering notes a person added on the dashboard while the job is at
`spec`, `build` or `review`, oldest first. Only a signed-in person appends one, one per write, and the
Worker refuses any edit, a device write, a write that drops stored notes,
a device write that removes an open row holding notes, and notes on riders.
`factory board` refuses a `steer` patch with exit 2. `factory prompt ... build` wraps every
note as data in each later build prompt, as [dashboard](dashboard.md) describes.
`spec_approval` records who passed the spec-approval gate. The dashboard's Approve spec,
the factory binary's Tracer approval and the `/factory approve-spec` patch write it with the approval
note. The Worker refuses `via: "dashboard"` from a device and any `via` without its note.
`board` refuses a `spec_approval` patch. `factory job` infers it for older rows from the
approval notes and marks it `inferred`.
`backlog` is set only on jobs decided from the Backlog column or `backlog decide`. It holds
the backlog line, the decision, when it was made, and any notes typed with the decision.
Approve also appends the notes to `idea`.
`decider` says who made the decision: `person` for the dashboard and `backlog decide`, `model` when backlog triage queued the line.

How the dashboard's idea column, Assign project and backlog decision page behave is in [dashboard](dashboard.md).

`experiment` is set only by pickup, at first claim, for an owner-model experiment; see
[pickup](pickup.md#owner-model-experiment).
Only pickup and `agent record` append `agents` rows, newest 32 kept;
`agent report` counts them.
A row's paseo is the session that recorded it, or the session whose hold claimed its tag, or the worker named by `--paseo-agent`.
A babysit sent to the job's own agent records a babysit row linked to that agent.
`paseo_writer` is the Paseo session that last wrote the job with `board` while it held no claim and the job had no `paseo`. Only `factory` writes it. The dashboard uses it as the session link when `paseo` is absent. Nothing else reads it.
`notes_read` is when a `/factory deny-notes` session finished a denied job's notes, and
only `factory deny-notes` writes it.
`babysit` is `{at}`, set on a `pr-open` job by Babysit PR or `control` (plus `via`, `quote`,
`by`). A `/factory babysit` session clears it with `null`; `board` refuses other values. Clearing it also drops a priority with the same at.
`handled` maps babysit item signatures to `{count, at}`. Only `factory pr-handled` writes it, newest 32 kept.
