# Project settings

Project settings defaults to an existing R2 `projects/<slug>.md` configuration.
Its Document selector also offers the three reference slots below, not repo files,
device paths, global configuration, or credentials. Keep all configuration agent headings.
Frontmatter allows one `repo`, `base`, and `title` only. Use an ASCII GitHub
`owner/name` (or empty repo), a simple 1-200-character ASCII ref without traversal,
empty/dot-ended/`.lock` components, and a 1-160-character title without controls.
Flat comments and matching quotes are accepted; nested/multiline YAML and unknown
keys are not. UTF-8 documents are capped at 1 MiB with no controls except tab/CR/LF.
Legacy unsupported headers remain readable but need explicit correction before save.
Do not store secrets in the body or treat tab session storage as a secret vault.

Save or Cmd/Ctrl+S submits the original loaded/restored ETag. Errors and conflicts
keep the draft and base, not an automatic retry. Copy draft before confirming
Load latest, then reapply intended edits. Close/Escape retain drafts; tab reload
restores their original base. If storage is blocked, copy the in-memory draft
before leaving. Saved titles/repo link patterns refresh in the dashboard, and the
next device `where` reads the configuration. Running jobs and checkout paths do not change.

The only extra names are `notes.md`, `decisions.md`, and `runbook.md`, in that order.
These are factory-owned reference Markdown at `project-documents/<slug>/<name>`,
not checkout aliases. Only registered projects can access them. There are at most
three per project, each at most 1 MiB of valid UTF-8 without controls except tab/CR/LF.
Empty Markdown is valid; extras need no configuration frontmatter. No arbitrary
names, file imports, rename, delete or preview is provided. Before a session
first dispatches an agent for a job, `factory.py documents <slug>` writes the
non-empty documents to `projects/<slug>.documents.md`, and Rules passes that
path as `$DOCUMENTS`. That copy is read-only: edit documents in the dashboard;
the CLI never uploads them. Do not store secrets here.

Select an absent slot and explicitly Create document with `If-None-Match: *`;
existing documents Save with their original observed `If-Match`. The existence
listing is advisory, never an editing baseline. Each resource retains its own
tab-only draft, identity, create/edit condition and blocked recovery state, separate
from configuration's unchanged draft record. Conflicts or unknown save outcomes
block submission, including empty creates. Copy draft before confirming Load latest;
only a validated successful replacement clears it. Reapply intended edits afterward.
Failed/cancelled loads, close and tab reload retain stored drafts; copy memory-only
drafts before leaving if storage is unavailable. Never obtain a newer ETag and retry
automatically. Writes are atomic per object, not across documents. Direct operator
registration deletion between its existence check and a write is not transactional;
there is no supported project-delete route.

Project settings rollout is Worker-first:

1. Pause project edits and registration, including browser saves. Back up device
   drafts outside the cache before old `where`/`init` can overwrite them locally;
   keep those old commands paused until that device is upgraded.
2. The release owner deploys the merged Worker and dashboard first. Old blind
   project writes then fail with 428. Do not use the editor during the pause.
   An old Worker ignores new clients' conditions and lacks PUT response ETags;
   upgrading clients first is not safe mixed-version writing.
3. Reinstall on active devices, initialize as needed, then `pull <slug>` to adopt
   observed baselines. Equal caches adopt safely; divergent caches require the
   `.remote` comparison above. Resume only after active writers are upgraded,
   local drafts accounted for, and recovery complete. Offline old devices are
   blocked remotely but still need draft backups before any old `where`/`init`.
4. No bulk R2 rewrite is needed. Correct unsupported legacy keys/values in a local
   copy, compare the saved remote baseline, then push. Narrower saves are a
   deliberate compatibility change, never silent stripping of unknown fields.
5. Do not roll back only the Worker to blind project writes with the editor
   enabled. Pause edits and coordinate recovery with the release owner. Board/job
   writes retain their existing rules.

Reference-document rollout is additive. After four independent reviews, a fresh
final gate, green CI and the reviewed-head merge, the root release owner coordinates
deployment of the merged Worker and bundled dashboard. Do not rewrite project
objects or auto-create extras. No device upgrade is needed specifically for extras;
any unfinished configuration pause/upgrade above remains required independently.
Old dashboards keep configuration and its old draft keys; refresh reveals the
selector. A rollback to the accepted version-checked configuration-only Worker hides
extras but keeps their R2 objects and browser draft keys. Export outstanding drafts
before rollback; restore the newer dashboard to recover them. Never roll back further
to blind project writes with editing enabled.

The installed bundle needs no source checkout. Repository development and hosted
deployment are separate from device setup.

## Project sync

- `pull <slug>` loads an observed project version without uploading. Equal legacy
caches adopt that baseline; clean caches refresh, local edits stay intact, and
divergent copies produce `<slug>.md.remote` with exit 6. Compare it with `.md`,
merge explicitly, delete `.md.remote`, then `push <slug>`. Push sends exactly one
`If-Match` from `<slug>.sync.json`, never a save-time GET or retry. A missing baseline
or existing `.remote` refuses the push. A new remote change still fails with 412.

| Command | Message | Next step |
| --- | --- | --- |
| project `push <slug>` | no saved base version / unresolved `.remote` | Pull to compare with R2; merge the displayed remote copy and delete `.remote` before pushing. Do not invent metadata. |
| project `push <slug>` | Project changed in R2 | Local edits remain. Pull, compare/merge, delete `.remote`, then push using the compared version. Never fetch/retry inside the failed save. |
| `where`, project `init` | Project has local edits | Preserve and push the intended project edits before continuing; resolve any remote conflict explicitly. |
