# Changing configuration and URLs

A preview-first procedure for changing deployment profile values or a device's
`api_url`. Every step here runs locally. It writes only Git-ignored files under
`deploy/` and `.build/`, or a backup beside a device's `config.md`. This runbook
never deploys, never sets repository variables, and never touches DNS, R2 or
Access. The apply step hands off to the operator gates in
[cloudflare.md](cloudflare.md).

## What this covers

| Change | File | Owner |
| --- | --- | --- |
| `image_origins` (image publication) | `deploy/profile.json` | Server operator |
| `agent_links` (agent app and web links) | `deploy/profile.json` | Server operator |
| `jira_origin` (Jira key links) | `deploy/profile.json` | Server operator |
| `title` | `deploy/profile.json` | Server operator |
| `origin` (instance URL) | `deploy/profile.json`, then each device's `config.md` `api_url` | Server operator, then each device host |

Changing `account_id`, `worker_name`, `bucket_name`, `issuer` or `audience` moves
storage or authentication. Follow [cloudflare.md](cloudflare.md) and the backup
gate in [recovery.md](recovery.md) for those instead.

Run every block from the checkout root in one shell, so `$stamp` stays set. If
you open a new shell, set `stamp` to the value you noted.

## 1. Back up

```sh
stamp=$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p deploy/backups
cp -p deploy/profile.json "deploy/backups/profile.$stamp.json"
python3 tools/deploy.py render --profile deploy/profile.json
cp -p .build/wrangler.production.json "deploy/backups/wrangler.$stamp.json"
cp -p .build/connection.json "deploy/backups/connection.$stamp.json"
```

If the instance deploys from the `deploy-main` workflow, also save the live
repository variable. This command only reads:

```sh
gh variable get FACTORY_DEPLOY_PROFILE -R <owner>/<repo> > "deploy/backups/variable.$stamp.json"
```

On each device whose `api_url` will change, back up its config on that host:

```sh
cp -p <factory-home>/config.md "<factory-home>/config.md.backup-$stamp"
```

Never copy device files between hosts.

## 2. Preview the diff

```sh
cp -p deploy/profile.json deploy/profile.candidate.json
# edit deploy/profile.candidate.json
python3 tools/deploy.py check --profile deploy/profile.candidate.json
git diff --no-index -- deploy/profile.json deploy/profile.candidate.json
python3 tools/deploy.py render --profile deploy/profile.candidate.json
cp -p .build/wrangler.production.json "deploy/backups/wrangler.candidate.$stamp.json"
git diff --no-index -- "deploy/backups/wrangler.$stamp.json" .build/wrangler.production.json
git diff --no-index -- "deploy/backups/connection.$stamp.json" .build/connection.json
```

`git diff --no-index` exits 1 when the files differ. That is the expected
preview, not an error. Read every changed line. Only the keys you meant to
change may differ, and `connection.json` changes only when `origin` changes.
If anything else moved, delete the candidate and start again.

## 3. Verify rendering with integrations configured and disabled

The candidate render above shows your configured image publication. Render the
same candidate with both optional integrations disabled, to confirm the core
settings do not depend on them:

```sh
python3 -c "import json, sys; p = json.load(open(sys.argv[1])); p.update(image_origins=[], agent_links={}); json.dump(p, open(sys.argv[2], 'w'), indent=2)" deploy/profile.candidate.json deploy/profile.disabled.json
python3 tools/deploy.py check --profile deploy/profile.disabled.json
python3 tools/deploy.py render --profile deploy/profile.disabled.json
git diff --no-index -- "deploy/backups/wrangler.candidate.$stamp.json" .build/wrangler.production.json
```

Expect only two values to change: `FACTORY_IMAGE_ORIGINS` becomes `"[]"` and
`FACTORY_AGENT_LINKS` becomes `"{}"`. Any other difference is a problem.

Then check the dashboard source renders both ways. These suites use synthetic
values and bind no port:

```sh
mise exec -- node tools/assets.mjs --offline
mise exec -- node --test tests/worker/entry-auth.test.mjs
mise exec -- python3 -m unittest discover -s tests/tools -p test_deploy.py
python3 tools/deploy.py dry-run --profile deploy/profile.candidate.json
```

`entry-auth.test.mjs` renders the dashboard page with image origins configured,
agent links configured, and both omitted. `test_deploy.py` renders profiles with
image publication configured and with both integrations disabled. `dry-run`
bundles the candidate and never contacts production storage.

## 4. Apply (operator-gated)

This runbook stops here. Applying is your own gated step.

- Widen first, narrow last. When you add an image origin or a new `origin`,
  deploy the profile that adds it before anything links to it. When you remove
  one, stop using it first, then deploy the profile without it.
- For local deploys, copy `deploy/profile.candidate.json` over
  `deploy/profile.json`, then follow
  [cloudflare.md §5](cloudflare.md#5-render-verify-deploy-hand-off).
- For workflow deploys, update `FACTORY_DEPLOY_PROFILE` and rerun as
  [cloudflare.md §8](cloudflare.md#8-continuous-deployment-from-main) describes.
- Changing `origin` also moves the Worker's custom domain; read the DNS effects
  in §8 first. Once the new origin answers an authenticated request, edit
  `api_url` in each device's `config.md` by hand (`init --connection` refuses to
  replace a different endpoint), then run
  `python3 <installed-skill>/runtime/factory.py doctor` on that device.

## 5. Roll back

```sh
cp -p "deploy/backups/profile.$stamp.json" deploy/profile.json
python3 tools/deploy.py check --profile deploy/profile.json
python3 tools/deploy.py render --profile deploy/profile.json
git diff --no-index --exit-code -- "deploy/backups/wrangler.$stamp.json" .build/wrangler.production.json
```

Exit 0 with no output proves the rendered config matches the pre-change render.
Then redeploy through the same gate you applied with: §5 locally, or restore
`FACTORY_DEPLOY_PROFILE` from `deploy/backups/variable.$stamp.json` and rerun
([cloudflare.md rollback](cloudflare.md#failed-deploys-reruns-and-rollback)).

On each device, copy `config.md.backup-$stamp` back over `config.md` and run
`doctor`. When reverting an `origin` change, revert devices first, then the
profile, the reverse of the apply order.

## 6. Retention

- Keep `deploy/backups/` until the change is verified live and you no longer
  need a rollback. Git ignores it, and nothing deletes it for you.
- Keep each device's `config.md.backup-<stamp>` on its own host.
- Delete `deploy/profile.candidate.json` and `deploy/profile.disabled.json`
  after applying or abandoning the change.
