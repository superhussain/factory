---
api_url:
device:
claim_stale_minutes: 120
---
# Device settings

Set `api_url` to your factory HTTPS origin. Import a nonsecret connection
descriptor with `init --connection <file>` or edit this field manually.
An empty `device` uses the hostname. `claim_stale_minutes` is a whole number
of minutes, including 0. Zero never blocks another session and disables the
stale-time test for stuck jobs; dead-session detection still applies.

Optional `claude_agents_dir` and `opencode_agents_dir` fields must be absolute
native agent directories. Roles install only with explicit `init --agents`.

Enter credentials manually in the private `.env` next to this file, using
`FACTORY_ACCESS_CLIENT_ID` and `FACTORY_ACCESS_CLIENT_SECRET`. Never put their
values in this document, command arguments, chat, screenshots or Git.
