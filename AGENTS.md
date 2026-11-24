# Development commands

```sh
mise install
mise exec -- pnpm install --frozen-lockfile
mise exec -- node tests/browser/provision.mjs
mise exec -- pnpm run lint
mise exec -- pnpm run format:check
mise exec -- pnpm test
mise exec -- pnpm exec nx affected -t auth protocol assets deploy local browser --base="$(git merge-base origin/main HEAD)" --head=HEAD
mise exec -- pnpm exec nx run-many -t verify
mise exec -- node tools/verify.mjs all
(cd rust && cargo build --profile host --locked && cargo test --locked </dev/null)
node tests/parity/run.mjs --bin rust/target/host/factory --jobs 8
```

The installed bundle is the factory binary, and its checks are `cargo test` and the parity
harness; neither needs Node, pnpm or mise. The affected targets cache under the ignored
`.nx/cache`; job 108 owns the lint and format targets. `local` binds
`127.0.0.1:8220`/`8221` and runs its device flows on `rust/target/host/factory`, or `FACTORY_BIN`,
so build the binary first; `browser` binds owned ephemeral ports, so parallel
jobs can run it at once. `browser` has no Nx dependency on `local`, so the two run side by side.

The parity harness compares the binary with the golden files in `tests/parity/golden/`. After an intended
output change, rerun the changed cases with `--record --case <id>` and review the golden diff
(see [development.md](docs/development.md), Parity harness).

Commits run a husky pre-commit hook. It runs gitleaks on the staged diff first,
then lints and format-checks only the staged `.js`/`.mjs`/`.cjs`/`.ts` files
(see [development.md](docs/development.md)).
`pnpm run format:check` is a CI gate. Run `mise exec -- pnpm run format` to
apply oxfmt over `worker tools tests paseo-plugin`; `.oxfmtrc.json` excludes `tests/fixtures/`.

Repair a broken install with `mise exec -- pnpm install --frozen-lockfile`. Never delete `node_modules` recursively; `<installed-skill>/runtime/factory clean <slug>/<job-id> node_modules` resets it in a factory worktree. Host hooks deny `rm -rf`, and `factory audit` flags it as `recursive-delete`.

To set up a factory for a person, read `.claude/skills/factory-setup/SKILL.md` and follow it.
