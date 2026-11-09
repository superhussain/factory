# Self-hosting on Cloudflare

A complete operator recipe for configuring your own Cloudflare instance from this
checkout. Every command that touches your account is an example you run as the
operator; this repository's own verification never performs them. Placeholder
values like `<your-worker-name>` are intentionally invalid until you fill them.

## Guided setup

`tools/bootstrap.py` stands up an instance in one command. Run it from this
checkout after the two install commands in section 1:

```sh
mise exec -- python3 tools/bootstrap.py up
```

**API token.** Create a Cloudflare API token scoped to your account and zone with
these permissions:

- Account: Workers Scripts Edit, Workers R2 Storage Edit, Access: Apps and
  Policies Edit, Access: Service Tokens Edit, Access: Organizations, Identity
  Providers, and Groups Read
- Zone: Workers Routes Edit, DNS Edit, Zone Read

`up` reads the token from `CLOUDFLARE_API_TOKEN`, or asks with a hidden prompt.
It never writes the token to a file or prints it.

**Inputs.** `up` asks four questions and saves the answers in the ignored
`deploy/bootstrap.json`. A short hint before each prompt says where to find the
value. Each prompt shows the saved value as its default.

| Prompt | Example |
| --- | --- |
| Cloudflare account ID | `00000000000000000000000000000000` |
| Zone, for example example.test | `example.test` |
| Factory hostname | `factory.example.test`, the default is `factory.<zone>` |
| Allowed emails, comma separated | `ada@example.test, grace@example.test` |

**What gets created.** Names carry the exact hostname, so two hostnames never
share one. On a host named `laptop`:

| Resource | Name |
| --- | --- |
| R2 bucket and Worker | `factory-example-test` |
| Self-hosted Access application | `factory.example.test bootstrap` |
| Allow policy for the emails | `factory.example.test bootstrap people` |
| Service token and Service Auth policy | `factory.example.test bootstrap device laptop` |

`up` first runs `factory.py init --connection` so this device points at the
hostname. It writes the service token's client ID and secret to the factory
home `.env` (mode 0600) as soon as the token exists. If that write fails, `up`
deletes the new token and stops, so no token is left that nobody holds. It
then writes `deploy/profile.json` with the Access issuer and audience and
deploys through `tools/deploy.py deploy`. Run
`python3 tools/bootstrap.py plan --answers deploy/bootstrap.json` to print the
names without contacting Cloudflare.

**Reruns.** `up` matches every resource by name and keeps no state file. A
second run creates nothing and changes no file. It updates the policies with
the current emails. If the service token exists but this device's `.env` does
not hold it, `up` stops; delete the token under Zero Trust > Access > Service
credentials and rerun. `up` never rotates a token. `up` keeps every policy
already on the application, in its order, and appends new device policies at
the end. It leaves every other application setting as it found it. If an
application for the hostname exists under another name, `up` stops; rename it
to `<hostname> bootstrap` to let `up` manage it.

**A second device.** Run `up` on the other host with the same answers. It
creates that host's token and policy and adds the policy to the application.

**Teardown.** `python3 tools/bootstrap.py down` asks you to type the hostname,
then deletes the custom domain, the Worker, the application, the service tokens
and the policies. It deletes only Access names that start with
`<hostname> bootstrap`, and deletes tokens before policies, so you can rerun it
after a failure. It keeps an application under another name. It deletes the
bucket only when it is empty, and never lists or deletes objects. Local files
stay; the output names the ones that now point at a deleted instance.

Sections 1 to 8 below are the manual fallback.

## 1. Local tooling and account

Install mise, then provision the pinned Node and pnpm versions (`mise.toml`,
`.node-version`, `package.json` `packageManager`) from this checkout:

```sh
mise install
mise exec -- pnpm install --frozen-lockfile
```

Only esbuild `0.28.2` and workerd `1.20261023.1` have approved install hooks;
fsevents `2.3.3` is denied. Browser provisioning is separate and explicit; see
[development.md](development.md) for that step and dependency rollback.
The local pinned Wrangler is now available. Wrangler always runs through the
checkout's locked
executable (`node_modules/.bin/wrangler`); never install Wrangler globally or
download it ad hoc.

Log in locally and confirm the intended account — administrative commands, run by
you, as the operator:

```sh
node_modules/.bin/wrangler login
node_modules/.bin/wrangler whoami
```

