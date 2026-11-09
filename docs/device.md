# Device setup

## Prerequisites and copy installation

Use Python 3.8+ and Git. No pip, npm, daemon or native agent runtime is required
for the device CLI. The repository installer copies only the self-contained skill.
Choose an absolute destination directory, then run from this checkout:

```text
python3 tools/install.py --destination <absolute-directory>
```

The installed skill is `<absolute-directory>/factory`. Load its SKILL.md through
your agent runtime, or invoke its Python CLI directly from any working directory:

```text
python3 <installed-skill>/runtime/factory.py init
```

On Windows use `py -3` instead of `python3`, quoting paths containing spaces.

Use this installer on every host.

The copied bundle works when the source checkout is unavailable. It includes all
templates, eight canonical role bodies, execution references and optional prompts.
There are no repository-relative runtime reads or runtime dependency downloads.

An identical install is a no-content-change success. A differing existing install
requires explicit replacement:

```text
python3 tools/install.py --destination <absolute-directory> --replace
```

Replacement retains `factory.backup-<uuid>` beside the installed directory and
prints both paths. It never merges trees or deletes older backups. A completed
sibling stage is switched only after validation. Pause other installers and
editors during replacement. `.factory-install.lock` serializes cooperating
installers, not arbitrary external filesystem writers. An existing lock fails
closed; inspect its owner and resolve it manually, never auto-break a stale lock.
The installer touches no device state, other skills, native roles or schedules.

## Offline initialization and credentials

`init` seeds only absent `config.md` and `.env`, prints `factory home: <home>` and
reports missing endpoint/token with success exit 0. No network or Git registration
is attempted until both are configured. No roles install implicitly.

Fresh config contains only `api_url`, `device` and `claim_stale_minutes: 120`.
An empty device uses the hostname. Stale minutes includes 0; zero never blocks
another claim and disables time-based stuck detection, not dead-session checks.
Existing unknown config fields, credentials and permissions remain unchanged.

Set `api_url` manually to your HTTPS origin in device config, or import a nonsecret
descriptor containing exactly `{"version":1,"api_url":"https://example.invalid"}`:

```text
python3 <installed-skill>/runtime/factory.py init --connection <connection.json>
```

The descriptor is always HTTPS and contains no credentials or administrator data.
Import seeds only an absent/empty endpoint. An optional single trailing slash is
equivalent. A differing configured endpoint fails with exit 2 before any write.
Userinfo, non-root paths, query, fragment, controls and malformed ports are rejected.
Plain HTTP is allowed only for localhost or 127.0.0.1 synthetic tests, not production.

Enter `FACTORY_ACCESS_CLIENT_ID` and `FACTORY_ACCESS_CLIENT_SECRET` manually in
the private `.env` beside config.md. Never enter token values in argv, chat,
screenshots, logs or Git. Both older and modern secret formats are opaque strings;
the CLI sends them only in the two Access headers to the validated origin and
does not follow Access redirects. Devices need no Cloudflare administrator profile.
Fresh POSIX `.env` is mode 0600. Repeated init preserves existing modes rather than
silently repairing them. Review existing ownership/permissions yourself. Windows
uses the user's directory ACL; POSIX mode bits do not promise Windows secrecy.

## Home selection and pointer-only adoption

| Priority | Location |
| --- | --- |
| 1 | Explicit `--home <absolute-path>` anywhere in the CLI invocation |
| 2 | Nonempty `FACTORY_HOME` |
| 3 | `<config_root>/factory-location.json` |
| 4 | `<config_root>/factory` |

On macOS/Linux `config_root` is absolute nonempty `XDG_CONFIG_HOME`, otherwise
`Path.home()/.config`. On Windows the same rule uses the user profile home,
normally `<USERPROFILE>/.config`; APPDATA is not selected implicitly. Empty
environment values mean unset. Relative explicit home/XDG values fail with exit 2.
Canonicalization creates nothing. Invalid pointers never fall back to an empty home.

Existing state stays where it is. Explicitly select it offline:

```text
python3 <installed-skill>/runtime/factory.py adopt-home <existing-absolute-directory>
```

Do not pass `--home` to adoption. It validates complete flat config/paths metadata,
project baselines and job manifests read-only, then exclusively publishes only the
generic pointer. The pointer contains exactly version 1 and a canonical home.
It changes no credentials, labels, claims, cache bytes, `.remote` files, permissions,
checkout paths, worktree registration or Git metadata. Dirty/conflicted caches are
valid retained state, not repair candidates. Checkout contents are not inspected.

A different pointer or initialized generic default home blocks adoption, including
a malformed default config. Identical adoption is idempotent and rechecks conflicts.
The pointer uses a mode-0600 same-directory temporary, flush/fsync and exclusive
hard-link publication. Unsupported atomic hard links fail closed with no fallback.
Readers see no pointer or a complete pointer. On Windows confidentiality relies
on the existing directory ACL. Adoption installs no roles or schedules.

Legacy `claim <factory-dir> ...` and `board <factory-dir> ...` retain authoritative
positional paths, including relative ones. An explicit `--home` must agree
canonically; environment/pointer defaults never replace those operands.
Global `--home` and `--session` each occur once with a nonempty value.

## Read-only doctor and project registration

```text
python3 <installed-skill>/runtime/factory.py doctor
```

