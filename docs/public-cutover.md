# Moving to a fresh public repository

Publish the current tree as a new public repository with one commit, and keep
the old private history under an archive name. `tools/public-cutover.py` checks
that the repository is ready, prints the plan, and acts only with `--apply`.
The examples use `OWNER/factory` for the repository, `OWNER/factory-private`
for the archive and `you@example.com` for the public author email.

## Before you start

- Job 680 (release audit and `tools/secret-scan.py`) is merged to `main`.
- Job 681 (README quickstart and `docs/cloudflare.md`) is merged to `main`.
- You have the values for the `deploy-main` workflow in hand: the
  `CLOUDFLARE_API_TOKEN` secret and the `FACTORY_DEPLOY_PROFILE` variable
  described in [cloudflare.md](cloudflare.md) section 8. Repository secrets and
  variables do not move to a new repository.
- `gh auth status` succeeds with an account that can rename and create
  repositories under `OWNER`, and `gitleaks` is installed.
- No factory job is in flight and no pull request is open.
- Autonomy and pickup are paused on every host, and stay paused until every
  checkout on every host is repointed. A session that starts in between can
  reuse a local branch that still holds private history and push it to the
  public `origin`.

## Dry run

Run from a clean checkout of `main` that matches `origin/main`:

```sh
python3 tools/public-cutover.py --repo OWNER/factory --archive OWNER/factory-private \
  --author-name "Your Name" --author-email you@example.com --project factory \
  --runtime <installed-skill>/runtime/factory.py --home <factory-home> \
  --checkout <checkout>
```

Every check prints `[ok]` or `[fail]`:

| Check | Fails when |
| --- | --- |
| tree | the checkout is dirty, or `HEAD` is not `origin/main` |
| auth | `gh auth status` fails |
| jobs | the runtime's `builds <project>` lists an in-flight or stale job |
| PRs | `OWNER/factory` has an open pull request |
| archive name | `OWNER/factory-private` already exists |
| gitleaks | `python3 tools/secret-scan.py` reports findings |
| names | a secret or variable the workflows use is missing |
| checkout | a `--checkout` path is dirty |

The script lists names only and never reads a secret or variable value. When
every check passes it prints the `[plan]` and `[human]` steps and ends with
`Dry run: nothing changed.` Exit 0 means ready, 1 means a check failed, 2 means
bad usage.

## Live run

Re-run the same command with `--apply`. The checks run again first, and any
failure stops the run before anything changes. Then the script:

1. Renames `OWNER/factory` to `OWNER/factory-private`.
2. Creates an empty public `OWNER/factory`.
3. Prints a `[human]` line per secret and variable and waits. Set each value
   yourself, for example `gh secret set CLOUDFLARE_API_TOKEN -R OWNER/factory`,
   then type `done`.
4. Lists the names on the new repository. If one is missing, it prints
   `[fail] names: missing <NAME>`, repeats the `[human]` line for it and waits
   again. It never pushes while a name is missing.
5. Builds one commit from the `HEAD` tree with `git commit-tree`, authored and
   committed as `you@example.com`, and pushes it to `main`.
6. Repoints each `--checkout`: adds remote `archive` for the old history,
   fetches `origin`, detaches at `origin/main`, and renames every local branch
   the new `origin/main` cannot reach to `archive/<name>`. It then runs
   `git branch --unset-upstream` on every `archive/` branch that still tracks a
   remote, so a plain `git push` cannot send it to the public `origin`. It
   prints the renamed branches and the cleared upstreams.
7. Rewrites the `repo:` line of `<home>/projects/<project>.md` only if it
   differs, then pushes the project file with the runtime.
8. Watches the first `deploy-main.yml` run on `main`.
9. Pushes an empty commit to branch `cutover-ci-check`, opens a pull request,
   watches `ci.yml`, then closes the pull request and deletes the branch.

The first push deploys the same tree production already runs.

## Other hosts

On every other host with a checkout, such as a second laptop, repoint it
without touching GitHub:

```sh
python3 tools/public-cutover.py --repo OWNER/factory --archive OWNER/factory-private \
  --author-name "Your Name" --author-email you@example.com --project factory \
  --runtime <installed-skill>/runtime/factory.py --home <factory-home> \
  --checkout <checkout> --repoint-only
```

`--repoint-only` skips the checks and runs only the checkout step, so it needs
at least one `--checkout`. It is safe to re-run: an existing `archive` remote
is kept, branches already under `archive/` are not renamed again, and a re-run
clears any upstream they still have. The renamed branches still hold private
history; push them only to `archive`. Resume autonomy and pickup once every
host is repointed. Until a host is repointed, pickup and `factory.py worktree`
refuse it with an `unsafe checkout` line, so it claims, launches and recovers
nothing.

## After

- Disable `nx-cache-warm.yml` on `OWNER/factory-private`, so the archive stops
  running its daily schedule.

## Old pull request links

Board jobs merged before the cutover still link `OWNER/factory/pull/N`, which
now opens a new pull request or a 404. Point them at the archive in this order:

1. Deploy the Worker and dashboard that accept `archive:`. The `deploy-main`
   workflow does this when the change merges. An older dashboard drops archive
   links, and an older Worker rejects the `archive:` line.
2. Add `archive: OWNER/factory-private` to the frontmatter of
   `<home>/projects/factory.md`, then push it:
   `FACTORY_HOME=<factory-home> python3 <installed-skill>/runtime/factory.py push factory`.
3. Dry run, and save the output. It lists every old URL, so it is also the undo
   record:

   ```sh
   python3 tools/rewrite-pr-links.py --runtime <installed-skill>/runtime/factory.py \
     --home <factory-home> --repo OWNER/factory --archive OWNER/factory-private
   ```

4. Review it. A `[plan]` line names a job whose archive pull request has head
   branch `factory/<job id>`. A `[keep]` line stays as it is.
5. Re-run the same command with `--apply`. It writes the board once and changes
   only the planned `pr` fields and the board's `updated` time. A re-run finds
   nothing left to rewrite.

The tool exits 1 without writing when `gh pr list` fails or the archive lists
1000 or more pull requests. To undo, swap a rewritten link back by hand.

## Recovery

A failed step prints `[fail]`, the command, its error output and a pointer to
this section. Ctrl-C or Ctrl-D at the `done` prompt stops before the push.

- A check fails: the run prints `Checks failed: nothing changed.` Fix the cause
  and re-run.
- The run stops after the rename but before the create: undo the rename with
  `gh repo rename factory -R OWNER/factory-private`.
- The run stops after the create: a human deletes the new `OWNER/factory` in
  the GitHub settings, then undoes the rename as above. The script never
  deletes a repository.
- The run stops after the push, for example at a failed `fetch` while
  repointing: do not re-run `--apply`, because the archive-name check now
  fails. Repoint each checkout with `--repoint-only`, as in Other hosts. If
  `repo:` in `<home>/projects/<project>.md` is not `OWNER/factory`, set it,
  then push the project:

  ```sh
  FACTORY_HOME=<factory-home> python3 <installed-skill>/runtime/factory.py push <project>
  ```

  Check the deploy with `gh run list -R OWNER/factory --workflow deploy-main.yml`.
  The next pull request runs `ci.yml`.
- The deploy or CI watch fails after the push: the archive still holds the
  full history. Fix the workflow input or code and rerun the workflow.
