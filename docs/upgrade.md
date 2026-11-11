# Upgrading your instance

Upgrade a running private instance to a newly merged source revision. The
complete Cloudflare recipe (bucket, Access policies, profile, deploy) is in
[cloudflare.md](cloudflare.md).

## 1. Pull merged source

Upgrade only from reviewed, merged source — never from a feature branch or a
dirty checkout:

```sh
git fetch origin && git checkout main && git pull
```

## 2. Pause writers and schedules

Ask device operators to stop claims and syncs, and pause any optional scheduled
agents (see [integrations.md](integrations.md)). Devices hold private drafts
locally; pausing writers prevents new remote writes during the upgrade.

## 3. Host-local backups

Back up, on each host that owns them, before touching the instance:

- the device's private `.env` and `config.md` (credentials)
- device-side drafts and any retained conflict copies

These are host-local files; the server does not hold device credentials.

## 4. Reinstall and verify

```sh
mise install
mise exec -- pnpm install --frozen-lockfile
mise exec -- python3 tools/verify.py all
```

The `hosted` group exercises assets, auth, protocol, local integration, browser
and deployment tooling against the merged source (browser provisioning first if
needed; see [development.md](development.md)).

Then replace the installed skill bundle on each host from this checkout:

```sh
python3 tools/install.py --destination <absolute-directory> --replace
```

The old bundle stays beside it as `factory.backup-<uuid>`. See
[device.md](device.md) for the lock and backup rules.

Install hooks are approved only for esbuild `0.28.2` and workerd `1.20261023.1`;
fsevents `2.3.3` is denied. Do not broaden approval to repair an installation.
To roll back dependencies, return to the previous reviewed checkout, remove
`node_modules` and reinstall using its documented package manager and lockfile
(`npm ci` for an npm-era checkout). This does not change instance or device state.

## 5. Dry-run, then operator-gated deploy

```sh
python3 tools/deploy.py check --profile deploy/profile.json
python3 tools/deploy.py render --profile deploy/profile.json
python3 tools/deploy.py dry-run --profile deploy/profile.json
python3 tools/deploy.py deploy --profile deploy/profile.json
```

`dry-run` never contacts production storage. The real `deploy` is the
operator's own gated step — reviewed merged source, passing tests, your own
approval — as [cloudflare.md](cloudflare.md) requires. Drafts are bound to the
origin's storage, so deploying to the same R2 bucket preserves them.

## Reassigning jobs between projects

Jobs sometimes need to move between projects — for example, factory history
consolidated under the `factory` project. `tools/reassign-jobs.py` does this in
two steps, both operator-run against your own instance:

```sh
python3 tools/reassign-jobs.py --runtime <installed>/runtime/factory.py --from <old-slug> --to factory plan
python3 tools/reassign-jobs.py --runtime <installed>/runtime/factory.py --from <old-slug> --to factory apply \
  --ids <comma-separated ids> --backup manifest.json
```

`--runtime` points at the installed runtime copy, which supplies the API client
and CAS board writes, so the tool authenticates as an ordinary device. `--from`
is required, as in `--from <old-slug> --to factory`; passing both slugs reversed
reverses the migration.

1. **Plan (no writes).** `plan` lists every job in the `--from` project, labeled
   `MIGRATE` (terminal stage, no live claim, no existing worktree),
   `KEEP` (not a candidate — the job id does not name the target), or
   `FLAG: <reason>` (`non-terminal`, `live claim`, `existing worktree`). Add
   `--json` for machine-readable output. Nothing is written.
2. **Review the ids.** The plan output is a starting point; the ids you pass to
   `apply` are the final authority. Only explicit ids are touched.
3. **Backup manifest.** Take your own verified R2 backup per
   [recovery.md](recovery.md), then write a manifest
   `{"version": 1, "files": {"<key>": "<sha256>"}}` covering `board.json` and
   every `jobs/<from>/<id>/` key for the ids being applied. `apply` refuses to
   run unless the manifest matches live GETs exactly.
4. **Pause writers and schedules** (see step 2 above), so the live state still
   matches the backup when `apply` runs.
5. **Apply.** Per reviewed job, in order: artifacts are copied to the new
   project prefix create-only (`If-None-Match: *` — a conflicting copy with
   different bytes flags the job and skips it entirely, changing nothing); a CAS
   board write changes only the job's `project` field (every other field,
   including stale claims, is preserved verbatim; only the board's top-level
   `updated` refreshes); the device-side job cache
   `<home>/jobs/<from>/<id>/` is renamed to `jobs/<to>/<id>/` with its
   `.sync.json`. Old R2 keys are never deleted: they remain frozen aliases, so
   historical `jobs/<from>/<id>/...` URLs and Markdown/image links inside old
   artifacts keep serving the same bytes. Add `--json` to `apply` for the
   per-file result record (old/new ETags and sha256).
6. **First pull after apply.** A device's first `pull <to>/<id>` reports
   "pulled N files" as it re-adopts the new ETags — expected, self-healing, no
   bytes change.
7. **Reversal.** Re-run `apply` with both slugs reversed and the same ids (and a
   fresh manifest for the reversed keys). Board project and cache move back;
   artifact copies at both prefixes stay as identical aliases.

Re-running `apply` for an already-migrated id verifies copies by sha256 and
board state, then makes no writes. Jobs outside the named projects, non-factory
`<old-slug>` jobs and worktrees are never touched: worktree and Git state is
left entirely alone.

## Known gaps

There is no snapshot/restore tooling in this repository; see
[recovery.md](recovery.md) for the backup gate that remains mandatory.
