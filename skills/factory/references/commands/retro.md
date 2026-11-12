# `/factory retro [--days N]`

Turns every host's pushed session digests for the last N days (default 7, at most 30)
into a ranked `findings.md` and up to 5 backlog lines that wait for a person.

1. Run `python3 "$RUNTIME" retro push`. On a nonzero exit, note the exit code and
   continue.
2. Run `python3 "$RUNTIME" retro aggregate [--days N]`. On exit 3, report the stderr
   line and stop. On any other nonzero exit, report the exit code and stop. On exit 0,
   read `run`, `aggregate`, `digest`, `transcripts`, `out` and `findings` from the
   printed JSON, and `host` from the `aggregate` file.
3. Dispatch one `factory-retro` subagent, giving it `run`, `aggregate`, `digest`,
   `transcripts` and `host`.
4. Write its reply byte for byte to `out`.
5. Run `python3 "$RUNTIME" retro admit <run>`. Report its output and the `findings`
   path. Exit 3 means the agent found nothing, autonomy is paused, or every card was a
   duplicate, over a cap or unusable. A person decides each line on the dashboard
   Backlog decision page. In the last two cases `findings.md` is written, so still
   report its path. A rejected findings[N] line means admission dropped that finding;
   findings.md lists it under Rejected findings without its text. Exit 2 after a wrote
   line means the cards failed: findings.md is written and no card was admitted.

Do the retro through these commands only, and edit neither the reply nor the run folder.
Never run an autonomy command or decide a backlog line; a person decides each line.
