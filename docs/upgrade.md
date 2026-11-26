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
mise exec -- node tools/verify.mjs all
```

The `hosted` group exercises assets, auth, protocol, local integration, browser
and deployment tooling against the merged source (browser provisioning first if
needed; see [development.md](development.md)).

Then bring each host up to date with one command (`factory.exe` on Windows):

```sh
<installed-skill>/runtime/factory catch-up
```

It fetches the checkout's origin, builds the binary from origin's own `rust/`
crate in a temporary detached worktree, and runs that binary's `install`. The new bundle then
rewrites the pickup timer if the host has one, and catch-up checks the Worker's
`deploy-main` run. Catch-up also rewrites every factory timer that still runs
`runtime/factory.py` (the crontab, `~/Library/LaunchAgents/factory.*.plist`,
`~/.config/systemd/user/factory-*.service` and `~/.config/factory-*.cmd`) and a
`stalls.json` adapter that runs `runtime/paseo_stalls.py`, so they run the binary
beside it, and reloads launchd and systemd. It prints one `timers:` line for each.
When `<home>/retro-schedule-prompt.md` differs from the installed `schedules/retro.md`
with `<installed-skill>` and `<home>` filled in, catch-up and `install` copy the old
file to `<home>/backups/retro-prompt-<stamp>` and rewrite it. Catch-up then reads
this host's Paseo schedules. It replaces the prompt of factory's own retro schedule
(its prompt starts with the template's heading, or its name starts with
`Factory retro`) when the prompt names `factory.py`, after saving the old one to
`<home>/backups/paseo-schedules-<stamp>`. Any other schedule that names `factory.py`
gets one `paseo schedules:` line naming its id and how to fix it; catch-up never edits
it. Without Paseo it prints one `paseo schedules: skipped` line and fails nothing.
`doctor` prints a `doctor:` line on stderr for each stored prompt or schedule that
still names `factory.py`. A host with nothing new prints `catch-up: nothing new` and
writes nothing else. Every run, even one with nothing new, first deletes `catch-up-*` copies
over a day old under `<home>/tmp` and prints `skill: removed <n> leftover binary copies over a
day old from <home>/tmp` when it removed any.

Before any local write it prints `backup: <home>/backups/catch-up-<stamp>`. That
folder (mode 700) holds `skill/factory/`, `crontab.txt` on cron hosts,
`LaunchAgents/factory.*.plist` on macOS, `config/factory-*.cmd` on Windows, and
`home/` with `config.md`, `paths.md`, `routes.md`, `launchers.json`, `stalls.json` and `.env`.
Catch-up never prunes these folders. To roll back:

1. Copy `skill/factory` back over `factory` in the skills root.
2. Restore the timer: `crontab <backup>/crontab.txt`, or copy the plists back to
   `~/Library/LaunchAgents` (or the `.cmd` files back to `~/.config`).
3. Copy the files in `home/` back into the factory home.

This restore is the only rollback. The Python runtime and its `FACTORY_RUNTIME=python`
switch are gone, and so is the `runtime/factory.py` stub that ran the binary beside it.

See [device.md](device.md) for the installer's lock and backup rules.

Settings, Hosts flags a host whose `bundle_behind_since` is more than a day
old. Reinstall that host first.

Install hooks are approved only for esbuild `0.28.2` and workerd `1.20261023.1`;
fsevents `2.3.3` is denied. Do not broaden approval to repair an installation.
To roll back dependencies, return to the previous reviewed checkout, remove
`node_modules` and reinstall using its documented package manager and lockfile
(`npm ci` for an npm-era checkout). This does not change instance or device state.

## 5. Dry-run, then operator-gated deploy

```sh
node tools/deploy.mjs check --profile deploy/profile.json
node tools/deploy.mjs render --profile deploy/profile.json
node tools/deploy.mjs dry-run --profile deploy/profile.json
node tools/deploy.mjs deploy --profile deploy/profile.json
```

`dry-run` never contacts production storage. The real `deploy` is the
operator's own gated step — reviewed merged source, passing tests, your own
approval — as [cloudflare.md](cloudflare.md) requires. Drafts are bound to the
origin's storage, so deploying to the same R2 bucket preserves them.

## Reassigning jobs between projects

`tools/reassign-jobs.py` moved factory history under the `factory` project once,
and was deleted after that. It loaded the Python runtime as a module, which is
gone. Run it from commit `3e510927` with that commit's `runtime/factory.py` if
another move is ever needed; its section in that commit's `docs/upgrade.md`
describes the plan, backup manifest and apply steps.

## Known gaps

There is no snapshot/restore tooling in this repository; see
[recovery.md](recovery.md) for the backup gate that remains mandatory.
