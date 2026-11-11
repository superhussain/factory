# `/factory approve <job-id>`

For jobs stuck at `needs-human` for reasons other than ESCALATE, such as the review
round cap or a failed agent. The job must be at `needs-human`. Otherwise report its
stage and stop. A human has decided, so ship and merge the PR as an APPROVE without
re-running reviews. If it can't be opened, leave the job at `needs-human` with the
reason. A job held at the spec-approval gate (note `gate: spec-approval`, no
`build.md`) has nothing to ship. Report that and point to
`/factory approve-spec <job-id>`.
