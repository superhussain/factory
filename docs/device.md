# Device setup

## Prerequisites and copy installation

Use Python 3.8+ and Git. No pip, npm, daemon or native agent runtime is required
for the device CLI. The repository installer copies only the self-contained skill.
Choose an absolute destination directory, then run from this checkout:

```text
python3 tools/install.py --destination <absolute-directory>
```

The installed skill is `<absolute-directory>/factory`. Load its SKILL.md through
your agent runtime, or invoke its Python CLI directly from any working directory:

```text
python3 <installed-skill>/runtime/factory.py init
```

On Windows use `py -3` instead of `python3`, quoting paths containing spaces.

Use this installer on every host.

The copied bundle works when the source checkout is unavailable. It includes all
templates, nine canonical role bodies, execution references and optional prompts.
There are no repository-relative runtime reads or runtime dependency downloads.

An identical install is a no-content-change success. A differing existing install
requires explicit replacement:

```text
python3 tools/install.py --destination <absolute-directory> --replace
```

Replacement moves the old bundle to `factory.backup-<uuid>` beside the installed
directory, switches in the new one, then deletes that backup and prints
`removed <path>`. A failed switch restores the old bundle. If the delete fails,
the install still succeeds and warns
`install.py: could not remove <path>; remove it by hand`. It never merges trees
or deletes older backups. Copy out any local edits before `--replace`. A completed
sibling stage is switched only after validation. Pause other installers and
editors during replacement. `.factory-install.lock` serializes cooperating
installers, not arbitrary external filesystem writers. An existing lock fails
closed; inspect its owner and resolve it manually, never auto-break a stale lock.
The installer touches no device state, native roles or schedules. It leaves other skills alone, except that it removes a `huss-factory` link beside `factory`, the retired runtime that skips the job id check. A real `huss-factory` folder stays, with a warning to remove it by hand.

## Offline initialization and credentials

`init` seeds only absent `config.md` and `.env`, prints `factory home: <home>` and
reports missing endpoint/token with success exit 0. No network or Git registration
is attempted until both are configured. No roles install implicitly.

Fresh config contains only `api_url`, `device` and `claim_stale_minutes: 120`.
An empty device uses the hostname. Stale minutes includes 0; zero never blocks
another claim and disables time-based stuck detection, not dead-session checks.
Existing unknown config fields, credentials and permissions remain unchanged.

Set `api_url` manually to your HTTPS origin in device config, or import a nonsecret
descriptor containing exactly `{"version":1,"api_url":"https://example.invalid"}`:

```text
python3 <installed-skill>/runtime/factory.py init --connection <connection.json>
```

The descriptor is always HTTPS and contains no credentials or administrator data.
Import seeds only an absent/empty endpoint. An optional single trailing slash is
equivalent. A differing configured endpoint fails with exit 2 before any write.
Userinfo, non-root paths, query, fragment, controls and malformed ports are rejected.
Plain HTTP is allowed only for localhost or 127.0.0.1 synthetic tests, not production.

