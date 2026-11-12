# Autonomy

Explorer agents may one day read a project and suggest work. This page covers the
controls that keep them to *proposing* idea cards. Only a human at a terminal turns
a suggestion into work. Stage 1 adds one read-only explorer for one allowlisted
project, run by a schedule a human enables. The kill switch defaults to paused.

## Commands

All of these run through `factory.py` on a device.

| Command | Who | What it does |
| --- | --- | --- |
| `autonomy status` | anyone | Prints the board's `autonomy` record with defaults filled. Writes nothing. |
| `autonomy pause` | anyone | Sets `paused: true`. The next `suggest admit` exits 3. |
| `autonomy resume` | human at a terminal | Sets `paused: false` after you type `resume`. |
| `autonomy allow <slug>` | human at a terminal | Adds a registered project to the allowlist after you type its slug. |
| `autonomy deny <slug>` | anyone | Removes a project from the allowlist. |
| `autonomy self <slug>` | anyone | Marks a project as part of the factory itself, so the classifier flags its cards. |
| `suggest admit <file>` | trusted launcher | Checks a `factory.suggestions/1` file and adds one gated idea card per new suggestion. |
| `suggest approve <job-id> <A\|B\|C>` | human at a terminal, or an orchestrator relaying the person's chat message (`--chat-file`) | Approves one option after you type the job id, or records the chat quote with `--chat-file`, which lets `factory.py pickup` promote the card. |
| `explore <slug>` | explorer schedule | Snapshots an allowlisted project's base for the explorer and prints the run's paths. Exits 3 when paused or not allowlisted. |
| `explore <slug> done <run>` | explorer schedule | Removes that run's snapshot and keeps its suggestions file. |

Loosening (`resume`, `allow`, `approve`) needs an interactive terminal and a typed
confirmation, or for `approve` a relayed chat message (`suggest approve --chat-file`), as
[chat decisions](../skills/factory/references/chat-decisions.md) says. Tightening never does.
A missing `autonomy` record means paused, with an empty allowlist.

## What admission checks

- The file is at most 64 KiB of UTF-8 JSON. It holds 1 to 5 suggestions with a fixed
  schema, no unknown keys, length caps, no links, no code fences and no control
  characters. Any violation exits 2, and nothing is written.
- The project is registered and on the allowlist, and autonomy is not paused (else
  exit 3).
- A suggestion whose fingerprint matches any card ever admitted, closed ones included,
  is skipped. So is one over the cap of 10 cards a day across the factory, or 5 open
  cards per project. Fingerprints survive archiving: when the Worker archives a card
  off the board, its fingerprint stays in the board's `archived.fingerprints`.
- A deterministic classifier records `eligible` and `reasons` (risky kinds, size L,
  no or many paths, sensitive paths such as CI, auth, deploy and lockfiles). In stage 0
  every card waits on a human whatever it says.

Each card lands at `stage: idea` with no `project`, a `needs` ask and an `origin`
record holding its provenance. The needs-you column shows it. The `idea.md` tab
shows the question, the options with pros and cons, the recommendation and how to
answer. To reject a card, discard it in the idea drawer.

## Stage 1 controls

These harden admission so it can take real explorer output.

- **Fail closed.** A malformed `autonomy` record counts as paused with an empty
  allowlist, and `factory.py` warns on stderr. Malformed means not an object, a
  `paused` that is not true or false, a `projects` or `self` that is not a list of
  strings, or more than one allowlisted project. `autonomy pause` rewrites it clean.
- **One project.** The allowlist holds at most one project. `autonomy allow <slug>`
  exits 2 before any prompt while another project is on it, and names that project.
  Run `autonomy deny <current>` first.
- **Untrusted text.** Every explorer string passes admission rules 3 to 7 from
  [trust.md](trust.md) through `check_untrusted`, so invisible characters, the
  envelope tag, links, secret shapes and override phrases exit 2 with nothing written.
- **Device secrets.** A file holding any `.env` value of 12 or more characters exits
  2 with nothing admitted. The error names the key, never the value.
- **Quoted prose.** The card renders the summary, question and recommendation reason
  as blockquotes. A heading or list inside them stays quoted, so explorer text cannot
  pose as the card's own instructions. A summary is one paragraph: a blank line exits
  2, since blank quote lines collapse.
