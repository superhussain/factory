# Development

Working on the repository itself: tests, tooling and docs. For deployment see
[cloudflare.md](cloudflare.md); for the installed device see [device.md](device.md).

## Prerequisites

- mise for the pinned tools in `mise.toml`
- Node `>=22.18.0`, development pinned to `24.14.0` (`.node-version`)
- pnpm `11.9.0` (`package.json` `packageManager`)
- Rust toolchain: install rustup from https://rustup.rs. `tools/install.sh` (`tools\install.ps1` on Windows) builds the factory binary with cargo from PATH or `~/.cargo/bin`
- Python `3.8+` for the `tools/*.py` scripts and the parity harness (still needed until those move off Python)
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
| `format:check` | `oxfmt --check` over `worker tools tests paseo-plugin` (CI gate; `tests/fixtures/` excluded by `.oxfmtrc.json`) |
| `format` | `oxfmt` over `worker tools tests paseo-plugin`, rewriting in place |
| `test:assets` | Asset build, deployment dry-run, bundle verification |
| `test:auth` | Asset preparation, then production auth unit suites |
| `test:protocol` | Asset preparation, then v1 protocol, settings, lockfile suites |
| `test:local` | Local integration check on owned loopback ports |
| `test:browser` | Full browser suite |
| `test:deploy` | Deployment tooling tests and projections |

Type checking is allowlisted per module: `mise exec -- pnpm run check:types` runs `tsc --strict` over the converted modules only (`worker/access.ts` today). Modules join the allowlist by adding them to `tsconfig.json` `include` as they are converted.

`tools/verify.mjs` composes them into three fixed groups:

```sh
mise exec -- node tools/verify.mjs device    # cargo test of the rust/ crate
mise exec -- node tools/verify.mjs hosted    # the six pnpm scripts above, in order
mise exec -- node tools/verify.mjs all       # device then hosted, each leaf once
```

`--json` prints a one-line pass/fail summary as the last stdout line. The first
failing command stops the run with exit 1; invalid arguments or a missing pnpm
exit 2. The installed bundle needs no Node, pnpm or mise. On a host without Node, the
`device` group is `cargo test --locked --manifest-path rust/Cargo.toml` run by hand.

CI runs the same leaves once each in one Ubuntu pull-request workflow with
parallel jobs, and `verify.mjs all` is never a second CI run. The `targets` job
runs `format:check`, the workflow contracts and the affected `auth`, `protocol`,
`assets`, `deploy`, `local` and `verify` targets. Four
`browser` jobs each run the affected `browser` target on a quarter of the
suites, picked by `FACTORY_BROWSER_SHARD=<n>/4`; unset runs them all. The `paseo-plugin` job installs `paseo-plugin/` from its own lockfile, then runs its `typecheck` and `pnpm run test:paseo-plugin`, which also checks that every plugin lockfile entry has a public registry tarball URL. `paseo-plugin/.npmrc` pins `registry=https://registry.npmjs.org/`, because the plugin is its own pnpm workspace and does not read the root `.npmrc`. A user `~/.npmrc` that names a mirror would otherwise fail every plugin install with `ERR_PNPM_TARBALL_URL_MISMATCH`. Its packages are not in the cached store, so that install downloads them each run. The `rust` job checks `cargo fmt --check`, builds and tests the `rust/` crate for linux-x64 with the toolchain pinned in `rust/rust-toolchain.toml` (which includes `rustfmt`), runs `cargo check` for the `aarch64-apple-darwin` and `x86_64-pc-windows-gnu` targets, then runs the parity harness against its golden files (see Parity harness). The `ci`
job runs unless the run was cancelled, prints each job's name with its result,
and passes only when every other job succeeds. `local` binds
`127.0.0.1:8220`/`8221`/`8222` and `browser` binds owned ephemeral ports,
so the two run side by side with no Nx dependency between them. A pull request that
changes only docs selects nothing in the `browser` shards, which skip their
gated provision. `targets` always provisions the browser because `assets` runs a
browser test, and it restores the browser cache read-only so only the shards
save it. Each job restores the pnpm store read-only from `main`'s
`node-cache-Linux-x64-pnpm-<lockfile hash>` key (saved by the deploy workflow's
`setup-node`), so installs are warm across pull requests. Nx keeps task results in
the local `.nx` folder, and every CI job starts it cold.

## Affected runs

The three concerns are Nx projects, so a change can run only what it touches:

```sh
mise exec -- pnpm exec nx affected -t auth protocol assets deploy local browser \
  --base="$(git merge-base origin/main HEAD)" --head=HEAD
mise exec -- pnpm exec nx run-many -t verify
```