Enter `FACTORY_ACCESS_CLIENT_ID` and `FACTORY_ACCESS_CLIENT_SECRET` manually in
the private `.env` beside config.md. Never enter token values in argv, chat,
screenshots, logs or Git. Both older and modern secret formats are opaque strings;
the CLI sends them only in the two Access headers to the validated origin and
does not follow Access redirects. Devices need no Cloudflare administrator profile.
A device that manages the instance may also hold the guided-setup API token as `CLOUDFLARE_API_TOKEN` in the same `.env` ([cloudflare.md](cloudflare.md#device-token)).
Fresh POSIX `.env` is mode 0600. Repeated init preserves existing modes rather than
silently repairing them. Review existing ownership/permissions yourself. Windows
uses the user's directory ACL; POSIX mode bits do not promise Windows secrecy.

## Home selection and pointer-only adoption

| Priority | Location |
| --- | --- |
| 1 | Explicit `--home <absolute-path>` anywhere in the CLI invocation |
| 2 | Nonempty `FACTORY_HOME` |
| 3 | `<config_root>/factory-location.json` |
| 4 | `<config_root>/factory` |

On macOS/Linux `config_root` is absolute nonempty `XDG_CONFIG_HOME`, otherwise
`Path.home()/.config`. On Windows the same rule uses the user profile home,
normally `<USERPROFILE>/.config`; APPDATA is not selected implicitly. Empty
environment values mean unset. Relative explicit home/XDG values fail with exit 2.
Canonicalization creates nothing. Invalid pointers never fall back to an empty home.
A command validates a pointer-selected home once, then reuses it until it exits.

Existing state stays where it is. Explicitly select it offline:

```text
python3 <installed-skill>/runtime/factory.py adopt-home <existing-absolute-directory>
```

Do not pass `--home` to adoption. It validates complete flat config/paths metadata,
project baselines and job manifests read-only, then exclusively publishes only the
generic pointer. The pointer contains exactly version 1 and a canonical home.
It changes no credentials, labels, claims, cache bytes, `.remote` files, permissions,
checkout paths, worktree registration or Git metadata. Dirty/conflicted caches are
valid retained state, not repair candidates. Checkout contents are not inspected.

A different pointer or initialized generic default home blocks adoption, including
a malformed default config. Identical adoption is idempotent and rechecks conflicts.
The pointer uses a mode-0600 same-directory temporary, flush/fsync and exclusive
hard-link publication. Unsupported atomic hard links fail closed with no fallback.
Readers see no pointer or a complete pointer. On Windows confidentiality relies
on the existing directory ACL. Adoption installs no roles or schedules.

Legacy `claim <factory-dir> ...` and `board <factory-dir> ...` retain authoritative
positional paths, including relative ones. An explicit `--home` must agree
canonically; environment/pointer defaults never replace those operands.
Global `--home` and `--session` each occur once with a nonempty value.

## Read-only doctor and project registration

```text
python3 <installed-skill>/runtime/factory.py doctor
```

Doctor emits one versioned JSON report with `home`, `api_url` and ordered location,
config, credentials, projects and board checks. Invalid location emits only that
failed check. It writes no files, claims, caches or roles. Invalid local config
prevents all requests; otherwise it GETs only `/projects.json` and `/api/board`.
An absent board is success and is not initialized. Reports never contain tokens,
headers, response bodies or raw transport exceptions. Doctor proves local setup
and read reachability, not JWT cryptography, write permission, deployment identity
or whole-Worker attestation.

After entering endpoint/token, register each unrelated checkout explicitly:

```text
python3 <installed-skill>/runtime/factory.py init <repo-path>
```

`init <repo>` is the sole registration flow, not a separate register verb.
It creates the remote project only with `If-None-Match: *`; existing projects
reconcile safely without overwriting drafts. Device checkout paths stay in
`paths.md`, not the shared project header. Fill the project context from its
actual repo, then use project push. Do not put credentials in project documents.

## Working commands

```text
python3 <installed-skill>/runtime/factory.py where sample
python3 <installed-skill>/runtime/factory.py claim <factory-home> small-feature sample
python3 <installed-skill>/runtime/factory.py sync sample
python3 <installed-skill>/runtime/factory.py handoff sample/001-small-feature --session <printed-tag>
python3 <installed-skill>/runtime/factory.py clean sample/001-small-feature node_modules
python3 <installed-skill>/runtime/factory.py clean sample/001-small-feature temp
python3 <installed-skill>/runtime/factory.py orphans
```

Use the allocated ID, not the illustrative ID above. Claims without a supplied
session print a new tag to stderr; reuse it for later ownership calls. `where`
prints the ten v1 path/project keys as JSON. `sync` preserves drafts and conflict
copies. It skips `merged` and `closed` jobs; `pull <slug>/<id>` fetches one.
Handoff requires a quiescent checkpoint, clean published branch and the
owning session. It uploads artifacts before releasing the claim. Resume follows
the installed [execution guide](../skills/factory/references/execution.md), never
an automatic takeover or reset of review/approval counters.
`clean` removes only git-ignored `node_modules` folders, or one `.context/<name>`
scratch folder, inside the job's own worktree, never through a link, and prints
`{"removed": [...], "left": [...]}` as JSON.
`temp` removes the job's `factory-<slug>-<job-id>` folder under the system temp root, the
`Temp folder:` that every role prompt names. `prompt` creates it with mode 0700 and
stops when a link, a file or another account holds the name. It checks the name again with
`lstat` after creating it and changes the mode through a handle that does not follow links.
`orphans` lists `factory/*` branches on origin, open factory PRs, job worktrees under
`<home>/worktrees` and Paseo workspaces named `factory-<id>` that no open board job owns, each
with its reason: `no job on the board`, `job merged` or `job closed`. It prints
`{"orphans": [...], "skipped": [...]}` and only reads; a source that fails becomes one
`skipped` line. Pickup runs it once per local date, recorded in `<home>/orphan-scan.json`,
and prints one `orphan scan:` summary line.
A workspace with no board job gets the `project` of the first `paths.md` checkout, by slug,
whose `git rev-parse --git-common-dir` matches the workspace's, since a Paseo worktree path
carries no slug. A workspace whose folder is gone, or matches no checkout, reports `null`.
Within one command, the runtime reuses a 200 answer from /projects.json, /api/pipeline/global or /api/pipeline/effective/<slug> for up to 60 seconds. A pipeline or project write clears it.

## Cloudflare credentials for one command

`with-cloudflare` runs one command with the `CLOUDFLARE_*` keys of the factory
home `.env` added to its environment. It passes no other `.env` key, prints no
value and runs the command without a shell. Wrap a command in `sh -c` when it
needs the token by name:

```text
python3 <installed-skill>/runtime/factory.py with-cloudflare -- sh -c 'printf "Authorization: Bearer %s\n" "$CLOUDFLARE_API_TOKEN" | curl -H @- https://api.cloudflare.com/client/v4/user/tokens/verify'
```

`printf` is a shell builtin, so the token reaches curl on stdin and never shows in a process list.

| Case | Exit | stderr |
| --- | --- | --- |
| no `--` or no command | 2 | `factory.py: usage: factory.py with-cloudflare -- <command> [<arg>...]` |
| no `.env` | 5 | `factory.py: no <env path>. docs/cloudflare.md, Device token, shows how to store CLOUDFLARE_API_TOKEN there.` |
| `.env` is a link or not a regular file | 5 | `factory.py: <env path> is a link or not a regular file, so it was not read. Replace it with a regular file.` |
| `.env` unreadable | 5 | `factory.py: cannot read <env path>: check its permissions and that it is UTF-8 text.` |
| no token | 5 | `factory.py: CLOUDFLARE_API_TOKEN missing from <env path>. docs/cloudflare.md, Device token, shows how to store it.` |
| command not found | 127 | `factory.py: cannot run <command>` |

Otherwise it exits with the command's code, or `128 + N` when signal `N` ends it.

## Session retro digest

```text
python3 <installed-skill>/runtime/factory.py retro digest [--days N]
python3 <installed-skill>/runtime/factory.py retro push
python3 <installed-skill>/runtime/factory.py retro pull YYYY-MM-DD
python3 <installed-skill>/runtime/factory.py retro aggregate [--days N]
python3 <installed-skill>/runtime/factory.py retro admit <run>
```

`retro digest` prints one JSON line per Claude Code main session whose
transcript changed in the last N days (default 1, at most 90). It reads
`~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects` when that is set, and
takes the host name from `device:` in `config.md`, else the hostname. Each row
counts turns, tokens once per message, tools, errors and denials, and folds
subagents in by type. It writes nothing and makes no network call. Error and
denial excerpts are redacted, have paths and digits normalized, and are clipped
to 160 characters. Header values, URL passwords, bearer tokens, `curl -u` user:pass,
a password glued to `mysql -p`, the `sshpass -p` password, `MYSQL_PWD`,
secret-named values and the value after a secret-named `--` flag become
`[redacted]`. The home prefix of `project`, through this host's user name,
becomes `~`, and every string and key is cut to 160 characters. A
transcript deleted mid-run is skipped. A window with no sessions prints
nothing and exits 0. A missing projects directory exits 4.

`retro push` sends the one-day digest to `PUT /api/retro/<device>/<date>.jsonl`,
where the date is this host's local date, and prints
`pushed N sessions to retro/<device>/<date>.jsonl`. It needs `device:` in
`config.md` and this host's paired service token, else it exits 2 with the pair
hint. A re-push the same day replaces that file. A host clock more than a day off
makes the push fail with exit 5. The Worker keeps digests for 35 days.
Pickup also pushes this host's retro digest on its first run of each local date that
`<home>/retro-push.json` does not record; a failed push prints one warning and the
run goes on. On the host that keeps `<home>/retro-schedule-prompt.md`, pickup
catches up a missed weekday retro: from 09:30 local, when no retro run folder
carries today's date and `<home>/retro-catchup.json` does not record today, it
launches that prompt once through the first route's launcher as `retro-<YYYYMMDD>`.
While no route or checkout is available, it prints why it waits once per local date
and reason, recorded in `<home>/retro-catchup-wait.json`. `pickup --dry-run` prints
it every time and records nothing.
`retro pull YYYY-MM-DD` reads `GET /api/retro/<date>` and prints each listed
host's lines in list order. A day
no host pushed prints a note on stderr and exits 0. Raw transcripts never leave
the host.

`retro aggregate` pulls every host's digest for the last N days (default 7, at
most 30), oldest date first, into a new run folder `<home>/retro/<run>/`. The run
tag is the UTC time plus 6 random hex digits. It writes `digest.jsonl`, the rows,
and `aggregate.json`: token sums, context medians, error, denial and command
patterns with counts and up to 5 `host/session` refs, the registered
projects, and `merged`: up to 100 board jobs of registered projects merged in the
window, each with `id`, `project`, `title` and `merged` time, newest first. It also writes `red_flags`: each row of the red-flag index without an `ack` whose job is a board job of a registered project, newest first, as `{time, job, project, host, agent, tool, flags, target, sessions}`. `sessions` lists up to 5 digest `host/session` refs of that job on that host. A Worker that answers 404 gives `[]`. It also writes `lessons`: up to 20 rows from jobs of registered projects in the window, closed jobs and repeated findings together, newest first, as `{job, project, kind, title, reason, at, sessions}`. `kind` is `denied` with the deny notes as `reason`, or `pr-closed` with the close reason. Rows with no usable reason are left out. It also lists each `repeated` board row from the window, at any stage, as kind `repeated-finding` with `reviewer` and `signature`. Every ad-hoc row's `project` becomes `ad-hoc` in both files, so neither
names that work. Lines that are not digest rows count as `skipped`. So do rows
whose `host` is not the host that pushed them or whose `session` is not a
transcript id. It prints the run, the file paths, this host's transcripts root, and
the `out` and `findings` paths for `/factory retro`. Each `hosts` entry carries
`latest`, the newest date in the window with a kept row from that host. The printed
`stale` lists hosts whose `latest` is not the window's last date. A window no host
pushed exits 3 and writes nothing. A linked `retro` or run folder exits 2.

`retro admit <run>` checks the retro agent's `reply.json` in that folder against
`factory.retro/1`. A bad reply, or one whose findings all fail, exits 2 with
nothing written. A bad finding is dropped, printed as a `rejected findings[N]` line
and listed under Rejected findings in `findings.md` without its text. A finding
whose optional `fixed_by` names a `merged` job, and whose sessions in `digest.jsonl`
all started before that merge, leaves the ranking. It prints as `fixed findings[N]:
<id> merged <time>` and is listed under Fixed findings. An unknown or later start
keeps the finding with a Fixed by line. Each card is checked on its own. A bad card
is dropped, printed as a `rejected suggestions[N]` line and listed under Rejected
cards without its text; the clean cards still go through. A clean card whose linked
findings were all fixed prints `skipped suggestions[N]: finding fixed by <id>` and is
not admitted. When every card is bad, admit exits 2 after `findings.md` is written,
with no cards admitted; when every clean card was skipped, it exits 3. The upload's
`rejected` counts only findings that failed checks. `{"none": true}` exits 3. It writes `findings.md`, ranked by count times cost per occurrence, then
writes the reply's cards to `suggestions.json` and adds each one to the backlog as a
held line with a decision form (see [Retro cards](autonomy.md#retro-cards)). It
checks pause, fingerprint dedupe and both caps first, and exits 3 when nothing was
added. A missing run, reply or `aggregate.json`
exits 4. A linked `reply.json`, `aggregate.json`, `findings.md` or
`suggestions.json` exits 2 with `is a link; nothing written`. A reply holding any
`.env` value of 12 or more characters exits 2 with nothing written. Added lines go
to the shared backlog, triage never rates them, and a person decides each one on the
dashboard; the run folder's files stay on this host.

A card may carry a `finding` number. `retro admit` then uploads a `factory.retro-run/1`
record to `PUT /api/retros/<run>` with the checked findings, each card's finding link and
whether it was admitted or why it was skipped, even when nothing was admitted. It prints
`uploaded retro run <run>`. It needs `device:` in `config.md`, and prints
`retro upload skipped: set device: in config.md` without it. An upload error prints
`retro upload failed: <status> <text>` to stderr and never changes the exit code. A Worker
without the route answers 404. The `#/retros` dashboard page lists the runs.

For each admitted red-flag card, `retro admit` sends its backlog line and grouped rows to `POST /api/audit-flags/backlog` and prints `linked <n> red-flag rows to <project>: <title>`. An error prints `red-flag link failed: <status> <text>` to stderr and never changes the exit code.

## Tool-call audit

```text
python3 <installed-skill>/runtime/factory.py audit <job-id>
```

`audit` lists every tool call that this host's Claude Code sessions made in a
job's worktree, one row per call, oldest first. It reads the transcript folders
under `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects`, whose name holds
`factory-<job-id>`, with each main session and its subagents. Each row shows the
time, the first 8 characters of the main session, the agent (`main` or the
subagent type), the tool, its flags and its target. It covers this host only:
sessions on other hosts, and sessions started outside the job's worktree, do not
show. It writes nothing and makes no network call.

Flags name risky calls. They are heuristics that inform a reader and block nothing.

| Flag | Tools | Example hit |
| --- | --- | --- |
| `force-push` | Bash | `git push --force origin factory/1017-add-x` |
| `hard-reset` | Bash | `git reset --hard HEAD~1` |
| `branch-delete` | Bash | `git branch -D old` |
| `recursive-delete` | Bash | `rm -rf build` |
| `no-verify` | Bash | `git commit --no-verify -m x` |
| `sudo` | Bash | `sudo chown u x` |
| `pipe-to-shell` | Bash | `curl ... \| sh` |
| `deploy` | Bash | `wrangler deploy` |
| `pr-merge` | subagent Bash | `gh pr merge 7` |
| `sandbox-off` | Bash | input `dangerouslyDisableSandbox: true` |
| `secret-file` | Bash, Read, Edit, Write, Grep `path` | `Read ~/app/.env` |
| `denied` | any | the call's result is an error that holds a denial phrase |

A `gh pr merge` in the main session carries no flag, since the orchestrator merges approved PRs there.
`secret-file` skips the body of a `cat` or `tee` heredoc with no pipe after it, so writing about `.env` does not flag. A heredoc fed to a shell or interpreter, and every other line, still flags. That includes output run through `eval`, `-c` or a combined flag such as `-lc`, `<(` or `>(`, a `$(cat <<EOF` used as the command itself, and every body line inside a `$(` that spans lines, a delimiter with quotes inside it such as `E"OF"`, and lines an unquoted heredoc expands with `$(` or a backtick. A body written to a file and run later, such as `cat > s.sh <<EOF` then `bash s.sh`, is beyond a per-call heuristic.

Flags print in this order, joined by `,`; a call with none prints `-`. Flags
match the raw target in memory. The printed target has the home path folded to
`~`, then the same redaction as `retro digest`, so header values, bearer tokens
and secret-named values print as `[redacted]`. Control characters, and Unicode
bidi and isolate controls such as U+202E, in the target and the agent name print
as `?`, and the target is cut to 120 characters.

A bad job id exits 2. A missing projects directory exits 4 with
`no Claude Code transcripts at <path>`. No session in the job's worktree exits 4
with `no Claude Code session on this host ran in a factory-<job-id> worktree`.

```text
python3 <installed-skill>/runtime/factory.py audit push <job-id>
```

`audit push` uploads the same rows, each with `host` (this host's `device:`) and
`job` added, to `audit/<job-id>/<device>.jsonl` through `PUT /api/audit/<job-id>/<device>.jsonl`,
and prints `pushed 6 calls to audit/1017-add-x/example-host.jsonl`. A re-push
replaces this host's file. The body is capped at 1 MiB: past that, the oldest rows
are dropped and the line reads `pushed 5 of 6 calls to ...; the oldest 1 did not
fit in 1 MiB`. Rows gain `paseo: {server, agent}` when a Paseo agent file on this
host (`$PASEO_HOME/agents/*/<id>.json`) names the session. The job drawer's `audit` tab shows every host's pushed rows.

`audit push` with no valid `device:` exits 2 with the pairing hint, as does a 403
`audit rows are written only by the host's bound service token`. Any other 403
exits 5. A bad job id, or a second argument without `push`, exits 2 with
`usage: factory.py audit push <job-id>, a full job id such as 1017-add-x`.

`pickup` also pushes this host's audit rows for each job at spec, build, review,
approve or pr-open whose transcripts here changed since `<home>/audit-push.json`
records; a failed push prints one warning and the run goes on. When the push
succeeds and this host's token totals for the job changed, it also writes them to
`tokens.<device>` and prints `recorded <device> tokens on <job-id>`.

## Token usage

```text
python3 <installed-skill>/runtime/factory.py usage [record] <project-slug>/<job-id>
```

`usage` sums the tokens this host's Claude Code transcripts spent on a job, by
model. It reads the same `~/.claude/projects` or `$CLAUDE_CONFIG_DIR/projects`
folders as `audit`. A transcript belongs to the job when:

- its main session ran in a `factory-<job-id>` worktree, which also counts every
  subagent of that session, or
- outside that worktree, a main session or subagent whose own first prompt names
  the job folder `<home>/jobs/<slug>/<id>` or the prompt folder
  `<home>/prompts/<slug>/<id>`. A main session that names the job does not bring
  its subagents along. `1017-add-x` never matches `1017-add-xy`.

Each `message.id` counts once, with the largest value seen for each kind. Models
whose counts are all 0 are dropped, and a model name that is not a plain model
id, such as `<synthetic>`, counts as `other`. It counts tokens, never prices.

```text
job 1017-add-x on host-a: 3 transcripts
model            input  output  cache_write  cache_read
claude-opus-5-5  1,200  48,000      210,000   3,400,000
```

`usage record` prints the same table, then writes it to the board job and prints
`recorded host-a tokens on 1017-add-x`. Each host keeps its own entry under
`tokens`, keyed by its `device:`, and a rerun replaces only that entry:

```json
"tokens": {"host-a": {"at": "2026-01-02T12:00:00Z", "transcripts": 3,
  "models": {"claude-opus-5-5": {"input": 1200, "output": 48000,
  "cache_write": 210000, "cache_read": 3400000}}}}
```

Only model names, counts and the host name reach the board. The job drawer shows
every host's entry as a Tokens table. Close-out runs `usage record` before
`audit push`. Pickup refreshes each host's entry, as `## Tool-call audit` says.

A parse failure warns and never blocks close-out:

| Failure | Result, exit 0 |
| --- | --- |
| bad JSON lines | `factory.py: usage: 2 unreadable lines in agent-b1.jsonl` on stderr, the rest counted |
| unreadable file | `factory.py: usage: cannot read <name>`, file skipped |
| any other scan error | `factory.py: usage: scan failed (<Type>: <msg>); nothing recorded` |
| no matching transcript | `no Claude Code transcripts on host-a name 1017-add-x; nothing recorded` |

A bad target, a second argument other than `record`, or `record` without a valid
`device:` exits 2. `record` exits 4 when the board has no such job and 2 when the
job belongs to another project; board errors keep their usual exits.

## Post-merge outcomes

```text
python3 <installed-skill>/runtime/factory.py outcomes [record] [<project-slug>]
```

`outcomes` sorts every board job at stage `merged` whose `done` falls in the last
30 days into held, reworked or reverted, optionally for one project. For each
project it reads the checkout from `paths.md` and `repo` and `base` (default
`main`) from the project file. It fetches `<base>` from
`https://github.com/<repo>.git` into `origin/<base>` once, then asks
`gh api graphql` for each pull request's merge commit, 50 at a time. It reads the
commits after the merge on `origin/<base>`, skipping merge commits, and applies
these rules in order:

| Order | Result | Rule | Evidence |
| --- | --- | --- | --- |
| 1 | `reverted` | a commit body contains `This reverts commit <merge oid>` | `commit <sha12>` |
| 2 | `reworked` | a board job has the ref `{"kind": "rework-of", "id": <job>}` | `rework-of <id>` |
| 2 | `reworked` | a commit at most 7 days after the merge whose subject contains `fix` or `revert` in any case touches a file the merge changed | `commit <sha12>` |
| 3 | `held` | none of the above | none |

`reworked` lists every matching ref and commit. Batched riders share their
lead's pull request, so they get the lead's result.

```text
outcomes of jobs merged in the last 30 days
project  held  reworked  reverted  unknown
alpha       1         1         1        0
alpha held: 1017-add-x
alpha reworked: 1018-add-y
alpha reverted: 1019-add-z
```

A job the command cannot judge counts as `unknown` and prints its reason, as in
`alpha unknown: 1020-add-w (no merge commit from GitHub)`:

| Reason | Printed |
| --- | --- |
| no checkout | `no checkout of alpha in paths.md on host-a` |
| the checkout folder is missing | `no folder at /src/alpha on host-a` |
| the folder is not a git checkout | `/src/alpha is not a git checkout on host-a` |
| the fetch fails | `could not fetch origin main in /src/alpha on host-a` |
| the PR is not in the project's repo | `PR is not in example/alpha` |
| gh fails | gh's failure, such as `gh is not installed on host-a` |
| GitHub reports no merge commit | `no merge commit from GitHub` |
| the merge commit is not on `origin/<base>` | `merge commit <sha12> is not on origin/main` |

With no matching jobs it prints `no jobs merged in the last 30 days`. A merged
job with no project is left out, and a scan without a project filter prints
`merged jobs with no project, skipped: <ids>` after the result lines.

`outcomes record` prints the same table, then writes each known result to its
job in one board write and prints `recorded 3 outcomes on the board`:

```json
"outcome": {"result": "reworked", "at": "2026-01-09T12:00:00Z",
  "evidence": ["commit 3c4d5e6f7a8b", "rework-of 1021-fix-y"]}
```

It changes a job only when its result or evidence differs from what is stored,
never touches the job's `updated`, and skips the write with
`outcomes unchanged; nothing written` when nothing differs. An `unknown` job is
never recorded. To clear a value, set `{"outcome": null}` with `factory.py board`.
Bad arguments exit 2 with `usage: factory.py outcomes [record] [<project-slug>]`.
The only git write is the `origin/<base>` remote-tracking ref, and a failed fetch
exits 5.

The Retros page's Cost per merged PR section shows recorded outcomes as a revert
count beside tokens per merged PR.

## Reviewer benchmark

```text
python3 <installed-skill>/runtime/factory.py bench score <cases-file> <arm-folder>...
python3 <installed-skill>/runtime/factory.py bench cases <project-slug>
```

`bench score` compares reviewer prompts or models, called arms, on past pull
requests with known defects. It reads a hand-labeled cases file and one folder of
saved review files per arm, and prints severity-weighted recall, tokens and
seconds per arm as JSON. It is offline: it reads only the paths you pass, never
the home, board, R2 or network, and writes no file. It never gates a job.

Cases hold instance data, since they name real pull requests and defects. Keep
them in the factory home, such as `<home>/bench/reviewer-cases.json`, never in
the repository.

```json
{"cases": [{"id": "1018-add-y", "pr": "https://github.com/example/alpha/pull/18",
  "head": "3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d", "source": "reworked",
  "defects": [{"path": "worker/export.js", "lines": [40, 52], "severity": "high", "note": "optional"}]}]}
```

`bench cases` drafts this file. It reads the board for the project's jobs whose
recorded outcome is `reverted` or `reworked`, asks GitHub for each pull request's
head commit, and prints one case per job with an empty `defects` list. Redirect it
to `<home>/bench/reviewer-cases.json`, then add each defect by hand. A redirect
replaces the file even when the command fails, so once you have labeled cases,
draft into a new file and copy over the cases you want. `bench score`
refuses a case with no defects, so delete any case you cannot label.

`bench cases` errors:

| Input | Exit | Message |
| --- | --- | --- |
| not exactly one project slug | 2 | `usage: factory.py bench cases <project-slug>` |
| no `projects/<slug>.md` | 4 | `bench: no project file for <slug>` |
| GitHub lookup fails | 5 | `bench: <why>` |
| nothing drafted | 3 | `bench: no reverted or reworked jobs to draft in <slug>` |

`source` is not scored. `bench replay` uses `pr` and `head`. Severity weights are
high 3, medium 2 and low 1.

An arm folder's name is the arm name. It holds one `<case-id>/` folder per case,
with any `review-*.md` files and an optional `run.json` in the shape
`usage_totals` prints:

```json
{"seconds": 300, "models": {"<model>": {"input": 200000, "output": 20000, "cache_write": 30000, "cache_read": 0}}}
```

### Replay

```text
python3 <installed-skill>/runtime/factory.py bench replay <cases-file> <arm-folder> --role <reviewer> [--model <model>]
```

`bench replay` fills an arm folder for `bench score`. `--role` is
`review-security`, `review-ux`, `review-ui` or `review-code` (or its
`factory-review-*` name). It needs `claude` and `gh` on PATH, each case's job
folder pulled into the home with its `spec.md`, and a checkout of the job's
project in `paths.md`. The project file's `repo` must match each case's `pr`.
Every case is checked, and GitHub is asked once per repo for the merge commits,
before any reviewer runs.

For each case, one at a time, it:

1. Fetches the head and merge commits from `origin` when the checkout lacks
   them, and takes the base as `git merge-base <merge>^1 <head>`.
2. Adds a detached scratch worktree at the head under a temp folder, and copies
   the job's `spec.md` and `build.md` into a scratch job folder with an empty
   `round-1/`.
3. Runs the installed `factory-<role>` agent through `claude -p` in the scratch
   worktree, with `--permission-mode dontAsk` and only Read, Grep, Glob, Write
   and `git` in Bash allowed, `--add-dir` on the scratch job folder, and
   `--model` when given. Its output goes to `<arm>/<case-id>/replay.log`, and it
   stops after an hour.
4. Copies the review to `<arm>/<case-id>/review-<role>.md` when its first line
   starts `VERDICT: `, and writes `run.json` with role, model, head, base, exit,
   wall seconds and the session's token totals. `run.json` and `replay.log` stay
   even when the case fails.
5. Removes the scratch worktree without `--force`. When that fails it names the
   folder on stderr for you to remove.

It prints one line per case and a total, and exits 1 when any case failed:

```text
1018-add-y: wrote review-code.md (VERDICT: CHANGES), 300 s, 250000 tokens
1019-add-z: skipped, review-code.md already exists
replayed 1, skipped 1, failed 0 of 2 cases into opus-medium
```

A failed case prints `<case-id>: failed: <reason>`. A rerun skips cases that
already have the review, so it resumes after a failure.

### By hand

Without `claude`, fill a case folder by hand:

1. In the project checkout, run `git worktree add --detach <scratch> <head>`.
2. Run the arm's reviewer there with its prompt and model.
3. Save its review as `<arm>/<case-id>/review-code.md`, and its wall seconds and
   `factory.py usage` totals as `<arm>/<case-id>/run.json`.
4. Run `git worktree remove <scratch>`.

Matching rule, for the defect `worker/export.js:40-52`:

| Review file | Cite | Result |
| --- | --- | --- |
| `VERDICT: CHANGES` | `worker/export.js:44` | caught |
| `VERDICT: CHANGES` | `export.js:50` (bare name) | caught |
| `VERDICT: CHANGES` | `worker/export.js:60` | missed |
| `VERDICT: PASS` | `worker/export.js:44` | missed |

Only review files whose first line is `VERDICT: CHANGES` count. A cite hits when
its path equals the defect path, or has no `/` and equals the defect's file name,
and a cited line or span overlaps the defect's lines. A missing case folder misses
every defect and lists the case under `missing`, so arms keep the same
denominator. A missing or invalid `run.json` lists the case under `no_run` and
adds no tokens or seconds. `tokens` counts input, output and cache_write;
`seconds` sums and rounds to a whole number; `weighted_recall` rounds to 2 places.

For two cases with three defects of total weight 6, and arms `opus-medium` and
`sonnet-low`, it prints (arms in argument order, second arm cut):

```json
{"cases": 2, "defects": 3, "weights": {"high": 3, "medium": 2, "low": 1}, "arms": [
 {"arm": "opus-medium", "weighted_recall": 0.83, "caught": 2,
  "severity": {"high": {"caught": 1, "of": 1}, "medium": {"caught": 1, "of": 1}, "low": {"caught": 0, "of": 1}},
  "missed": ["1018-add-y docs/device.md:12-12"], "tokens": 413000, "seconds": 540, "runs": 2,
  "missing": [], "no_run": []}, ...]}
```

The command checks the cases file and every arm folder before reading any review,
so a bad input prints nothing on stdout. Errors print after `factory.py: `:

| Input | Exit | Message |
| --- | --- | --- |
| wrong arguments | 2 | `usage: factory.py bench score <cases-file> <arm-folder>...` |
| no cases file | 4 | `bench: no cases file at <path>` |
| not `{"cases": [non-empty list]}` | 2 | `bench: <path> is not a cases file: needs {"cases": [...]}` |
| bad case field | 2 | `bench: <path>: case <n>: <reason>` |
| no arm folder | 4 | `bench: no arm folder at <path>` |
| arm name not `[\w.-]+` or repeated | 2 | `bench: arm name <name> must be unique letters, digits, ., _ or -` |

Case reasons, with `<n>` and `<m>` counting from 1: `id must look like 1018-add-y`,
`duplicate id <id>`, `pr must be a GitHub pull request URL`,
`head must be a 40-character commit`, `source must be reverted or reworked`,
`defects must be a non-empty list`,
`defect <m>: path must be a relative repository path` (no leading `/`, no `\`,
no `..` segment), `defect <m>: lines must be [start, end] with 1 <= start <= end`
and `defect <m>: severity must be high, medium or low`.

## Exit recovery

| Exit | Action |
| --- | --- |
| 1 | Inspect Git failure or remaining stop PIDs. Stop only this job's owned processes; do not kill unrelated services. |
| 2 | Fix usage, absolute location, config, connection descriptor or role delimiters. Preserve state; no fallback to a new home. |
| 3 | Nothing runnable in backlog/ideas/stuck. Stop without retrying or claiming another device's work. |
| 4 | Object/project/branch missing. Confirm the ID/checkout and use init in the intended repo when registration is needed. |
| 5 | Check connectivity, HTTPS endpoint, Access policy and token renewal; origin failures also stop. Do not expose credentials while diagnosing. |
| 6 | Stop on another owner, drift, conflict or unsafe link/junction/unlistable path. Never retry/take over automatically. |

For `.remote`, compare retained local and remote copies, explicitly merge/select,
then delete only the reconciled conflict copy and push. Never upload `.remote`,
invent a missing baseline, silently change permissions, follow unsafe links or
force a claim/branch. Keep existing original state and backups while diagnosing.

## Host registration

Before pairing, the operator issues one Access service token per host and adds it
to the Service Auth policy. Never share one token between hosts. The Worker binds
each token to one host through `POST /api/hosts/<id>/pair` and accepts that
host's status only from its bound token. A person creates, confirms and retires
hosts through `/api/hosts`. Once any host is registered, pickup reaches only
active hosts, so register and confirm every host in one sitting. Pair each host
from its own terminal:

1. In Settings, Hosts, add the host under this device's current name: the
   `device:` value in `config.md`, else the hostname. Board claims use that
   name, so a different id is refused.
2. Within 15 minutes, run this on the host with the code Settings showed:

   ```text
   python3 <installed-skill>/runtime/factory.py host pair <id> <code>
   ```

   It sets `device: <id>` in `config.md` and sends the host's first status.
3. Confirm the host in Settings, Hosts.

A wrong attempt uses the code up, so shell history holds only a spent code. Ask
for a new code after any refusal. Retiring a host does not turn the filter off. To
return to pass-through, delete every `hosts/` object from the bucket. See
[architecture](architecture.md) for the routes.

Add, confirm and retire hosts in Settings, Hosts. The pickup controls list active
hosts as checkboxes.

## Explicit native agents and platform verification

```text
python3 <installed-skill>/runtime/factory.py init --agents both
```

Other selections are `none`, `claude` and `opencode`. See
[integrations](integrations.md) for native activation, opaque headers and optional
schedules. Installation, initialization and adoption never weaken security policies.

The `CI` workflow runs parallel Ubuntu 22.04/Python 3.12 jobs on pull requests, and its
`python` job runs the device suite. The runtime
supports Python 3.8+. The following additional platform checks can run locally.
`FactoryTest.test_released_hold_fails_on_real_windows_junction` runs on Windows
and is skipped elsewhere. POSIX-only permission cases
`FactoryTest.test_released_hold_fails_on_unlistable_round_directory`,
`FactoryTest.test_released_hold_fails_on_unreadable_job_folder_hiding_a_link` and
`LocationTest.test_unreadable_metadata_and_unlistable_tree` run as non-root on
Linux/macOS and skip Windows. `LocationTest.test_pointer_and_metadata_links_refused_but_home_alias_allowed`
and `LocationTest.test_unreadable_pointer_and_trusted_default_alias` use POSIX
symlinks/permissions; Windows owns the real junction case. Legacy symlink cases
run on POSIX and also on Windows when native symlink creation is available.
`PaseoStallsTest.test_list_reports_only_idle_claude_limit_stalls` and `PaseoStallsTest.test_dateless_reset_anchors_on_message_time` skip where `zoneinfo` cannot resolve `UTC` (Python 3.8, or no zone data); `PaseoStallsTest.test_missing_zoneinfo_leaves_stalls_out` covers that path on every host.
Do not skip a case when running on its owning platform.