- **ASCII word rules.** The secret and phrase patterns match ASCII word boundaries
  only, as the Worker's do, so `öignore the previous instructions` still trips them.

## Stage 1 explorer

`schedules/explore.md` runs one explorer over the allowlisted project. A human
materializes and enables it; nothing enables it by default.

```mermaid
flowchart LR
  S[schedule session] -->|explore slug| E[factory.py]
  E -->|snapshot, JSON| S
  S -->|checkout path| X[factory-explorer: Read Grep Glob]
  X -->|one JSON reply| S
  S -->|write verbatim to out, suggest admit out| E
  S -->|explore slug done run| E
```

- **Snapshot.** `explore <slug>` checks the kill switch and allowlist first, then
  fetches `origin/<base>` and unpacks that commit with `git archive` into
  `<home>/explore/<slug>/<run>/`. Only regular files and directories with safe
  relative paths land there. There is no `.git`, so no origin URL, and no symlink or
  hardlink. It prints `project`, `run`, `commit`, `checkout` and `out`.
- **Done.** `explore <slug> done <run>` deletes the snapshot. The `<run>.json` file
  beside it stays as the run's record. A crashed run leaves its snapshot until a
  `done` removes it.
- **Claude only.** The `factory-explorer` role has Read, Grep and Glob only: no
  shell, write or web tool. OpenCode's installed copy carries no tool limit, so the
  schedule stops with `no claude route` unless the picked route's provider is
  `claude`.
- **Human gate.** The schedule never runs `suggest approve` or an `autonomy`
  command. Every admitted card still waits on a human.

The explorer replies with one `factory.suggestions/1` object, or `{"none": true}`.
Admission enforces these field limits:

| Field | Limit |
| --- | --- |
| `schema`, `project`, `run`, `commit` | `factory.suggestions/1`, then the run's values as `explore` printed them |
| `suggestions` | 1 to 5 objects |
| `kind` | `bug-fix`, `test-gap`, `docs`, `cleanup`, `dependency-upgrade`, `migration`, `security`, `product-idea` or `tooling` |
| `title` | 1 to 80 characters, one line |
| `summary` | 1 to 600 characters, one paragraph |
| `evidence` | 1 to 5 of `path`, `note` (1 to 200 characters, one line) and an optional `line` of at least 1 |
| `paths` | 0 to 20 relative repo paths of `[A-Za-z0-9_./-]`, at most 200 characters, no `..` |
| `size` | `S`, `M` or `L` |
| `question` | 1 to 300 characters, one line |
| `options` | 2 or 3 keyed `A`, `B`, `C`; `label` 1 to 80 and `detail` 1 to 300 characters; 0 to 4 `pros` and `cons` of 1 to 120 characters |
| `recommend` | one of the option keys |
| `reason` | 1 to 300 characters |
| `diagram` | optional, 1 to 1500 characters |

## Trust boundary

```mermaid
flowchart LR
  subgraph Untrusted data
    R[repo contents, project files, prompts] --> X[explorer agent]
    X --> F[suggestions JSON file]
  end
  F -->|schema, caps, allowlist, dedupe, classifier| A[factory.py suggest admit]
  A -->|CAS PUT, idea + needs, no project| B[(board.json)]
  H[human at terminal] -->|typed confirm| P[suggest approve]
  O[orchestrator] -.->|chat quote| P
  P -->|sets project, clears needs| B
  B --> L[existing loop: spec, build, 4 reviews, approver, PR]
```

- `factory.py`, the Worker and the human are trusted. Everything an explorer reads
  or writes is untrusted data.
- Admission sends only GET and PUT to `/api/board` and GET to `/projects.json`. It
  never runs git, gh or any other process.
- Guards: `ideas` skips any job with a `needs` ask or an unapproved explorer card.
  `board` refuses a patch that sets `origin`, and refuses moving an unapproved
  explorer card to any stage but `idea` or `closed`.
