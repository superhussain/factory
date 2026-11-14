# Board fields

```json
{
  "id": "003-rate-limiting",
  "project": "the project slug",
  "title": "Rate limiting on the sync API",
  "branch": "factory/003-rate-limiting",
  "stage": "idea | spec | build | review | approve | needs-human | pr-open | merged | closed",
  "idea": "the feature as typed, set only on dashboard-added jobs",
  "goal": "the spec Goal paragraph, at most 300 characters; set and cleared only by factory.py push",
  "round": 2,
  "rounds": [{ "security": "CHANGES", "ux": "PASS", "ui": "PASS", "code": "PASS" },
             { "security": "pending", "ux": "PASS", "ui": "pending", "code": "PASS" }],
  "pr": "https://github.com/<owner>/<repo>/pull/44",
  "escalated": true,
  "automerge": true,
  "resumes": { "count": 1, "key": "000000000000" },
  "jira": "optional Jira key, read by the Tracer mirror",
  "tracer": { "follow": true, "gather": true, "feature": "factory-42", "url": "set only by factory.py after a Tracer mirror", "scope": "gathering, approved or declined; set only by factory.py", "building": "true only while a Tracer build agent holds the feature; set by pickup", "stopped": "true once factory.py asked Tracer to stop the build scope approval queued; set only by factory.py", "tasks": "up to 30 {id, title, status} from the Tracer feature; set by pickup once Tracer builds the job" },
  "note": "optional one-line status, e.g. escalation reason",
  "needs": "optional: what only a human can do, e.g. approve spec.md",
  "port": "optional dev-server port or block the builder holds, e.g. 8130-8139",
  "refs": [{ "id": "029-mobile-offline-swr", "kind": "rework-of | follow-up-of | sub-job-of | batched-into (set only by factory.py batch; board refuses it)" }],
  "stacked": "true on the one rider that ships as a PR stacked on its lead's PR; set only by factory.py stack",
  "build": { "state": "queued | running | done | failed", "runs": ["https://github.com/<repo>/actions/runs/1"] },
  "pipeline": { "global": 3, "project": 2 },
  "backlog": { "line": "- [ ] <slug>: <feature>", "decision": "approved | denied | rework", "at": "...", "notes": "optional", "answers": "optional", "notes_read": "..." },
  "experiment": { "name": "pilot", "arm": "a | b", "provider": "provider-b", "model": "model-b", "effort": "high" },
  "agents": [{ "at": "...", "stage": "spec", "role": "spec", "harness": "claude", "via": "native", "model": "...", "effort": "medium", "session": "a1b2c3", "paseo": {"server": "...", "agent": "..."} }],
  "claim": { "device": "device-one", "session": "a1b2c3", "since": "...", "seen": "..." },
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
Set `port` when you assign one and clear it when you run `cleanup`, so other sessions can see
which ports live jobs hold. Only `factory.py` writes `claim`.
`done` is when the job entered `merged` or `closed`, and only `factory.py` writes it.
`started` is when the job first got a claim, and only `factory.py` writes it.
`escalated` is set when an ESCALATE PR ships, and marks a PR that waits on a human.
`automerge` marks an APPROVE PR waiting to merge. `resumes` counts consecutive
scheduled resumes without progress, and only `factory.py stuck` writes it.
`refs` links a job to the jobs it came from, and the dashboard shows them on the
card and as links in the job panel. The drawer's Add follow-up idea item saves a
new idea with a `follow-up-of` ref to the open job. `build` is set only on merged jobs that need a
device build. Jobs whose `runs` match rode the same build. Both are optional, and
`factory.py` rejects a malformed one.
`pipeline` holds the pipeline configuration versions the job last ran under, from
`factory.py pipeline`. `factory.py` rejects a malformed one.
`backlog` is set only on jobs decided from the Backlog column or `backlog decide`. It holds
the backlog line, the decision, when it was made, and any notes typed with the decision.
Approve also appends the notes to `idea`.

How the dashboard's idea column, Assign project and backlog decision page behave is in [dashboard](dashboard.md).

`experiment` is set only by pickup, at first claim, for an owner-model experiment; see
[pickup](pickup.md#owner-model-experiment).
Only pickup and `agent record` append `agents` rows, newest 32 kept;
`agent report` counts them.
A row's paseo is the session that recorded it, or the session whose hold claimed its tag, or the worker named by `--paseo-agent`.
A babysit sent to the job's own agent records a babysit row linked to that agent.
`notes_read` is when a `/factory deny-notes` session finished a denied job's notes, and
only `factory.py deny-notes` writes it.
`babysit` is `{at}`, set on a `pr-open` job by Babysit PR or `control` (plus `via`, `quote`,
`by`). A `/factory babysit` session clears it with `null`; `board` refuses other values. Clearing it also drops a priority with the same at.