Prerequisites for the origin you will serve:

- A domain whose zone is active in your Cloudflare account (for example
  `factory.example.test` in your own zone; the examples here are reserved
  placeholders).
- DNS control of that zone. The Worker uses one custom domain route with a
  custom-domain record; Cloudflare can create or update that record when the
  domain is attached (locally at deploy time, or automatically by the §8
  workflow). If a conflicting record exists at that hostname, resolve or remove
  it before deploy rather than pointing the Worker elsewhere.

## 2. R2 bucket and private profile

Create the bucket (choose a name you own; keep it private):

```sh
node_modules/.bin/wrangler r2 bucket create <chosen-name>
```

Create the ignored private profile `deploy/profile.json` with your values for
exactly these fields: `version` (`1`), `account_id`, `worker_name`, `bucket_name`,
`origin`, `title`, `issuer`, `audience`, `image_origins`, `agent_links`. Every
key is required; set `image_origins` to `[]` and `agent_links` to `{}` to run
with those integrations disabled.
Optional `jira_origin`, such as `https://your-site.atlassian.net`, turns Jira keys in job files into links to `<jira_origin>/browse/<KEY>`; leave it out to keep keys as text.
`python3 tools/deploy.py check --profile deploy/profile.json` validates the file
without writes; `tests/fixtures/deployment-valid.json` shows the shape with
synthetic values.

The generated config projects your bucket onto the fixed `BUCKET` binding, sets
one custom-domain route for your origin's hostname, and disables both
`workers.dev` and preview URLs. The instance is reachable only at your configured
HTTPS origin.

Arrange the Access policies (step 3) before treating a new instance as usable.
Until a request carries a valid signed assertion and the Worker has its required
configuration, every request is denied: missing assertion or configuration fails
closed even before Access itself is fully set up.

## 3. Access application and policies

In the Cloudflare dashboard (Zero Trust), create a self-hosted Access application
for the **whole hostname** (all paths) of your origin:

- **Identity policy (Allow):** the intended human users for browser access.
- **Service Auth policy:** one per device's service token. Each device gets its
  own distinct policy entry; revoke or rotate them independently.
- No **Bypass** policy. A Bypass would exempt paths from assertion validation.

From the application, discover your team issuer (an origin like
`https://<team>.cloudflareaccess.com`) and the application's AUD string. Both go
into `deploy/profile.json` as `issuer` and `audience`.

Three credential shapes, three different transports:

- **Browser sessions** present an Access cookie set after identity login; the
  Worker never reads it as identity — it still requires the signed assertion.
- **Devices** send their Service Auth headers to Cloudflare Access; Access
  validates the token and supplies the signed `Cf-Access-Jwt-Assertion` header to
  the Worker. The device itself never holds a JWT.
- The Worker reads only `Cf-Access-Jwt-Assertion` and verifies it against your
  issuer's JWKS with the configured audience. A cookie alone, garbage assertion,
  device service-token headers alone, or any other header is not origin identity.

## 4. Device secret handling

Each device stores its Service Auth credentials once, in the device's private
`.env` beside its `config.md`, using the exact names `FACTORY_ACCESS_CLIENT_ID`
and `FACTORY_ACCESS_CLIENT_SECRET` with mode `0600` (see `docs/device.md`).
Create the secret once and enter it on the device that needs it; never copy
credentials between hosts — updating one host's credentials must not change
another's.

Treat every secret value as opaque: the older 64-hex form and the modern
`cfast_…` form are both accepted verbatim. Cover the credential lifecycle from
the dashboard: expiry and renewal, rotation with its grace period, disabling and
revocation. Rotation is the right response to any suspected exposure.

Access strict mode means a request without a client cookie is rejected 401/403
even for a user inside the Allow policy; devices are unaffected because they use
header-only Service Auth. Never suggest disabling strict mode as a fix.

## 5. Render, verify, deploy, hand off

From the checkout, with your private profile:

```sh
python3 tools/deploy.py check --profile deploy/profile.json
python3 tools/deploy.py render --profile deploy/profile.json
python3 tools/deploy.py dry-run --profile deploy/profile.json
python3 tools/deploy.py deploy --profile deploy/profile.json
```