- Honest limit: the Worker cannot yet tell a human from a service token, so a token
  holder could forge an approval with a raw PUT. The runtime guards stop automation,
  except an agent that runs `--chat-file` or `--chat`, which only the chat-decisions
  rule holds back. They do not stop abuse. A Worker-side human-only approval gate must land before stage 2.
- Upgrade every host that runs `factory.py pickup` before the first `autonomy resume`.
  Older runtimes skip cards without a `project`, so unapproved cards stay put there too.

## Retro cards

`retro admit` adds each retro card to the backlog as one held line, not as an idea
card. The line is `- [ ] <project>: <title>`, and its indented context renders on the
dashboard Backlog decision page as a one-pick form. A person answers it there with
Approve or Deny.

```
- [ ] other: Guard the ls alias
  Recommend: approve

  The ls alias crashes without a terminal.

  Why Interactive only: It keeps colors for people.

  Evidence:
  - 253x winsize_crash in tool results (1 session)

  Retro: run 20261111T090000Z-a1b2c3, fingerprint bac6f4150d124158, created 2026-11-12T05:40:00Z

  ## Question: Guard the ls alias
  How should the alias be guarded?
  Pick: one
  - Interactive only (recommended): Alias ls only in interactive shells. Pros: Keeps colors.
  - Drop the alias: Remove the ls alias. Pros: Simplest fix. Cons: Loses colors.
```

- **Per-card project.** Each card carries its own registered `project`. An
  unregistered one exits 2 with nothing added. The line, fingerprint and open cap use
  that project. The one-project allowlist does not apply.
- **Gates.** A paused kill switch exits 3 with nothing added. A card is skipped as
  `duplicate <id>` when a board origin or the archive holds its fingerprint, and as
  `duplicate in backlog` when a `Retro:` line under any backlog line, checked or not,
  does. The daily cap counts board origins and `Retro:` lines created in the last 24
  hours plus this run's adds. The open cap counts the project's idea origins, its
  unchecked `Retro:` lines and this run's adds. Other skips are
  `title ends in a backlog ref`, `context: <why>` and `already in backlog`. It prints
  `{"admitted": [...], "skipped": [...]}` and exits 3 when nothing was added.
- **Never triaged.** `backlog add` and the pickup triage sweep skip any line whose
  context has a `Retro:` line, so the Worker never queues one. An older runtime's
  sweep still rates them, so upgrade every pickup host before the first `retro admit`.
- **Checks.** The field limits, the untrusted-text checks and the `.env` value check
  apply as they do to explorer files. The summary and reason are collapsed to one line.
- **Evidence.** Each evidence item is `{"note", "count", "sessions"}` with 1 to 5
  `host/session` refs.
- **Kind.** `tooling` joins the suggestion kinds.

Retro idea cards admitted before this change keep `origin.by: "retro"` and still
take `suggest approve`, which skips the allowlist for them but needs a registered
project and a human typing at a terminal, or a relayed chat message.

## Retro pipeline edits

A retro reply may carry up to 2 `edits`, each `{"scope", "stage", "note", "summary"}`.
`retro aggregate` records each scope's `version` and `stage_notes` under `pipeline`.
`retro admit` checks each edit, then sends it to `PUT /api/pipeline/retro/<scope>` as
`{"schema": "factory.retro-edit/1", "run", "base_version", "stage", "note", "summary"}`.
The Worker replaces only `stage_notes[stage]` and saves the next version with
`saved_by: "retro"`, `summary`, `run` and `host`.

| Gate | Refusal |
| --- | --- |
| a paired or active host's bound service token | 403 `retro edits come only from a bound host` |
| a `projects/<slug>` scope whose slug is on the autonomy allowlist; `global` never | 403 `retro edits only allowlisted project scopes; global is person-only` |
| autonomy `paused` is `false`; a missing or malformed record counts as paused | 409 `autonomy is paused; no retro edit` |
| `base_version` is the stored version | 412 `precondition failed` |
| at most 600 characters changed, counted after trimming the shared prefix and suffix | 413 `retro edit changes more than 600 characters` |
| at most 2 retro versions per scope in 24 hours | 429 `retro edit cap: 2 per scope per day` |
| note and summary pass the stage-note checks; the summary is one line of 1 to 120 characters | 400 `<field>: <reason>` |