| Project | Target | Runs |
| --- | --- | --- |
| `worker-hosted` | `auth`, `protocol` | `pnpm run test:auth`, `pnpm run test:protocol` |
| `web-dashboard` | `assets` | `pnpm run test:assets` |
| `tooling` | `deploy`, `local`, `browser`, `verify` | `pnpm run test:deploy`, `pnpm run test:local`, `pnpm run test:browser`, `pnpm run test:verify` |

Selection is input-based, so shared Worker code invalidates the assets build and
both Worker suites. `worker-hosted`'s suites depend on `web-dashboard:assets`
because they import the `.build` artifacts it writes, so a clean checkout builds
assets first. The `local` and `browser` suites are cached like the rest: a
cache hit replays the stored output without executing. Only `local` binds
`127.0.0.1:8220`/`8221`/`8222`; `browser` starts its local services on owned ephemeral
ports, so the two never contend for a port. Their inputs cover
`tests/`, `worker/` and `web/` sources only — `skills/` stays outside.

`nx` is a pinned devDependency. Task results cache locally under the ignored
`.nx/cache`; `pnpm exec nx reset` clears them and `pnpm exec nx show projects`
lists the three projects. The `rust/` crate is not an Nx project: CI builds and tests it
in its own jobs, and `tools/verify.mjs device` runs its `cargo test`. Lint and format run as package scripts (`pnpm run lint`, `pnpm run format:check`), not Nx targets, so `nx.json` declares none.

## Parity harness

The parity harness proves a `factory` binary against golden files, one argv at a time. The goldens were recorded from the Python CLI before it was removed, so they keep its behavior as the oracle. It runs on POSIX hosts only.

```sh
python3 tests/parity/run.py --bin PATH [--ids FILE|DIR] [--case ID]... [--jobs N] [--shard K/N]
python3 tests/parity/run.py --bin rust/target/host/factory --jobs 8
python3 tests/parity/run.py --bin rust/target/host/factory --record --case 'prompt/*'
```

- `tests/parity/cases/<subsystem>.json` holds the hand-written cases as `{"version": 1, "cases": [...]}`. A case id is `<subsystem>/<name>` and lives in its subsystem's file; a misplaced or duplicate id exits 2. The runner adds an `arity/<command>/<n>` case for each argument count just outside a command's range in `tests/fixtures/cli-v1.json`.
- A case has `id`, `argv`, `home` and `stubs`, and may add `routes`, `stdin`, `env`, `exit` and `from`. `stdin` is piped to the binary; without it stdin is `/dev/null`. `env` is added to the isolated environment. `from` names the Python test the case came from, as `tests/cli/<file>::<Class>.<test>`.
- `routes` maps `"METHOD /path"` to `[status, body]`, `[status, body, headers]`, or a list of those served in order, the last one repeating. A string body is sent as its UTF-8 bytes; any other body as JSON. Each run gets its own loopback Worker, a `config.md` pointing at it and a `.env` with a fake Access token. A case with `routes` must set `exit`, and `--record` refuses a run that exits otherwise.
- Each case runs in its own temp root with `home/`, `bin/` and `stub/`. `home/` starts as a copy of `tests/parity/homes/<home>`, or empty. The runner compares the exit code, stdout, stderr, the home tree, each stub's call log and the Worker requests with `tests/parity/golden/<case-id>/`, one file per nonempty field. The home tree is gzipped as `home-tree.gz`; a missing file is an empty field. The Worker origin, home path and temp root are written as `{api}`, `{home}` and `{tmp}`, the binary's path as `{runtime}` (`{bin}` in a skill folder), and the folder two levels above it as `{skill}`. A `YYYY-MM-DDTHH:MM:SSZ` stamp inside the run window becomes `{now}`, today's UTC date `{today}` (the binary runs with `TZ=UTC`), today minus six days `{today-6}`, an `"elapsed"` count `{n}`, and the host's OS label in an `"os"` field `{os}`. A request is one JSON line of method, path, body and the `content-type`, `if-match`, `if-none-match`, `cf-access-client-id`, `cf-access-client-secret` and `user-agent` headers.
- The binary is `--bin`, else `FACTORY_BIN`. `--record` writes the selected cases' goldens from that binary instead of comparing. Use it only for an intended output change, then review the golden diff before committing it.
- `bin/gh`, `bin/paseo`, `bin/git`, `bin/crontab` and `bin/claude` run `tests/parity/stub.py`, which replays the first entry in the case's `stubs` map whose `argv` matches, logs every call, and exits 127 with `stub: no recorded response for ...` otherwise.
- `--ids` reads one `fnmatch` pattern per line, with `#` comments, from a file or from every `*.txt` in a folder. `rust/parity/<subsystem>.txt` lists the cases CI runs, which today is every case. In a folder, each pattern must start with its file's `<subsystem>/`. A pattern that matches nothing exits 2.
- `rust/factory/src/usage.txt` is the binary's no-argument stderr, frozen from the Python runtime's docstring.

## Visual captures

