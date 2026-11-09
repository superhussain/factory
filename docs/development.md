# Development

Working on the repository itself: tests, tooling and docs. For deployment see
[cloudflare.md](cloudflare.md); for the installed device see [device.md](device.md).

## Prerequisites

- mise for the pinned tools in `mise.toml`
- Node `>=22.18.0`, development pinned to `24.14.0` (`.node-version`)
- pnpm `11.9.0` (`package.json` `packageManager`)
- Python `3.8+` (standard library only)
- Git

## Install and browser provisioning

```sh
mise install
mise exec -- pnpm install --frozen-lockfile
mise exec -- node tests/browser/provision.mjs
```

`pnpm install --frozen-lockfile` installs the locked public-registry tarballs,
checks their SHA-512 integrity and rejects manifest/lockfile mismatches. It
downloads no browser. Browser tests need the explicitly provisioned Chrome for
Testing; runtime and offline
tests never use it.

The root-only `pnpm-workspace.yaml` uses isolated linking, strict dependency
build approval and no side-effects cache. Only esbuild `0.28.2` and workerd
`1.20261023.1` may run install hooks; fsevents `2.3.3` is explicitly denied.
Unreviewed hooks fail installation: do not blanket-approve them to get past a
failure. `verifyDepsBeforeRun: error` rejects stale dependencies before scripts
run; reinstall with the frozen command above instead of relying on auto-install.

To roll back, return to the previous reviewed checkout, remove `node_modules`
and reinstall dependencies with that checkout's documented package manager and
lockfile (`npm ci` for an npm-era checkout). No device config or state migration
is involved.

## Checks

The pnpm scripts are the source of truth (`package.json`):

| Script | What it runs |
| --- | --- |
| `lint` | `oxlint` over `worker tools tests` |
| `format:check` | `oxfmt --check` over `worker tools tests` (CI gate; `tests/fixtures/` excluded by `.oxfmtrc.json`) |
| `format` | `oxfmt` over `worker tools tests`, rewriting in place |
| `test:assets` | Asset build, deployment dry-run, bundle verification |
| `test:auth` | Production auth unit suites |
| `test:protocol` | v1 protocol, settings, lockfile suites |
| `test:local` | Local integration check on owned loopback ports |
| `test:browser` | Full browser suite |
| `test:deploy` | Deployment tooling tests and projections |

Type checking is allowlisted per module: `mise exec -- pnpm run check:types` runs `tsc --strict` over the converted modules only (`worker/access.ts` today). Modules join the allowlist by adding them to `tsconfig.json` `include` as they are converted.

`tools/verify.py` composes them into three fixed groups:

```sh
python3 tools/verify.py device    # cli + installer unittest discovery
mise exec -- python3 tools/verify.py hosted    # the six pnpm scripts above, in order
mise exec -- python3 tools/verify.py all       # device then hosted, each leaf once
```

`--json` prints a one-line pass/fail summary as the last stdout line. The first
failing command stops the run with exit 1; invalid arguments or a missing pnpm
exit 2. The `device` group and installed Python bundle need no Node, pnpm or mise.

CI runs the same leaves once each in one Ubuntu pull-request workflow with
parallel jobs, and `verify.py all` is never a second CI run. The `targets` job
runs `format:check`, the workflow contracts and the affected `auth`, `protocol`,
`assets`, `deploy` and `local` targets. The `python` job runs the cached `cli`,
`install` and `verify` targets via `nx run-many` on every pull request. Four
`browser` jobs each run the affected `browser` target on a quarter of the
suites, picked by `FACTORY_BROWSER_SHARD=<n>/4`; unset runs them all. The `ci`
job runs unless the run was cancelled, prints each job's name with its result,
and passes only when every other job succeeds. `local` binds
`127.0.0.1:8220`/`8221` and `browser` binds owned ephemeral ports,
so the two run side by side with no Nx dependency between them. A pull request that
changes only docs selects nothing in the `browser` shards, which skip their
gated provision. `targets` always provisions the browser because `assets` runs a
browser test, and it restores the browser cache read-only so only the shards
save it. Each job restores the pnpm store read-only from `main`'s
`node-cache-Linux-x64-pnpm-<lockfile hash>` key (saved by the deploy workflow's
`setup-node`), so installs are warm across pull requests. Nx keeps task results in
the local `.nx` folder. The `python` job saves that folder to the Actions cache
under a key scoped to its pull request, so a rerun or a later commit on the same
pull request replays unchanged `cli`, `install` and `verify` results. A pull
request with no entry of its own restores the newest `main` entry, which the
`Nx cache warm` workflow (`.github/workflows/nx-cache-warm.yml`) saves at 06:17 UTC
each day or when run by hand. Other jobs start cold.