- **Device first.** Before any PUT, `retro admit` repeats the text checks, the `.env`
  value check, the scope rule, the 600-character cap, the daily cap and the pause. While paused it prints
  `autonomy is paused; no pipeline edit` and sends nothing.
- **Output.** Each edit prints `edited <scope> stage_notes.<stage> to version N: <summary>`
  or `skipped edits[i]...`, and `findings.md` lists it under Pipeline edits. Edits never
  change the exit code. An older Worker answers 404.
- **Kill switch.** `factory.py autonomy pause` stops every retro edit, on the device and
  in the Worker.
- **Restore.** Settings, Pipeline lists each retro version as
  `Version 8 · retro · <time> · <summary> · run <run>`. Click Restore to draft on the
  version before it, type a Change summary such as `Undo retro run a1b2c3`, and Save.
  That writes a new version; history keeps every one.

## Stages

```mermaid
flowchart LR
  S0[Stage 0: admission + guards, paused by default] -->|merged, all pickup hosts upgraded| S1[Stage 1: one read-only explorer, one project, every card human-gated]
  S1 -->|20+ cards decided, Worker human-only approval live| S2[Stage 2: auto-admit low-risk kinds, one project]
  S2 -->|recorded human sign-off| S3[Stage 3: cross-project]
```

| Stage | Entry | Exit |
| --- | --- | --- |
| 0 | coordinator gate on the spec | merged after 4 reviews and the approver; `autonomy status` on production shows `paused: true`; every host running `factory.py pickup` upgraded |
| 1 | stage 0 exit; a human runs `autonomy allow <one slug>` and `resume` | at least 20 explorer cards decided by a human; zero cards promoted without approve; approve and discard counts by `origin.eligible` recorded |
| 2 | stage 1 exit; Worker human-only approval gate merged; eligible-card approval rate at least 80 percent over at least 20 eligible cards | two weeks with zero reverted auto-card PRs |
| 3 | stage 2 exit; a signed-off decision recorded by a terminal-gated command in the board and in the factory project's `decisions.md` | ongoing |

Each later stage is its own gated job.

## Backlog triage

Backlog triage lets a project skip the dashboard Approve click for backlog lines its
rules clearly allow. It is off unless a person turns it on.

```mermaid
flowchart LR
  A[factory.py backlog add] --> B[(backlog.md)]
  A --> W[POST /api/backlog/triage]
  W --> M[clef-flash on Workers AI]
  W -->|queue| I[(board.json idea)]
  W -->|hold or off| B
  I --> P[factory.py pickup]
  P[factory.py pickup] --> S[backlog_triage_pass]
  S --> W
  S --> X[POST /api/backlog/triage/batch]
  S --> V[POST /api/backlog/triage/verdicts]
```

Settings, Pipeline holds a `triage` block per scope. A project value replaces the global
one whole. With neither set, triage is off.

| Field | Rule |
| --- | --- |
| `enabled` | true or false |
| `threshold` | an integer from 1 to 100. The dashboard fills in 90 |
| `rules` | text, may be empty. It passes the same admission rules as stage notes, so no links, secrets or instructions to skip review |

After `factory.py backlog add` appends a line, it posts the line and its `--context`
text to the Worker. The Worker reads the effective `triage` block and answers:

| Case | Model called | Decision |
| --- | --- | --- |
| triage off or not set | no | `off` |
| context line 1 is `Recommend: deny` (LF or CRLF) | no | `hold` |
| context has a `## Question:` line | yes | `hold`, with `choices` |
| confidence at or above the threshold | yes | `queue` |
| confidence below the threshold | yes | `hold` |

- **Queue.** `factory.py` adds an `idea` row with `backlog.by: "triage"` and the verdict,
  then checks the line off with the new id, as a dashboard Approve does. If the line
  changed meanwhile, it drops the row while the row is still an unclaimed idea. The idea
  carries the line's context under `Context from the backlog:`, cut so the idea stays within
  4000 characters, as a dashboard Approve does.
