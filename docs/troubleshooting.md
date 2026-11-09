# Troubleshooting

Symptoms first seen from the outside, with the safe fix for each. For deeper
context see [cloudflare.md](cloudflare.md), [device.md](device.md) and
[architecture.md](architecture.md).

## Missing endpoint or profile

- Device CLI reports missing endpoint/token: run `init` in the device and set
  `api_url` plus the two Access secrets in the private `.env`
  ([device.md](device.md)). `doctor` reports exactly which check fails.
- Deployment tooling rejects the profile: `deploy/profile.json` is missing or
  invalid. Copy the shape from `tests/fixtures/deployment-valid.json` (synthetic
  values) and validate with `python3 tools/deploy.py check --profile ...` —
  `check` writes nothing.

## Access 401/403

- A browser session inside the Allow policy can still get 401/403: strict mode
  requires the signed assertion on every request. Never disable strict mode or
  add a Bypass policy as a fix.
- Devices get 401/403 when their service token is expired, disabled or rotated:
  renew or rotate the token in the dashboard, then update only that device's
  `.env`.
- Still failing: check the origin hostname, `issuer`/`audience` in the profile,
  clock skew (30-second tolerance) and JWKS reachability
  (`<issuer>/cdn-cgi/access/certs`), in that order
  ([cloudflare.md](cloudflare.md)).

## Offline or sign-in banner

- "You're offline": the dashboard's service worker is showing the board saved on
  this device. Edits won't save. The page re-reads the board and reconnects live
  updates as soon as the browser reports it is back online.
- "Your sign-in has expired": a read met the Access login redirect, a 401 or a
  403, so the service worker cleared its cache. Choose Sign in again to reload
  through Access.
- A stale page after a deploy: the service worker serves the cached page first
  and refreshes it in the background, so the next reload shows the new version.
- The walkthrough frame shows "Open this walkthrough from its job in the dashboard.": a service worker from before the walkthrough fix still controls the page and re-fetches it without its frame header. Close and reopen the installed app, or reload the tab, then choose Show again. Safari and iOS before 16.4 send no frame header at all, so update the device.

## Pull or sync fails

- "the Worker returned no ETag" for a job with walkthrough.html: the deployed Worker predates the no-transform fix, so Cloudflare's edge rewrote the HTML and dropped its ETag. Deploy the current Worker, then pull again.
- "CERTIFICATE_VERIFY_FAILED": this Python found no CA bundle. On macOS the runtime retries with /etc/ssl/cert.pem unless SSL_CERT_FILE or SSL_CERT_DIR is set. Elsewhere, set SSL_CERT_FILE to your system CA bundle, for example /etc/ssl/certs/ca-certificates.crt on Ubuntu, then rerun.

## Occupied ports

Local tests bind `127.0.0.1:8220`/`8221` or owned ephemeral ports. Stop only the
process you own that holds the port; never kill another operator's or job's
service. A stuck owned process can be found with `lsof -i :8220` and restarted
afterwards.

## Browser provisioning failures

`test:browser` needs the explicitly provisioned Chrome for Testing:
`node tests/browser/provision.mjs`. If provisioning fails, check network access
to the Chrome for Testing download and disk space in the ignored
`.context/browser` cache; re-run provisioning rather than deleting the cache by
hand.

## Exit-6 sync conflicts

Exit 6 means another owner, drift, a conflict or an unsafe path was found — the
sync stopped on purpose. Compare the retained local and remote copies, merge or
select explicitly, then delete only the reconciled conflict copy and push
([device.md](device.md) "Exit recovery"). Never force a claim or branch, never
take over another device's claim, and never delete drafts or baselines to make
an error disappear.