Doctor emits one versioned JSON report with `home`, `api_url` and ordered location,
config, credentials, projects and board checks. Invalid location emits only that
failed check. It writes no files, claims, caches or roles. Invalid local config
prevents all requests; otherwise it GETs only `/projects.json` and `/api/board`.
An absent board is success and is not initialized. Reports never contain tokens,
headers, response bodies or raw transport exceptions. Doctor proves local setup
and read reachability, not JWT cryptography, write permission, deployment identity
or whole-Worker attestation.

After entering endpoint/token, register each unrelated checkout explicitly:

```text
python3 <installed-skill>/runtime/factory.py init <repo-path>
```

`init <repo>` is the sole registration flow, not a separate register verb.
It creates the remote project only with `If-None-Match: *`; existing projects
reconcile safely without overwriting drafts. Device checkout paths stay in
`paths.md`, not the shared project header. Fill the project context from its
actual repo, then use project push. Do not put credentials in project documents.

## Working commands

```text
python3 <installed-skill>/runtime/factory.py where sample
python3 <installed-skill>/runtime/factory.py claim <factory-home> small-feature sample
python3 <installed-skill>/runtime/factory.py sync sample
python3 <installed-skill>/runtime/factory.py handoff sample/001-small-feature --session <printed-tag>
```

Use the allocated ID, not the illustrative ID above. Claims without a supplied
session print a new tag to stderr; reuse it for later ownership calls. `where`
prints the ten v1 path/project keys as JSON. `sync` preserves drafts and conflict
copies. Handoff requires a quiescent checkpoint, clean published branch and the
owning session. It uploads artifacts before releasing the claim. Resume follows
the installed [execution guide](../skills/factory/references/execution.md), never
an automatic takeover or reset of review/approval counters.

## Exit recovery

| Exit | Action |
| --- | --- |
| 1 | Inspect Git failure or remaining stop PIDs. Stop only this job's owned processes; do not kill unrelated services. |
| 2 | Fix usage, absolute location, config, connection descriptor or role delimiters. Preserve state; no fallback to a new home. |
| 3 | Nothing runnable in backlog/ideas/stuck. Stop without retrying or claiming another device's work. |
| 4 | Object/project/branch missing. Confirm the ID/checkout and use init in the intended repo when registration is needed. |
| 5 | Check connectivity, HTTPS endpoint, Access policy and token renewal; origin failures also stop. Do not expose credentials while diagnosing. |
| 6 | Stop on another owner, drift, conflict or unsafe link/junction/unlistable path. Never retry/take over automatically. |

For `.remote`, compare retained local and remote copies, explicitly merge/select,
then delete only the reconciled conflict copy and push. Never upload `.remote`,
invent a missing baseline, silently change permissions, follow unsafe links or
force a claim/branch. Keep existing original state and backups while diagnosing.

## Host registration

Before pairing, the operator issues one Access service token per host and adds it
to the Service Auth policy. Never share one token between hosts. The Worker binds
each token to one host through `POST /api/hosts/<id>/pair` and accepts that
host's status only from its bound token. A person creates, confirms and retires
hosts through `/api/hosts`. Once any host is registered, pickup reaches only
active hosts, so register and confirm every host in one sitting. Pair each host
from its own terminal:

1. In Settings, Hosts, add the host under this device's current name: the
   `device:` value in `config.md`, else the hostname. Board claims use that
   name, so a different id is refused.
2. Within 15 minutes, run this on the host with the code Settings showed:

   ```text
   python3 <installed-skill>/runtime/factory.py host pair <id> <code>
   ```

   It sets `device: <id>` in `config.md` and sends the host's first status.
3. Confirm the host in Settings, Hosts.

A wrong attempt uses the code up, so shell history holds only a spent code. Ask
for a new code after any refusal. Retiring a host does not turn the filter off. To
return to pass-through, delete every `hosts/` object from the bucket. See
[architecture](architecture.md) for the routes.

Add, confirm and retire hosts in Settings, Hosts. The pickup controls list active
hosts as checkboxes.

## Explicit native agents and platform verification

```text
python3 <installed-skill>/runtime/factory.py init --agents both
```

Other selections are `none`, `claude` and `opencode`. See
[integrations](integrations.md) for native activation, opaque headers and optional
schedules. Installation, initialization and adoption never weaken security policies.

The `CI` workflow runs parallel Ubuntu 22.04/Python 3.12 jobs on pull requests, and its
`python` job runs the device suite. The runtime
supports Python 3.8+. The following additional platform checks can run locally.
`FactoryTest.test_released_hold_fails_on_real_windows_junction` runs on Windows
and is skipped elsewhere. POSIX-only permission cases
`FactoryTest.test_released_hold_fails_on_unlistable_round_directory`,
`FactoryTest.test_released_hold_fails_on_unreadable_job_folder_hiding_a_link` and
`LocationTest.test_unreadable_metadata_and_unlistable_tree` run as non-root on
Linux/macOS and skip Windows. `LocationTest.test_pointer_and_metadata_links_refused_but_home_alias_allowed`
and `LocationTest.test_unreadable_pointer_and_trusted_default_alias` use POSIX
symlinks/permissions; Windows owns the real junction case. Legacy symlink cases
run on POSIX and also on Windows when native symlink creation is available.
`PaseoStallsTest.test_list_reports_only_idle_claude_limit_stalls` and `PaseoStallsTest.test_dateless_reset_anchors_on_message_time` skip where `zoneinfo` cannot resolve `UTC` (Python 3.8, or no zone data); `PaseoStallsTest.test_missing_zoneinfo_leaves_stalls_out` covers that path on every host.
Do not skip a case when running on its owning platform.
