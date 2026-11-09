# Recovery

Recovering data and access for a private instance. This repository deliberately
ships no snapshot/restore tooling yet; recovery is an operator procedure built
on your own backups.

## Before any real cutover

Pause all writers, back up host-local credentials and drafts, and complete an
independently verified R2 backup before any real cutover. Restore only to an
authorized empty scratch bucket; there is no destructive live restore and no
restore tooling in this repository yet.

The backup gate is not waived by anything: no planned or deferred snapshot
tooling, no clean-copy evidence and no upgrade procedure removes the
requirement for a complete, independently verified R2 backup first.

## What to back up, where it lives

| Data | Owner | Location |
| --- | --- | --- |
| Device credentials (`.env`, `config.md`) | Device host | Private file beside device config; never copied between hosts |
| Device drafts and conflict copies | Device host | Device home directory; preserved by `sync` |
| Project documents and board state | Server operator | The configured R2 bucket, via your own verified backup |

## Recovery boundaries

- Restoring bucket contents happens only into an empty, authorized scratch
  bucket you control, followed by explicit operator validation before any
  re-pointing of a live instance.
- The repository never performs a destructive live restore, and no command in
  this checkout writes to your bucket.
- Device credentials are restored per host from that host's own backup; do not
  import credentials from another host (see [device.md](device.md)).
- For deploy and cutover steps, follow [cloudflare.md](cloudflare.md) and
  [upgrade.md](upgrade.md); the same review gates apply after a recovery.

## Deferred tooling

A custom administrator snapshot/restore CLI, S3 client dependency and synthetic
S3 server are explicitly deferred follow-ups. Treat them as absent: until they
exist and are reviewed, your own verified backup is the only recovery path.