`render` writes `.build/wrangler.production.json` and the connection descriptor
`.build/connection.json` — exactly `{"version":1,"api_url":"<your origin>"}`, no
secrets. That descriptor is the nonsecret handoff to devices: import its
`api_url` into device config (`docs/device.md`), or set `api_url` manually.
Never put tokens in argv, URLs, Markdown, reports or client-visible variables.

Credentialed verification uses a private curl configuration or header file so no
secret appears in shell arguments or output:

```sh
# ~/private/factory-curl.conf (mode 0600), never committed:
# header = "CF-Access-Client-Id: <client-id>"
# header = "CF-Access-Client-Secret: <client-secret>"
curl -q --config ~/private/factory-curl.conf https://<your-origin>/api/board
```

Keep the file on the host, out of any repository, and do not print request
headers (they contain the credential).

## 6. Disposable-instance release checklist

Verify a fresh instance with a throwaway bucket/profile before trusting it, using
your own browser and a device service token:

- Browser identity login to the origin reaches the board; unauthenticated asset
  and API requests are denied; an assertion from another host's origin is denied.
- Header-only Service Auth: a plain GET succeeds; a dedicated conditional
  create/edit/readback roundtrip returns exact bytes and ETag; a stale
  conditional edit returns 412.
- A denied or revoked token is rejected with zero storage side effects.
- Every asset path and every route is denied unauthenticated.

Browser access can start with an edge 302 to the identity login — expected.
Strict Service Auth failures surface as 401/403 errors. These real-flow proofs
are distinct from this repository's local crypto tests, which exercise the same
verifier against local workerd, not live Access. Never treat forged headers seen
through Access as proof of origin-forgery rejection: Access may replace or strip
client-supplied headers.

## 7. Behavior notes and release boundary

- The local test entries (`tests/support/local-entry.js`, the workerd auth
  wrapper) are test-only. Production builds exclude them and no deployment
  profile or config can select them.
- Denial responses are `factory unavailable` (invalid required configuration) or
  `forbidden` (origin mismatch or failed verification), both `text/plain`,
  `no-store`, with no verifier detail disclosed.
- Diagnosis order for a denial: check the browser/device reaches the right origin
  (DNS), then issuer and audience in the profile, then clock skew (30-second
  tolerance), then JWKS reachability (`<issuer>/cdn-cgi/access/certs`) and key
  freshness. The Worker keeps resolved issuer keys in memory for ten minutes with
  a five-second refresh cooldown; a still-fresh cached key can verify during a
  brief issuer outage, but missing, stale or unusable keys deny with no stale
  bypass.
- Drafts are bound to the origin's storage, not to any client; preserving the
  same origin across deploys preserves drafts. Legacy job-file writes without
  conditions remain possible for old clients and are unsafe under concurrent
  writers; new devices always send conditions.
- The dashboard installs to a phone's home screen through Access. The manifest,
  icons and `/sw.js` sit behind the same Access check as every other route, and
  the page fetches the manifest with credentials. Chrome on Android may fetch the
  icons without the Access cookie; if so, it adds a plain shortcut instead of an
  app icon.
- The service worker caches only the page, `/assets/*`, `/board.json`,
  `/projects.json` and `/jobs/*`, and never `/api/*`. Board and job reads try the
  network first and fall back to this device's saved copy under an offline
  banner. Offline is read-only: writes fail until the connection returns, and
  nothing queues or replays them. The service worker clears its cache on Access
  logout (`/cdn-cgi/access/logout`) and whenever a read meets an Access
  redirect, 401 or 403. A device that goes offline before its next request keeps
  showing its last board until it reconnects.
- Deploying/cutting over a real instance requires reviewed merged source, passing
  tests, four independent reviews, final approval and later release-owner
  authorization. This repository's own verification stops at render, dry-run and
  local checks; the only automated deploy path is the §8 workflow, which the
  release owner enables by provisioning its inputs on the repository (§8
  describes that boundary and its effects in full).

## 8. Continuous deployment from main

The `deploy-main` workflow (`.github/workflows/deploy-main.yml`) deploys every
pushed `main` commit automatically: each run first reconciles against
`origin/main` — it resolves the current head and deploys that, not necessarily
its own pushed SHA — so a stale, reordered or rerun run can never lose the
latest reviewed head even when GitHub's single pending slot displaces the
head's own queued run. It then validates the deployment inputs, installs with
a cached frozen install, and runs an uncached `python3 tools/deploy.py deploy
--profile deploy/profile.json`. The profile is materialized at runtime from one
repository variable into the ignored, temporary `deploy/profile.json` in the
workflow's checkout (never committed); the token comes from one secret and
exists only in step environment variables.