`node tests/browser/run.mjs --capture <outdir> --reference-html <absolute path>`
records matched before and after views. Each `view(...)` line in
`tests/browser/run.mjs` sets one view's name, viewport and theme.

| Viewport | Used by |
| --- | --- |
| 1440x900 | desktop views, `retros-flags-offline-desktop`, `board-merged-all-1440`, `board-merged-header-1440`, `board-merged-loading-1440`, `board-merged-saved-click-1440`, `board-banner-saved-1440`, `board-offline-deeplink-1440`, `board-saved-deeplink-1440`, `board-saved-deeplink-back-1440`, `board-merged-160-1763`, `board-merged-160-1763-loading`, `header-badges-1440`, `halt-banner-1440`, `backlog-lesson-desktop` |
| 1280x900 | settings, archive and agent-run views, `notify-toast-1280`, `notify-toast-top-left-1280`, `notify-toast-bottom-center-status-1280`, `notify-toast-corner-status-1280`, `display-toast-position-1280`, `notifications-menu-kinds-1280`, `notifications-split-1280`, `notifications-page-hidden-links-1280`, `notify-sound-wait-1280`, `retros-split-1280`, `settings-split-1280`, `approve-spec-drawer-1280`, `approve-spec-failed-1280`, `inbox-spec-1280`, `inbox-unblock-1280`, `resolve-drawer-1280`, `inbox-resolve-1280`, `steer-drawer-1280`, `steer-human-1280`, `grounding-tab-1280`, `grounding-chips-1280`, `not-before-launch-1280`, `job-tokens-1280`, `settings-halt-1280`, `settings-halt-blank-1280`, `inbox-lesson-1280`, `pipeline-trials-1280`, `idea-chips-loading-1280` |
| 390x844 | mobile views, `notify-toast-390`, `notify-toast-swipe-390` (touch), `notify-toast-bottom-status-390`, `notify-alerts-390`, `board-merged-header-390`, `approve-spec-drawer-390`, `babysit-drawer-390`, `rework-dialog-390`, `rework-dialog-saving-390`, `steer-dialog-390`, `grounding-tab-390` (touch), `job-tokens-390`, `halt-banner-390`, `backlog-lesson-mobile`, `inbox-lesson-390` and `pipeline-trials-390` |
| 375x667 | `drawer-mobile-scrolled` |
| 320x844 | `header-notifications-320` |
| 900x900 | `inbox-split-900` |

- Board data is the fixed `CAPTURE_JOBS`, `CAPTURE_MERGED` and `CAPTURE_BACKLOG` fixtures.
- `FACTORY_CAPTURE_VIEWS=a,b` limits a run to the named views.
- Views on a hash route that reads the full board call `waitFullBoard(page)` before they click or shoot, so the second draw cannot detach a node.
- A spec that needs another size or state adds a task for a new `view(...)` line.
- Capture mode starts a fresh browser every 50 sides and on each retry. A failed attempt logs a `capture fault` line with its stage, browser age, open contexts and load.

### Paseo plugin cards

`node tools/capture-plugin-cards.mjs --capture <outdir>` renders the Factory screen's three cards (first login, stall, sign-in expired) from the real `paseo-plugin/client/dashboard-screen.tsx`. It writes `plugin-card-<card>-1280.png` (1280x800) and `plugin-card-<card>-390.png` (390x844, compact layout). It needs `pnpm --dir paseo-plugin install --frozen-lockfile` and the provisioned browser.

Set `FACTORY_CAPTURE_PROBE_URL` to make the page request that URL; the run must then fail with "requests during plugin card capture" and write no PNG. `node --test tests/paseo-plugin/card-capture-guard.test.mjs` proves it.

- Settings are synthetic: dashboard `https://factory.example.test`, Paseo origin `https://app.example.test`.
- Before images: `git archive <base> paseo-plugin | tar -x -C .context/base`, then add `--plugin .context/base/paseo-plugin`.
- `FACTORY_CAPTURE_VIEWS=a,b` limits a run to the named views.
- Owned stubs in `tools/paseo-capture/` stand in for React Native, the Paseo plugin API and the frame. Layout is close to Paseo's, not identical. Icons are inline Lucide paths for the four names the screen uses. The dark palette is synthetic. Compare before and after from this tool only, never against app screenshots.

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
- The hook filters `skills/` out of the staged JavaScript list, and that is the
  only filtered path. gitleaks still scans `skills/`. Generated assets are
  excluded from the `lint`/`format:check` path lists by omission. Other
  directories are not filtered.
- oxfmt failure fails the commit with `formatting required: run 'mise exec --
  pnpm exec oxfmt <files>' and restage`. Replace `<files>` with the staged
  paths, or run `mise exec -- pnpm run format`.
- The repository follows oxfmt's default style over `worker tools tests paseo-plugin`, and
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
