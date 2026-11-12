# Repository security

The `main` ruleset lives in `.github/rulesets/main.json`, and this guide covers
applying it and the other repository security settings.

## Main ruleset

- Nobody can force-push or delete `main`, and there are no bypass actors.
- Every change reaches `main` through a pull request, with zero approvals
  required.
- The `ci` check from GitHub Actions must pass before merge. Strict mode is off,
  so a pull request does not need a rebase after every merge.
- The `ci` check guards against accidents, not against a hostile pull
  request. `ci.yml` runs on `pull_request`, so a pull request from a writer
  or from a fork can redefine its own `ci` job to always pass. Read workflow
  changes in any pull request before merging, since a green `ci` does not
  prove the tests ran.

```sh
# Apply
gh api -X POST repos/superhussain/factory/rulesets --input .github/rulesets/main.json
# Find the ruleset id, used as <id> below
gh api repos/superhussain/factory/rulesets --jq '.[] | select(.name=="main") | .id'
# Update
gh api -X PUT repos/superhussain/factory/rulesets/<id> --input .github/rulesets/main.json
# Verify, prints ["deletion","non_fast_forward","pull_request","required_status_checks"]
gh api repos/superhussain/factory/rules/branches/main --jq '[.[].type] | sort'
# Emergency off, which also lifts deletion and force-push protection
gh api -X PUT repos/superhussain/factory/rulesets/<id> -f enforcement=disabled
# Re-enable after the emergency
gh api -X PUT repos/superhussain/factory/rulesets/<id> -f enforcement=active
```

If you rename the `ci` job in `.github/workflows/ci.yml`, update `main.json`
in the same PR and re-run the update command after merge.

## Settings checklist

| Setting | Now (2026-11-09) | Recommended | Command or UI path | Who |
| --- | --- | --- | --- | --- |
| Secret scanning, push protection | on | keep | `gh api repos/superhussain/factory --jq .security_and_analysis` | agent verifies |
| Dependabot alerts | off | on | `gh api -X PUT repos/superhussain/factory/vulnerability-alerts` | agent |
| Private vulnerability reporting | off | on | `gh api -X PUT repos/superhussain/factory/private-vulnerability-reporting` | agent |
| Fork PR workflow approval | `first_time_contributors` | `all_external_contributors` | `gh api -X PUT repos/superhussain/factory/actions/permissions/fork-pr-contributor-approval -f approval_policy=all_external_contributors` | agent |
| Delete branch on merge | off | on | `gh api -X PATCH repos/superhussain/factory -F delete_branch_on_merge=true` | agent |
| Default workflow token | read, cannot approve PRs | keep | `gh api repos/superhussain/factory/actions/permissions/workflow` | agent verifies |
| Dependabot security updates | on | keep | `gh api -X PUT repos/superhussain/factory/automated-security-fixes` | agent verifies |
| Allowed Actions | all | GitHub-owned plus `pnpm/action-setup@*` | Settings, Actions, General, Actions permissions | Hussain |
| Require SHA-pinned Actions | off | on; actions pinned in workflows, Dependabot bumps them | `gh api -X PUT repos/superhussain/factory/actions/permissions -F enabled=true -f allowed_actions="$(gh api repos/superhussain/factory/actions/permissions --jq .allowed_actions)" -F sha_pinning_required=true` (keeps the current `allowed_actions`) | agent, after merge |
| Wiki, Projects | off | keep | `gh api -X PATCH repos/superhussain/factory -F has_wiki=false -F has_projects=false` | agent verifies |
| Deploy secret scope | repo secret; production environment exists (main only); deploy job uses it | environment secret only | `gh secret set CLOUDFLARE_API_TOKEN --env production -R superhussain/factory`, then after one green deploy `gh secret delete CLOUDFLARE_API_TOKEN -R superhussain/factory` | Hussain |

When you review a Dependabot PR that bumps an action, check that the new SHA is the commit its `# vX.Y.Z` release tag points to.