## Affected runs

The four concerns are Nx projects, so a change can run only what it touches:

```sh
mise exec -- pnpm exec nx affected -t auth protocol assets deploy local browser \
  --base="$(git merge-base origin/main HEAD)" --head=HEAD
mise exec -- pnpm exec nx run-many -t cli install verify
```

| Project | Target | Runs |
| --- | --- | --- |
| `worker-hosted` | `auth`, `protocol` | `pnpm run test:auth`, `pnpm run test:protocol` |
| `web-dashboard` | `assets` | `pnpm run test:assets` |
| `tooling` | `deploy`, `local`, `browser`, `verify` | `pnpm run test:deploy`, `pnpm run test:local`, `pnpm run test:browser`, `pnpm run test:verify` |
| `device-bundle` | `cli`, `install` | `pnpm run test:cli`, `pnpm run test:install` |

Selection is input-based, so shared Worker code invalidates the assets build and
both Worker suites. `worker-hosted`'s suites depend on `web-dashboard:assets`
because they import the `.build` artifacts it writes, so a clean checkout builds
assets first. The `local` and `browser` suites are cached like the rest: a
cache hit replays the stored output without executing. Only `local` binds
`127.0.0.1:8220`/`8221`; `browser` starts its local services on owned ephemeral
ports, so the two never contend for a port. Their inputs cover
`tests/`, `worker/` and `web/` sources only — `skills/` stays outside.

`nx` is a pinned devDependency. Task results cache locally under the ignored
`.nx/cache`; `pnpm exec nx reset` clears them and `pnpm exec nx show projects`
lists the four projects. The `device-bundle` Python suites run as the cached
`cli`/`install` targets (and `tooling:verify`) through `nx run-many` — they are
not part of the `affected` selection; `tools/verify.py` still
runs them directly with no Node. Job 108 adds
the lint and format targets later; `nx.json` deliberately declares none.

## Pre-commit checks

Every `git commit` runs the `.husky/pre-commit` hook (installed by the
`prepare` script through the pinned `husky` devDependency; it only sets
`core.hooksPath` to `.husky/_`):

- It first runs `mise exec -- gitleaks git --pre-commit --staged --redact --no-banner --config .gitleaks.toml`
  over every staged file. A finding fails the commit before any lint step runs.
  gitleaks is pinned in `mise.toml`, and `--redact` keeps matched values out of
  the output.
- Next it lists the staged `.js`/`.mjs`/`.cjs`/`.ts` files and runs `mise exec -- pnpm
  exec oxlint` and `mise exec -- pnpm exec oxfmt --check` on exactly those
  files. With no staged JavaScript it exits immediately. If `pnpm` is
  unavailable it fails with a hint to install mise and run `mise exec -- pnpm
  install --frozen-lockfile`.
- The hook filters exactly `skills/` out of the staged JavaScript list; gitleaks still scans `skills/` — that is the only
  filtered path. Generated assets are excluded from the `lint`/`format:check`
  path lists by omission; other directories are not filtered.
- oxfmt failure fails the commit with `formatting required: run 'mise exec --
  pnpm exec oxfmt <files>' and restage`. Replace `<files>` with the staged
  paths, or run `mise exec -- pnpm run format`.
- The repository follows oxfmt's default style over `worker tools tests`, and
  the `CI` pull-request job runs `pnpm run format:check` as a gate.
  `.oxfmtrc.json` sets only `ignorePatterns: ["tests/fixtures/"]`. Fixtures are
  test data that suites read as bytes, so the formatter never rewrites them.
- The repo-wide reformat commit is listed in `.git-blame-ignore-revs`. GitHub's
  blame view skips it automatically. Locally, run
  `git config blame.ignoreRevsFile .git-blame-ignore-revs` once.
- `prepare: husky` runs on `pnpm install`: it executes only the pinned husky
  binary (public-registry tarball with SLSA provenance), performs no network
  calls or build steps, and runs no install scripts, so frozen-lockfile CI
  installs are unaffected. `.husky/_/` is gitignored.

## Data hygiene

Use synthetic data and temporary homes (`--home <absolute-path>` or
`FACTORY_HOME`) for manual experiments; local tests bind owned loopback ports
and never touch production. Never link or copy real credentials into test homes.
See [architecture.md](architecture.md) for ownership boundaries and
[integrations.md](integrations.md) for optional native roles.
