# `/factory approve-spec <job-id>`

A person passes the spec-approval gate. Run step zero for an existing job. The job
must be at `needs-human` with the note `gate: spec-approval`, with `spec.md` in its
folder and no `build.md`. A job with the note `gate: tracer-scope` gets the report
`Tracer is still gathering scope; approve or decline it in Tracer first`. Otherwise
report which check failed and stop. Run
`factory.py pipeline "$SLUG"` as Rules describes, patch
`{"stage": "build", "needs": null, "note": "spec approved"}`, then spawn the builder
as loop step 3 describes without its gate check, and continue the loop.