## Account hygiene

- Hussain: turn on 2FA with a passkey or security key at github.com/settings/security.
- Hussain: review the `gh auth status` scopes.
- Hussain: remove unused tokens at github.com/settings/tokens and apps at github.com/settings/applications.
- Hussain: keep `CLOUDFLARE_API_TOKEN` scoped to Workers deploy only.

## Pipeline stage notes

This repository is public, so the `factory` project pipeline adds a review note and a build note. Agents read them inside the operator-preferences envelope described in [trust.md](trust.md). `tests/worker/public-repo-notes.test.mjs` checks that both notes pass `validatePipeline`. `tools/stage-notes-check.py` compares the live notes with this file.

### Review stage note

```text
This repository is public. Every reviewer owns item 1. The security reviewer also owns items 2 to 4. Report each miss as a finding with a concrete fix. Read the diff, the commit messages (git log origin/main..HEAD) and build.md, since the PR description draws on it.

1. Nothing private. No tokens, keys, Access client ids or AUD tags, Cloudflare account ids, real instance hostnames, bucket names or internal URLs, real email addresses, and no employer, client or other private project names. No reference to the private history archive. Fixtures, tests and screenshots use synthetic values, such as example.test addresses and made-up board rows.
2. Workflows. Every third-party action is pinned to a full commit SHA. Each workflow or job grants the least permissions it needs. No pull_request_target or workflow_run trigger checks out or runs pull request code. Secrets reach only a job bound to the production environment. Downloaded tools are checksum-verified, as ci.yml does for gitleaks.
3. Dependencies. A new package needs the spec's approval, an exact version in package.json and a lockfile resolved from the public registry. Code that ships in the Worker or dashboard bundle gets a THIRD_PARTY_NOTICES.md entry with its license text.
4. Trust model. A diff that loosens FLOORS or the admission patterns in worker/admission.js, Access verification in worker/access.ts, the operator-preferences envelope sentence, SECURITY.md or the main ruleset is a finding unless the spec names that change.
```

### Build stage note

```text
This repository is public. Everything you commit, every commit message and the build.md text the PR description draws on is public.

- Use synthetic values only: example.test addresses, placeholder hostnames and ids, made-up board rows in fixtures and screenshots.
- Never name an employer, client or other private project, a real instance hostname, or the private history archive.
- A new or changed workflow pins third-party actions to a full commit SHA, grants the least permissions it needs, and reads secrets only in a job bound to the production environment.
- Add no dependency the spec does not name. One that ships in the Worker or dashboard bundle gets a THIRD_PARTY_NOTICES.md entry.
- Before each commit, read git diff --cached for the items above.
```

### Apply and verify

Only a signed-in person saves pipeline settings (F11); retro admit may edit only stage notes. Apply after a PR that changes a note merges.

1. Open the dashboard at `#/settings/pipeline`.
2. In Scope, pick the `factory` project. Its option reads `factory`, or `<title> (factory)` when the project has a title. Then select Load latest.
3. On the Review tab, paste the review note. On the Build tab, paste the build note. Leave every other field as loaded.
4. Select Save. History lists the new version.
5. On a device, from the root of a checkout of this repository at current `main`, run this command. `<installed-skill>` is the `factory` folder that `tools/install.py` created, see [device.md](device.md). It prints `review: matches` and `build: matches`. A `differs` or `missing` line means the live note is out of date, so repeat steps 1 to 4. A `no note in docs/repo-security.md` line means this checkout lost its note block, so restore that file from current `main`. If `factory.py` prints an error first, fix that error and run the command again. Do not re-paste the notes for it. If the `stdin is not a factory.py pipeline view` line prints with no error before it, the command before the pipe is wrong. Copy the command as shown and change only `<installed-skill>`.

```sh
python3 <installed-skill>/runtime/factory.py pipeline factory | python3 tools/stage-notes-check.py
```

To change a note, edit it here in a pull request first, then repeat these steps.
