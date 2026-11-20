# `/factory approve <job-id>`

For jobs stuck at `needs-human` for reasons other than ESCALATE, such as the review
round cap or a failed agent. The job must be at `needs-human`. Otherwise report its
stage and stop. A human has decided, so ship and merge the PR as an APPROVE without
re-running reviews. If it can't be opened, leave the job at `needs-human` with the
reason. A job held at the spec-approval gate (note `gate: spec-approval`, no
`build.md`) has nothing to ship. Report that and point to
`/factory approve-spec <job-id>`.

With `--session <tag>`, `factory.py pickup` started this session for the dashboard's
Approve as is press. Run `hold` with the tag, then read `resolve` from `factory.py job
<id>`. If `resolve.action` is not `approve`, report that and stop. Patch
`{"resolve": null}` before any other step, then approve the job as above. When there is
nothing to ship, keep the job at `needs-human` and patch the reason as its `note`.
