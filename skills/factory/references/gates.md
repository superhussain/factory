# Gate stages

The gate catalog is code: `GATES` in `worker/admission.js` and `PIPELINE_GATES` in
`factory`. Pipeline configuration can switch a catalog gate on for every project
or for one project, and a gate switched on globally applies to every project. Config
cannot add a gate outside the catalog or remove a loop stage: the spec, the build,
the reviews F2 requires, the approver and the PR always run.

| Id | Gate | Holds |
| --- | --- | --- |
| `spec-approval` | A person approves spec.md before the build starts. | between loop steps 2 and 3 |

**spec-approval.** Right before a job would move from `spec` to `build`, run
`factory pipeline "$SLUG"` again and use that fresh `$PIPELINE`. A non-zero exit
moves the job to `needs-human` as Pipeline configuration in [SKILL.md](../SKILL.md#rules) describes. When
`spec-approval` is in `$PIPELINE.gates`, run `push "$SLUG/<id>"`, patch
`{"stage": "needs-human", "note": "gate: spec-approval", "needs": "approve spec.md, then run /factory approve-spec <id> to start the build"}`
with `<id>` expanded, and dispatch nothing. When the job's board `tracer.scope` is
`gathering`, patch
`{"stage": "needs-human", "note": "gate: tracer-scope", "needs": "approve or decline scope for <feature> in Tracer; the factory build waits for that decision"}`
instead, with `<feature>` set to `tracer.feature`. When Tracer approves, factory
moves the job to `build` with note `spec approved in Tracer`, and pickup launches
`/factory recover <id>`. When Tracer declines, factory moves it to
`gate: spec-approval`. When Tracer already approved the scope, factory turns
either hold patch into `build` with note `spec approved in Tracer`. The printed job
shows it. Dispatch nothing, and pickup launches `/factory recover <id>`. With the
gate off, Tracer never holds the build. Only a person passes this gate: by
`/factory approve-spec`, by approving scope in Tracer, or by pressing Approve spec on the
dashboard, which moves the job to `build` with note `spec approved on the dashboard`.
