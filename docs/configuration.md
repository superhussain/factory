# Configuration

This page is the reference for the factory's non-secret configuration: each layer, its owner, and which value wins.
Credentials are covered under [Secrets stay out of configuration](#secrets-stay-out-of-configuration).

## Layers and owners

| Layer | Where it lives | Scope | Who writes it | Source |
| --- | --- | --- | --- | --- |
| Deployment profile | Ignored `deploy/profile.json`, or the path given to `--profile` | One hosted instance | Operator | `tools/deploy.py` `load_profile`, `PROFILE_KEYS` |
| CI copy of the profile | Repository variable `FACTORY_DEPLOY_PROFILE`, written to `deploy/profile.json` at run time | Main-push deploys | Operator | `.github/workflows/deploy-main.yml` "Materialize profile" step |
| Worker vars | Rendered `.build/wrangler.production.json` `vars` | Running Worker | `tools/deploy.py render` only | `tools/deploy.py` `project_wrangler`; `worker/assets.js` `readServerConfig` |
| Connection descriptor | `.build/connection.json`, exactly `{"version":1,"api_url":...}` | Handoff to devices | `tools/deploy.py render` | `project_wrangler`; runtime `load_connection` |
| Device config | `<home>/config.md` frontmatter | One device | Device owner | runtime `api_url`, `holder`, agents install (`*_agents_dir`) |
| Device checkout paths | `<home>/paths.md` | One device | `init <repo>` | runtime `init`, `validate_home` |
| Device dispatch routes | `<home>/routes.md` | One device | Device owner, by hand | runtime `read_routes`, `routes` |
| Device launchers | `<home>/launchers.json` | One device | Device owner, by hand | runtime `read_launchers`, `pickup` |
| Device stall adapter | `<home>/stalls.json` | One device | Device owner, by hand | runtime `read_stalls`, `pickup` |
| Device Tracer mirror | `<home>/tracer.json` | One device | Device owner, by hand | runtime `read_tracer`, `tracer` |
| Pipeline pickup | R2 `pipeline/global.json` and `pipeline/projects/<slug>.json` key `pickup` | One hosted instance, overridable per project | Access-authenticated person | `validatePipeline`, `effectivePipeline` |
| Shared project file | R2 `projects/<slug>.md`, cached at `<home>/projects/<slug>.md` | One project, all devices | Dashboard settings or `push <slug>` | `worker/index.js` `projects`, `projectPut`; runtime `pull_project`, `push_project` |
| Project documents | R2 `project-documents/<slug>/{notes,decisions,runbook}.md` | One project | Dashboard document selector | `worker/index.js` `PROJECT_DOCUMENT_NAMES`, `projectDocumentPut` |
| Credentials | `<home>/.env` (POSIX mode 0600 when seeded) | One device | Device owner, by hand | runtime `api`, `read_conf`, `init` seed |

`board.json` and `jobs/...` in R2 are state, not configuration. `<home>/route-limits.json` is device state that `routes limit` and `routes clear` write. `<home>/logs/pickup/` is device state that `pickup` writes, one launch log per idea.

## Device config keys

| Key | Default when empty or absent | Read by | Notes |
| --- | --- | --- | --- |
| `api_url` | none; commands that call the Worker exit 2 | `api_url()` | HTTPS origin; HTTP only for localhost/127.0.0.1 tests (`normalized_origin`) |
| `device` | `socket.gethostname()` | `holder()` | Claim owner name on the shared board |
| `claim_stale_minutes` | `120` | `holder()` | Whole number, 0 allowed |
| `claude_agents_dir` | `~/.claude/agents` | agents install | Absolute path |
| `opencode_agents_dir` | `<config_root>/opencode/agents` | agents install | Absolute path |

Any other key is preserved by `init` and ignored by the runtime. Settings other tools keep in `config.md` (for example an image-host key used by an external publishing skill) are owned and documented by that tool, not by this repository.

Keep each key once. Runtime reads take the last duplicate (`frontmatter`), while home validation and adoption reject duplicates (`closed_frontmatter`).

## Precedence

1. Home: `--home` > nonempty `FACTORY_HOME` > `<config_root>/factory-location.json` > `<config_root>/factory`. See the table in [device.md](device.md#home-selection-and-pointer-only-adoption). (`factory_home`, `config_root`)
2. Session tag: `--session` > `FACTORY_SESSION` > a new random tag printed to stderr. (`main` option parsing, `new_session`)
3. Device settings and credentials have no environment override. The runtime reads `api_url`, `device` and `claim_stale_minutes` only from `config.md`, and the two Access values only from `<home>/.env`. Exporting `FACTORY_ACCESS_CLIENT_ID` in a shell does nothing. (`api`, `read_conf`)
4. Endpoint: `profile.origin` renders to `.build/connection.json`; `init --connection` copies it into `config.md` `api_url` only when that field is empty. A different existing value fails with exit 2 and changes nothing. After that, `config.md` wins. (`init`)
5. Worker vars: the rendered config is the only managed source. `FACTORY_IMAGE_ORIGINS` and `FACTORY_AGENT_LINKS` default to `[]` and `{}` only when absent; any other missing or invalid value returns 503 `factory unavailable`. The rendered config does not set Wrangler `keep_vars`, so a deploy replaces vars edited in the Cloudflare dashboard. (`readServerConfig`, `fetch`)
6. Profiles: main-push deploys use `FACTORY_DEPLOY_PROFILE`; operator deploys use the file passed to `--profile`. Neither reads the other. After editing one, update the other; [cloudflare.md §8](cloudflare.md#8-continuous-deployment-from-main) says the variable holds the same JSON. (`load_profile`, `.github/workflows/deploy-main.yml`)
7. Project file: R2 `projects/<slug>.md` is authoritative. The local cache syncs by ETag compare-and-swap; a concurrent change leaves `<slug>.md.remote` and exits 6 for an explicit merge. (`pull_project`, `push_project`)
8. Layers do not overlap. No key appears in two layers except the endpoint (rule 4), so there is no cross-layer override to resolve. (`project_wrangler`, `init`)

## Secrets stay out of configuration

- Device Access credentials, `FACTORY_ACCESS_CLIENT_ID` and `FACTORY_ACCESS_CLIENT_SECRET`, live only in `<home>/.env`, sent only as the two `CF-Access-Client-*` headers to the validated origin (`api`).
- `CLOUDFLARE_API_TOKEN` lives only as a repository secret for `deploy-main` ([cloudflare.md §8](cloudflare.md#8-continuous-deployment-from-main)).
- The Tracer token stays in the file Tracer wrote. `<home>/tracer.json` holds only that file's path, and the runtime sends the token only as `Authorization: Bearer` to the configured Tracer URL (`tracer_call`).
- Never put a credential in `config.md`, `deploy/profile.json`, the CI variable, `projects/<slug>.md` or project documents. The dashboard renders project files and documents as markdown to every Access-authenticated viewer, and the profile and connection descriptor are nonsecret by design.
- `deploy/profile.json` is listed in `.gitignore` but is not a secret; it holds instance values that never belong in committed source.

## Related

- [device.md](device.md) — device setup, credentials and home selection
- [cloudflare.md](cloudflare.md) — instance setup, profile and continuous deployment
- [architecture.md](architecture.md) — ownership and request boundary
- [integrations.md](integrations.md) — optional native roles and schedules
