# needs.md format

- When the ask is a decision, or steps for him to run, write `needs.md` in the job
folder, `push` it, and set `needs` to `answer the questions at $DASHBOARD#<id>/needs`.
Submitting the form saves `answers.md` and clears needs when it names `#<id>/needs`
and the job is not at `needs-human`, so the resume schedule picks the job up. At
`needs-human`, `needs` stays until the human runs the When you are done command.
The dashboard renders it as a form, or as numbered steps when it has no questions.
Keep secrets out of it: the Worker serves it to anyone past Access. Use 1 to 4
questions, 2 to 4 options each, the recommended option first, and `Pick: one` or
`Pick: many`. Add a numbered `## Steps` section for action asks and a
`## When you are done` section with the exact command that lets you continue.

```text
# <one-line ask>

<why you stopped: one to three short paragraphs>

## Steps
1. <one action per step, exact commands in backticks>

## Question: <header, 1-60 chars>
<the question, one paragraph>
Pick: one
- <label> (recommended): <what it means and its trade-off>
- <label>: <description>

## When you are done
<the exact command or action that lets you continue>
```

On pickup, `pull`, then read `answers.md` only when its line
`For needs.md sha256 <12 hex>, submitted ...` matches the first 12 hex characters of
`sha256sum needs.md`. Otherwise it answers an older ask. The picks and the Other text
together are the decision, and Other text may refine an option that was not picked.