- **Hold.** The line waits in the backlog for a person, as before.
  The Worker keeps the latest hold verdict for each line, keyed by `<slug>: <feature>` without trailing whitespace, and the backlog drawer shows it under Held by triage. The board reads the stored verdicts of the lines it lists in one `POST /api/backlog/triage/verdicts` call per 50 lines. It reads again only when the listed lines change, and only for lines with no verdict yet. Opening a line with no verdict reads that line again. A line the Worker would refuse, such as one over 2000 characters, reads as no verdict and leaves the other lines' verdicts intact.
- **Sweep.** Lines that `backlog rework` appends, or that a person types, get the same
  verdict from `backlog_triage_pass`. `factory.py pickup` runs it before it claims ideas, never with `--dry-run`, and at most once every 30 minutes per host. The host's `backlog-triage-sweep.json` holds the start of the last sweep, and `factory.py pickup --triage-now` runs a sweep at once. Each sweep takes up to 20 unchecked lines in projects whose effective `triage.enabled` is true and reads their stored verdicts in one `POST /api/backlog/triage/verdicts`. A line another host already held is recorded without a post. The rest go with the indented block under each as context. Plain lines go up to 8 per `POST /api/backlog/triage/batch` call, one project per call, in file order. Lines whose context has a `## Question:` line then post alone to `POST /api/backlog/triage`. An older Worker answers the batch route with 404, and a batch it refuses whole gets a 400, so those lines post one at a time. In a batch reply, an entry the model answered unusably (502) warns and waits for the next sweep without ending it, and an entry refused with a 400 is recorded like a single 400. Held lines, and lines the Worker refuses with a 400, go into the host's `backlog-triage.json`, so this host never sends them again. `backlog add` records its held line there too. A 404, 409, 502 or 503 reply, or a network failure, warns and ends the sweep. That line and the rest are retried on the next sweep. Deleting the file
  only causes rescoring. A line whose new idea row could not be checked off or removed goes
  there too, so it is never queued twice.
- **Failure.** A 404 from an older Worker means off. Any other error prints a warning,
  and the line waits. If a board or backlog write fails while
  queueing, it prints what is left (the line, or the new id to check off or remove by hand).
  `backlog add` still exits 0. A Worker error inside the sweep, such as a 500 on GET /api/backlog,
  prints the error and "backlog triage stopped; unsent lines wait for the next sweep, or run pickup --triage-now". The sweep
  keeps the holds it recorded, and pickup still claims and launches ideas. While the sweep keeps
  failing, pickup's host status carries `triage_failed_since`, the start of the first failing run.
  Settings, Hosts shows "Backlog triage failing since <time>" in red with a warning sign on the
  host's card until a sweep succeeds.
- **Person only.** Pipeline documents are saved only by a signed-in person (F11), so a
  service token cannot switch triage on or move the threshold. The one exception, retro
  admit, may edit only stage notes. The Worker compares the
  score with the threshold itself.
- **Prompt.** The Worker sends `@cf/cloudflare/clef-flash` a `state` text and one `build`
  question of type `noul`. In `state` the rules travel in an `operator-preferences` envelope and
  the item in a `backlog-item` envelope, both marked as data. The question asks whether the item
  should be built now without a person reviewing it, and its yes probability times 100, rounded,
  is the confidence. A batch call puts each item in its own `backlog-item id="<n>"` envelope,
  where `<n>` is the item's position in the request, and asks one `build.<n>` question per item
  that judges only item `<n>`, under the same rules envelope. The verdict's `reason` is fixed
  text: `Rated by clef-flash against the project rules.`, or `Rated by clef-flash.` when the rules
  are empty, so an item can move the score but write no text. An item or context holding either
  tag, including fullwidth, Cyrillic, Greek or other lookalike forms, or a letter that reads as
  either l or I, is refused with a 400.
  For an item whose context has `## Question:` sections, the same call adds one `choice`
  question `q<i>` per backlog question, over its options, and one `choice` question
  `q<i>.o<k>` per option, over fixed reasons such as `It gives the item the most value.`
  (`It follows the project rules.` only when rules are set). The hold verdict then carries
  `choices`: for each question its header, the model's pick and its percent, and each option's
  percent and reason. The drawer shows them under Held by triage. If the questions do not parse,
  the binding is missing, or the call fails or gives an unusable reply, the item gets the fixed
  hold with no `choices`.
