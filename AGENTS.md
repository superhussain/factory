# Development commands

```sh
mise install
mise exec -- pnpm install --frozen-lockfile
mise exec -- node tests/browser/provision.mjs
mise exec -- pnpm run lint
mise exec -- pnpm run format:check
mise exec -- pnpm test
mise exec -- pnpm exec nx affected -t auth protocol assets deploy local browser --base="$(git merge-base origin/main HEAD)" --head=HEAD
mise exec -- pnpm exec nx run-many -t cli install verify
mise exec -- python3 tools/verify.py all
python3 tools/verify.py device
PYTHONPATH=tests/cli python3 -B -m unittest tests.cli.test_pickup.CloseTest
```

The device checks and installed Python bundle use only Python's standard library;
they need no Node, pnpm or mise. The affected targets cache under the ignored
`.nx/cache`; job 108 owns the lint and format targets. `local` binds
`127.0.0.1:8220`/`8221`; `browser` binds owned ephemeral ports, so parallel
jobs can run it at once. `browser` has no Nx dependency on `local`, so the two run side by side.

CLI tests import `support` from `tests/cli`. A command that names one test file, class or method needs the `PYTHONPATH=tests/cli` prefix, as above. On Windows, or in a shell without that prefix form, run `python3 -B -m unittest discover -s tests/cli -p test_pickup.py`. Without either, the run fails with `No module named 'support'`.

Commits run a husky pre-commit hook. It runs gitleaks on the staged diff first,
then lints and format-checks only the staged `.js`/`.mjs`/`.cjs`/`.ts` files
(see [development.md](docs/development.md)).
`pnpm run format:check` is a CI gate. Run `mise exec -- pnpm run format` to
apply oxfmt over `worker tools tests`; `.oxfmtrc.json` excludes `tests/fixtures/`.

To set up a factory for a person, read `.claude/skills/factory-setup/SKILL.md` and follow it.
