# Repository security

The `main` ruleset lives in `.github/rulesets/main.json`, and this guide covers
applying it and the other repository security settings.

## Main ruleset

- Nobody can force-push or delete `main`, and there are no bypass actors.
- Every change reaches `main` through a pull request, with zero approvals required.
- The `ci` check from GitHub Actions must pass before merge. Strict mode is off,
  so a pull request does not need a rebase after every merge.

```sh
# Apply
gh api -X POST repos/superhussain/factory/rulesets --input .github/rulesets/main.json
# Update
gh api -X PUT repos/superhussain/factory/rulesets/<id> --input .github/rulesets/main.json
# Verify, prints ["deletion","non_fast_forward","pull_request","required_status_checks"]
gh api repos/superhussain/factory/rules/branches/main --jq '[.[].type] | sort'
# Emergency off
gh api -X PUT repos/superhussain/factory/rulesets/<id> -f enforcement=disabled
```

If you rename the `ci` job in `.github/workflows/ci.yml`, update `main.json` in the same PR and re-run the update command after merge.

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