- **Floors.** A queued idea still goes through the spec, the reviews F2 requires, the
  approver, merge rules and any `spec-approval` gate.
- **Deploy.** The Worker needs the `ai` binding, which `tools/deploy.py render` adds.
  Workers AI usage bills against the account. Without the binding the route answers 503,
  and lines wait. In a 36-call live run, a call sent a median of 292 input tokens, which costs about
  $0.000011, or about 1 neuron. The free tier of 10,000 neurons a day covers about 9,900
  calls. A question item with 3 questions of 4 options is an estimated 2000 to 2500 input
  tokens, about 8 neurons. That is a guess until a live run measures it.

To stop triage, set it to Off in Settings, Pipeline. Ideas it already queued stay
ordinary ideas, and a person can discard them.

## Review escalation

After a review round with CHANGES below the round cap, one clef-flash call over the
findings decides whether the job needs a person now. There is no setting: the
threshold is a code constant of 80, a guess that has not been measured.

```mermaid
sequenceDiagram
  participant O as orchestrator
  participant F as factory.py escalation
  participant W as Worker
  participant M as clef-flash
  O->>F: sample/042-csv-export 1
  F->>W: POST /api/review/escalation
  W->>M: env.AI.run
  M-->>W: noul 0.91
  W-->>F: escalate, 91
  F-->>O: one JSON line
```

- `factory.py escalation <slug>/<job-id> <round>` reads `round-<N>/review-*.md` and sends
  only the reviews whose first line is `VERDICT: CHANGES`, each cut to 8000 characters.
- The Worker wraps each review in a `<review-findings reviewer="..." trust="data">`
  envelope and asks one `escalate` question of type `noul`. Confidence 80 or more is
  `escalate`, below is `continue`. The reason is fixed text, so review text can move the
  score but write no words.
- On `escalate`, loop step 5 in references/commands/feature.md pushes the job, patches it to `needs-human` with
  a `needs` link to the first CHANGES review, and dispatches nothing.
- The check runs only below the cap. It never runs after approver CHANGES, at the round
  cap, or on an all-PASS round, and it sees only the current round.

Request: `{"reviews": {"security"?: text, "ux"?: text, "ui"?: text, "code"?: text}}`, at
least one key, body cap `160 << 10` bytes. A review over 8000 characters, an unknown
reviewer or key, or a control or invisible character gets a 400. Variation selectors and
zero-width joiners between emoji are the two exceptions. Before the call, the Worker
removes those, replaces secret-shaped text with `[secret-shaped text removed]`, replaces a
`review-findings` tag with `[review-findings tag removed]` and an `operator-preferences`
tag with `[operator-preferences tag removed]`, and cuts the result to 8000 characters.
When the text holds a tag or a secret shape in fullwidth or look-alike letters, the Worker
folds those letters to ASCII first, and the rest of that review is folded with it, so a
token such as `ｇｈｐ＿…` or `gһp_…` is removed too. A review that quotes a secret or a
tag, or writes an emoji such as ⚠️ or 👩‍💻, is still rated.

Reply 200: `{"decision": "escalate"|"continue", "confidence": int, "threshold": 80,
"reason": text, "model": "@cf/cloudflare/clef-flash"}`. The reason is
`clef-flash rated the findings as needing a person.` or
`clef-flash rated the findings as fixable by the builder.`

| Case | Output | Loop |
| --- | --- | --- |
| no CHANGES review | `continue`, no call | unchanged |
| 400, 404, 502, 503, network, bad reply | warning, `continue` | unchanged |

A failed check never blocks a job. It only skips the extra stop. The round cap, the
approver and the merge rules still apply. The Worker must deploy before hosts upgrade, or
`factory.py` sees 404 and continues.

## Rollback

| Scope | Rollback |
| --- | --- |
| One card | Discard it in the idea drawer. It moves to `closed`, and its fingerprint stays, so it never returns. |
| Admission | `autonomy pause` from any device. The next admit exits 3. |
| Stage 0 code | Pause, discard open explorer cards, revert the PR. No data migration: older code ignores the `autonomy` and `origin` keys. |
| Stage 1 and later | Pause, then `autonomy deny <slug>`, and revert that stage's PR. |
