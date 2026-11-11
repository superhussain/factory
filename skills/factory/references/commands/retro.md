# `/factory retro [--days N]`

Turns every host's pushed session digests for the last N days (default 7, at most 30)
into a ranked `findings.md` and up to 5 gated idea cards.

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
   path. Exit 3 means the agent found nothing or no card was admitted.

Do the retro through these commands only, and edit neither the reply nor the run folder.
Never run suggest approve or any autonomy command; a human approves each card.
