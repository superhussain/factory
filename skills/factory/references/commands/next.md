# `/factory next`

Works from any directory. The backlog is `backlog.md` in R2, one
`- [ ] <slug>: <feature>` line per item. `next` takes the first unchecked line whose
project has a checkout on this device, and skips the rest with a `skipping <slug>`
line on stderr. An item may be followed by indented context lines that
`backlog add --context` wrote. `next` ignores them.

1. Run `python3 <this skill's dir>/runtime/factory.py sync`, with no slug, since the
   project isn't known yet. Conflicts don't block, as in the Claims table.
2. Run `python3 <this skill's dir>/runtime/factory.py next --session <tag>` (no tag on
   the first call) and read its JSON: `id`, `project`, and `feature`. It claims the
   id under that project and marks the line `- [x] <slug>: <feature> (<id>)`.
3. Exit 3 means nothing in the backlog can run on this device. Say so and stop
   after step 5, because nothing was claimed. Exit 2 or 4 names a malformed line or an unknown
   project that blocks the queue. Report it and stop.
4. Run `where <project>`, then step one of `/factory <slug>: <feature>` without
   the sync and claim, since `next` already claimed `<id>` for this session. A feature
   ending in `(<kind> <id>)`, such as `(sub-job-of 041-recipe-import)`, also sets
   `refs` in that first board patch. Continue the loop from there.
5. On exit 3, check the build queue as Builds describes before stopping.

A decision from the dashboard's backlog decision page checks lines off the same way.

To queue work, run `factory.py backlog add "<slug>: <feature>" [--context <file>]`, or edit
`backlog.md` in R2 by hand. Rework lines carry `(rework-of <id>)` like any other ref.
Report a queued item with the `link:` line `backlog add` prints.

A question for him about the backlog itself, such as which line to run next or
whether to drop one, has no job folder. When the decisions-via-askuserquestion
skill is installed, ask it as that skill describes.