DNS effects: the rendered config attaches your origin's hostname as a Cloudflare
custom domain (`tools/deploy.py` renders `custom_domain: true`), so a deploy can
make Cloudflare create or update the DNS record for that hostname. That is the
boundary: reconciling the already-configured custom domain is in scope; the
workflow never creates other or new DNS records or zones and performs no other
DNS management. Pick a hostname without an existing record, and resolve
conflicts before deploying (step 1). Wrangler publishes the Worker version
before applying the custom-domain trigger, so a failed or timed-out deploy can
leave a partial state: new Worker version live, domain attachment failed. An
origin answering is not proof the intended SHA deployed — an older Worker can
still answer after a partial failure. After any deploy, confirm the run's
Deploy step completed for the intended head SHA, then verify an authenticated
successful response (`curl -fsS --config ~/private/factory-curl.conf
https://<your-origin>/api/board`, which fails on HTTP errors) and that the
custom domain is attached to the Worker. Once the cause of a failure is fixed,
rerun the run — the atomicity guarantee covers the Worker version, not the
domain attachment.

One-time operator setup, after you complete steps 1–6 for the instance:

1. Create the Cloudflare API token with the **Edit Cloudflare Workers** template
   **plus** Account → Workers R2 Storage → Edit, scoped to the single target
   account. The account ID itself comes from the rendered profile, so no
   separate account-id value is needed.
2. Add the token as the repository secret `CLOUDFLARE_API_TOKEN`
   (`gh secret set CLOUDFLARE_API_TOKEN -R <owner>/<repo>`).
3. Add the repository variable `FACTORY_DEPLOY_PROFILE` holding the full profile
   JSON exactly as `deploy/profile.json` (same schema as step 2):
   `gh variable set FACTORY_DEPLOY_PROFILE -R <owner>/<repo> < deploy/profile.json`.
   It is nonsecret instance configuration, validated at runtime by
   `python3 tools/deploy.py check`; keep real values out of the repository.

Until both are provisioned, the workflow fails fast with a diagnostic naming the
missing input and this section. Concurrency coalesces pushes without cancelling
an in-flight deploy.

### Failed deploys, reruns and rollback

Saving a missing or corrected input never re-triggers a push-only workflow, so a
failed deployment stays failed until you act. After fixing the input
(`gh secret set` / `gh variable set` as above), rerun the current `main` run from
the Actions tab (`Re-run jobs`) or `gh run rerun <run-id> -R <owner>/<repo>`: it
reconciles to `origin/main`'s current head and redeploys it with the new inputs.
Rerunning an older run is equally safe: it also deploys the current head, never
its original SHA, so no rerun can roll production back.

Rollback is a manual CLI redeploy of the previous reviewed SHA. A manual deploy
does not hold the workflow's `deploy-main` concurrency lock, so coordinate with
GitHub Actions first:

1. Stop new workflow deploys: disable the workflow (`gh workflow disable
   deploy-main -R <owner>/<repo>`, or Actions tab → ⋮ → Disable workflow), then
   wait for every active run to finish (in-flight deploys are never cancelled)
   and confirm no run is queued (`gh run list --workflow deploy-main`).
2. Check out the previous reviewed SHA, recreate the ignored
   `deploy/profile.json` locally (the same JSON as `FACTORY_DEPLOY_PROFILE`),
   run `pnpm install --frozen-lockfile`, and authenticate Wrangler locally with
   `CLOUDFLARE_API_TOKEN` in the environment.
3. Deploy: `python3 tools/deploy.py deploy --profile deploy/profile.json`, then
   verify as above (Deploy step equivalent: the CLI's own output, the
   authenticated `curl -fsS` check, and domain attachment).
4. Re-enable the workflow (`gh workflow enable deploy-main`). The rollback holds
   only while the workflow stays disabled: rerunning any run deploys
   `origin/main`'s head (reverting the rollback), and pushes made while the
   workflow was disabled trigger no run, so they are not auto-deployed.
   Immediately after re-enabling, reconcile production with `origin/main`'s
   head — trigger a `deploy-main` run (or deploy the head via the CLI) and
   verify it — rather than waiting for the next push.
