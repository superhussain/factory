---
repo: owner/name
base: main
title: name
# buildkite_org: <org slug>
---
# Factory project file

Project context for the factory agents. The orchestrator passes this file to
every agent, and each one reads the sections named for it. Keep it to what is true
of this repo. The installed factory skill owns the shared loop and agents.

This file lives in R2 behind the factory Worker, outside the repo. `factory.py`
caches it in `<factory-home>/projects/`. The frontmatter names the
GitHub repo, the base branch for PRs, and a display title. `repo`, `base` and
`title` are required. `archive` and `buildkite_org` are optional. Uncomment
`buildkite_org` so `pr-state` can read Buildkite logs. Each device keeps its own
checkout path separately in `paths.md`; paths and credentials do not belong here.

Edit this existing shared document through the dashboard's Project settings or
the device project pull/push commands. Keep every agent section heading. Saves
compare the version edited; copy a conflicting draft before loading the latest
version and reapplying changes. Changes affect future agent reads, not running jobs.

## Stack

Repo layout and tooling the spec writer and builder need. One bullet per app or
top-level folder, with what it is.

## Setup

Steps the builder runs in a fresh worktree before its first change: runtime
version and install command. Use isolated test credentials, never production secrets.

## Verify

Commands that must pass before each handoff, and the commands that fix formatting.
Say whether a test suite exists.

Add Babysit push: <command> when babysit fix pushes need more than git push --no-verify, such as a secret scan with slow hook steps skipped. It runs from the job worktree. Add Babysit verify: <check> for one targeted check babysit adds to source fixes.

## Spec

How the spec names affected areas, and project risks it must call out (deploys
triggered by a merge, native config, and so on).

## UI

Paths that count as UI, the styling system and components to stay consistent with,
and how to capture and upload screenshots (skill, script, port, image host).

## Security

What the security reviewer must check beyond the general list: auth flows, where
secrets live, sensitive data and where it may go.

## UX

Surfaces to judge and states that matter here (offline, permissions, sync), plus
the copy tone.

## Code

Project conventions the code reviewer enforces, such as where shared types live.

## Escalate

Extra triggers that make the approver ESCALATE and hold the PR for a human. Keep
them to what is risky or hard to undo. Everything else merges on its own.

## Device builds

Which diffs need a device build (say, any change under the mobile app), and the
commands that trigger one batched build from the base branch. Leave this out if
the project has no device builds.

## Orchestrator

Load limits, ports the factory must not use, and secrets rules for build output.

## PRs

How PR descriptions are written (screenshot skill, testing section).
