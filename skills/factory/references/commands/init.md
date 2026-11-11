# `/factory init`

Sets up the selected device home, then registers the current repo only when
configured. Run `python3 <installed-skill>/runtime/factory.py init [<repo-path>]`.
It seeds absent config and private `.env` only, preserves existing bytes and modes,
and reports missing endpoint/token without network access. Enter credentials
manually in that private file, never in chat or arguments. Import an endpoint with
`--connection <file>` or set `api_url` manually. A disagreeing descriptor fails
without writes. `init <repo>` remains the only registration flow.

No roles install by default. Explicit `--agents claude|opencode|both` refreshes
nine role bodies after complete preflight, even offline. Both native runtimes
retain complete existing headers byte-for-byte; invalid YAML remains opaque.
Missing exact `---` delimiters or intervening `...` stops all role writes.
Native diagnostics own YAML validation. No provider, permission, model or schedule
settings change. See [integrations](../integrations.md) for paths and activation.

The first configured device uploads `projects/<slug>.md` to R2, and later
devices cache that copy. Every device records its checkout in `paths.md`. The slug
is the lowercased repo name with anything outside `a-z0-9-` turned into `-`, so
`example.invalid` becomes `example-invalid`.

Project registration is create-only with `If-None-Match: *`. A race exits 6 and
keeps the candidate for pull/compare, never replacing the existing remote.
Existing registrations reconcile through project pull: dirty/conflicted caches
stop instead of being overwritten, without disturbing their checkout-path entry.
New candidates have no `path` frontmatter. The runtime strips that legacy field
from older device caches before uploads; the Worker rejects raw path-bearing
documents. After the Worker-first rollout above, use `pull <slug>` to adopt an
observed baseline before editing an existing project on an upgraded device.

Before using the generated native OpenCode roles, verify role activation:
run `opencode api get /api/agent` and confirm all nine `factory-*` role IDs are
registered. A missing or invalid installed role stops native-role dispatch; fix that role's
frontmatter manually with the native diagnostics and rerun the check. Init
itself never needs the OpenCode CLI, and Claude-only devices do not run this
check.

A new device takes three steps:

1. Install the skill with `tools/install.py` (see `docs/device.md`).
2. Run `/factory init`.
3. Set `api_url` in config.md and `FACTORY_ACCESS_CLIENT_ID` and `FACTORY_ACCESS_CLIENT_SECRET` in `.env`, then
   run `/factory init` again in each project.

When `init` prints that the token is missing, tell the user to do step 3 themselves.
Never ask for the token, print it, or copy it anywhere else.

Fill a newly uploaded project file's body from what the repo actually uses: its
README, manifests, lint and typecheck scripts, styling setup, and auth code, then
`push "$SLUG"`. Leave an existing one alone.

Home precedence is explicit `--home <absolute-path>`, nonempty `FACTORY_HOME`,
`<config_root>/factory-location.json`, then `<config_root>/factory`.
`config_root` is absolute nonempty XDG_CONFIG_HOME or Path.home()/.config.
`adopt-home <existing-absolute-directory>` validates retained state offline and
publishes only a location pointer. It never moves state or Git worktrees, changes
permissions, installs roles or contacts the network. It refuses different active
homes. `doctor` prints sanitized read-only JSON and checks local configuration
before GET /projects.json and /api/board. It proves reachability, not deployment
identity, JWT cryptography or write permission. Malformed pointers fail closed.

Setup and diagnostics are exceptions: `init`, `adopt-home` and `doctor` resolve
locations directly and never require `where` first. Offline setup must not try to
read a remote project before endpoint/token configuration. The steps below apply
to job/work commands.
