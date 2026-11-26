# Migrating a host from the Python factory to Rust

This guide moves one host from the Python factory CLI to the Rust binary. It
works for a person at a terminal and for an agent running unattended. Your board,
backlog, projects and job files stay where they are. The host ends up running
`<installed-skill>/runtime/factory` (`factory.exe` on Windows), with the daemon
as its scheduler.

Do one host at a time. Each step below says what to run and what you should see.
An agent can follow the [agent checklist](#agent-checklist) and stop at the first
output that does not match.

## What stays

- **Board, backlog, projects and job files.** They live in R2 behind the Worker.
  The migration never writes them.
- **The local home.** `config.md`, `paths.md`, `routes.md`, `launchers.json`,
  `.env`, `jobs/` and `worktrees/` stay as they are. The Rust binary reads the
  same files the Python runtime did.
- **A backup of what changes.** Before catch-up writes anything, it prints
  `backup: <home>/backups/catch-up-<stamp>`. That folder (mode 700) holds:
  - `skill/factory/`, the old installed skill;
  - `crontab.txt` on cron hosts, `LaunchAgents/factory.*.plist` on macOS,
    `systemd/factory-*.service` on systemd hosts, and `config/factory-*.cmd` on
    Windows;
  - `home/`, with `config.md`, `paths.md`, `routes.md`, `launchers.json`,
    `stalls.json` and `.env`.

  Catch-up never prunes these folders.

## Prerequisites

- **Rust.** Install rustup from https://rustup.rs. The installer finds `cargo` on
  `PATH`, or in `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows) when a
  non-login shell leaves it off `PATH`. Without cargo the install stops with
  `install: cargo not found` and changes nothing.
- **Windows.**
  - Use the GNU toolchain. Run `rustup-init.exe --default-host x86_64-pc-windows-gnu`,
    or `rustup set default-host x86_64-pc-windows-gnu` on an existing install.
  - Install MSYS2 and its mingw-w64 binutils (`pacman -S mingw-w64-x86_64-binutils`).
    Append `C:\msys64\mingw64\bin` to the end of your user `PATH`, then open a new
    shell.
  - Pull a main that has the root `.gitattributes`. It forces LF line endings, so a
    checkout with `core.autocrlf=true` installs the same files as everyone else.
- **Git, gh and a clean factory checkout.** `paths.md` names the checkout under
  `factory:`.

## Pause intake first

Stop new work while the host changes over. Jobs already running keep running.

1. On the dashboard, open Settings > Hosts and use the Stop factory form. Give a
   reason such as `migrating <host> to Rust`. An agent can set the same flag with
   `<runtime> halt set --reason "migrating <host> to Rust"`, where `<runtime>` is
   whatever runs the factory on this host today.
2. Check it with `<runtime> halt status`. It prints the record as JSON, with
   `"halted": true`.

Only a person can clear the flag. Do that last, from the banner's Resume factory
button, after every host you are migrating passes its checks.

## Pick your upgrade path

Read `<installed-skill>/bundle.json`. The installed skill is the `factory` folder
inside your skills root. `~/.claude/skills/factory` is often a symlink into
`~/.agents/skills`. Resolve it with `readlink -f ~/.claude/skills/factory`, and use
the parent of the result as the skills root.

Take the first row that matches.

| What `bundle.json` shows                     | Path                                                    |
| -------------------------------------------- | ------------------------------------------------------- |
| No `entry` key, or no `bundle.json` at all   | [Fresh install](#fresh-install-or-a-python-only-bundle) |
| On Windows, a `commit` older than `7568c536` | [Fresh install](#fresh-install-or-a-python-only-bundle) |
| On a Mac, a `commit` older than `e137ba69`   | [Catch-up](#catch-up), once with `TMPDIR`               |
| `"entry": ["runtime/factory"]`               | [Catch-up](#catch-up)                                   |

To test a commit, run
`git -C <checkout> merge-base --is-ancestor <fix> <commit>`, where `<commit>` is
the `commit` in `bundle.json`. Exit 0 means the install already has the fix, and
exit 1 means it is older.

### Catch-up

```sh
<installed-skill>/runtime/factory catch-up
```

It fetches the checkout's origin and backs up. Then it builds origin's binary
through the build cache, installs the skill and fixes the pickup timer. It ends with
`catch-up: done`.

**A Mac on a build older than `e137ba69`.** The old binary builds under the
default `TMPDIR`, which sits below `/var`, a symlink. The installer refuses that
path. Run this once:

```sh
mkdir -p "$HOME/.cache/factory-tmp"
TMPDIR="$HOME/.cache/factory-tmp/" <installed-skill>/runtime/factory catch-up
```

The new binary picks a temp folder with no symlinked parent, so later runs work
with the default `TMPDIR`.

### Fresh install, or a Python-only bundle

Use this for a new host, for a bundle without `entry`, and for a Windows host on
a build older than `7568c536`. The install script builds the binary from the
checkout, so the old runtime plays no part.

The script replaces the old skill and keeps no copy of it. Copy it into the
factory home first, so you can roll back:

```sh
mkdir -p <home>/backups
cp -R <skills root>/factory <home>/backups/pre-rust-skill
```

On Windows, use `Copy-Item -Recurse` in PowerShell.

Then update the checkout and install:

```sh
git -C <checkout> fetch origin
git -C <checkout> checkout main
git -C <checkout> merge --ff-only origin/main
sh <checkout>/tools/install.sh <skills root>
```

On Windows, run `<checkout>\tools\install.ps1 <skills root>` in PowerShell.

The first build takes one to two minutes. The script prints the installed skill
path, for example `/home/you/.agents/skills/factory`, and exits 0. Then run
catch-up once. It rewrites a pickup timer that still runs Python, after a backup.

```sh
<installed-skill>/runtime/factory catch-up
```

## Check the host

| Check                       | Command                                              | Expect                                                            |
| --------------------------- | ---------------------------------------------------- | ----------------------------------------------------------------- |
| The bundle names the binary | `cat <installed-skill>/bundle.json`                  | `"entry": ["runtime/factory"]` and a 64-character `binary_sha256` |
| Nothing is left to do       | `<installed-skill>/runtime/factory catch-up`         | the last line is `catch-up: nothing new`, in about a second       |
| Pickup runs                 | `<installed-skill>/runtime/factory pickup --dry-run` | exit 0; it launches nothing while intake is paused                |
| The daemon mirror answers   | `time <installed-skill>/runtime/factory job <id>`    | the job's JSON in milliseconds, once the daemon runs              |

Without the daemon, `job <id>` reads from the Worker and takes about a second.
Measured on 2026-11-23, the mirror answered in under 10 ms on WSL and in 50 to
70 ms on a Mac.

## Set up automations

### The daemon is the scheduler

```sh
<installed-skill>/runtime/factory daemon setup --install
```

It installs the daemon as a service and removes the pickup timer. It exits 0
and prints `daemon setup (<os>, <scheduler>): running`. Run `daemon setup` with no flag to
see its state, its last pass and its log, `<home>/logs/daemon.log`. The daemon trims
that log to its last 1 MiB once it passes 5 MiB. On Windows the restart loop sends the
daemon's startup output, such as an error that stops it before it opens its log, to
`<home>\logs\daemon-boot.log`, which the loop empties once it passes 256 KiB.

| Host                         | Service                                                                                                                            |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| macOS                        | launchd agent `~/Library/LaunchAgents/factory.daemon.plist`, with `KeepAlive` and `RunAtLoad`                                      |
| Linux or WSL with systemd    | user unit `~/.config/systemd/user/factory-daemon.service`                                                                          |
| Linux or WSL without systemd | two crontab lines: `@reboot` and a `*/5` line that restarts a dead daemon                                                          |
| Windows                      | Task Scheduler task `factory-daemon` at logon, running `%USERPROFILE%\.config\factory-daemon.cmd`, a loop that restarts the daemon |

What the daemon does:

- It keeps a mirror of the board and serves it to this host's commands on
  `<home>/daemon.sock` (on Windows, a loopback port named in `<home>/daemon.addr`).
- It runs pickup about 2 seconds after a board or backlog change, when a local
  `factory <id>` agent stops, and after 5 minutes with no pass.
- It still runs a pass every 5 minutes while it cannot reach the Worker.

The service starts at boot or logon. WSL without systemd needs two more pieces,
because WSL starts no cron on its own:

- `/etc/wsl.conf` with this section:

  ```ini
  [boot]
  command="service cron start"
  ```

- A Windows Task Scheduler task that starts WSL at logon, so the `[boot]` command
  runs.

A cron-only host restarts the daemon from the `*/5` line, so after a crash there
can be a gap of up to 5 minutes before the new one runs. Catch-up does not wait
for the line: it stops the old daemon, starts the new binary itself, and prints
`daemon: restarted (pid N)` once the new daemon holds its lock.

If macOS reports a disabled label, enable it and install again:

```sh
launchctl enable gui/$(id -u)/factory.daemon
<installed-skill>/runtime/factory daemon setup --install
```

### The pickup timer goes away

Where the daemon runs, the host has no pickup timer. `daemon setup --install`
removes it, and backs up the crontab, plist or task XML to
`<home>/backups/daemon-setup-<stamp>/` first. If a timer survives, the next
catch-up removes it and prints `pickup timer: replaced by the daemon`.

To go back to the timer, run `daemon setup --uninstall`. It puts the pickup timer
back before it removes the service.

### The retro push timer goes too

The daemon pushes the retro digest at the timer's times, so the host has no
`retro push` timer either. `daemon setup --install` removes it with the same
backup, and catch-up removes a survivor and prints
`retro-push timer: replaced by the daemon`. `daemon setup --uninstall` leaves
it off and prints `retro-push timer: left off`, since the host may never have
had one or had its own times; restore it from the backup or set it up again. A
host without the daemon keeps the timer its
[schedules reference](../skills/factory/references/schedules.md#daily-retro-push)
lines set up; point it at the binary as the next section shows.

## Update old automations

The installed skill no longer has `runtime/factory.py`. The one-release stub that
ran the binary beside it is gone, so anything that still calls
`python3 <installed-skill>/runtime/factory.py`, or
`py -3 <installed-skill>\runtime\factory.py` on Windows, now fails. Move every
caller to `runtime/factory` (`runtime\factory.exe` on Windows).

Find them:

| Where                            | Find with                                                                                                                                                       |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| crontab                          | `crontab -l \| grep factory.py`                                                                                                                                 |
| launchd                          | `grep -l factory.py ~/Library/LaunchAgents/*.plist`                                                                                                             |
| Windows wrappers                 | `findstr /m factory.py %USERPROFILE%\.config\*.cmd`                                                                                                             |
| Windows tasks                    | `schtasks /Query /FO LIST /V \| findstr factory.py`                                                                                                             |
| Home config and schedule prompts | `find <home> -maxdepth 1 -type f \( -name '*.md' -o -name '*.json' \) -exec grep -l factory.py {} +`, then the same in any folder that holds your agent prompts |

Each search prints the files or lines that name `factory.py`. No output means no
matches.

Fix them:

- **Pickup and daemon units.** Don't edit them by hand. Run
  `daemon setup --install`, which writes the units for the binary.
- **Catch-up does the timers for you.** From the release after job 2251, catch-up
  rewrites a crontab line, `factory.*.plist`, `factory-*.service` or
  `factory-*.cmd` that runs `runtime/factory.py`, and a `stalls.json` adapter that
  runs `runtime/paseo_stalls.py`, to the binary. It reloads launchd and systemd and
  prints one `timers:` line each. The steps below are for anything else.
- **Retro push and other lines.** Replace `python3 <installed-skill>/runtime/factory.py`
  with `<installed-skill>/runtime/factory`, and `py -3 <installed-skill>\runtime\factory.py`
  with `<installed-skill>\runtime\factory.exe`. Keep the arguments. On macOS, load
  a changed plist again with `launchctl bootout` and `launchctl bootstrap`.
- **Prompts.** Agents resolve the binary with `where`, which prints it as
  `runtime`. Replace a hard-coded `factory.py` path with that value. Catch-up
  rewrites `<home>/retro-schedule-prompt.md` and factory's own Paseo retro
  schedule for you, and names each other Paseo schedule that still runs
  `factory.py`. `doctor` lists what is left.

## Roll back

There is no Python runtime switch any more. To go back, restore the backup that
catch-up printed as `backup: <path>`:

1. If you installed the daemon, run `daemon setup --uninstall` with the new binary.
   It puts the pickup timer back and removes the service.
2. Copy `<backup>/skill/factory` back over `factory` in the skills root. On the
   fresh install path, use `<home>/backups/pre-rust-skill` instead.
3. Restore the timers. Run `crontab <backup>/crontab.txt`, or copy the plists back
   to `~/Library/LaunchAgents`, or the `.cmd` files back to `%USERPROFILE%\.config`.
   On a systemd host, copy `<backup>/systemd/*` back to `~/.config/systemd/user`
   and run `systemctl --user daemon-reload`.
4. Copy the files in `<backup>/home/` that differ back into the factory home with
   `cp -p`, which keeps their modes. Leave `.env` alone unless it changed, so a
   rotated token does not come back. `diff <backup>/home/.env <home>/.env` shows
   whether it changed.

## Agent checklist

Run each step in order. Compare the output with the expected column. Stop at the
first mismatch and report the step, the command and its full output. `$SKILL` is
the installed skill folder and `$RT` is `$SKILL/runtime/factory`. `<runtime>` is
`$RT` when `bundle.json` has `entry`, otherwise `python3 $SKILL/runtime/factory.py`.
Use `factory.exe` and PowerShell paths on Windows.

1. **Find the skill.** `readlink -f ~/.claude/skills/factory`, or the
   `--destination` your host uses. Expect a folder that holds `SKILL.md`. Set
   `$SKILL` to it. No folder means a fresh host. Pause intake from the dashboard,
   skip to step 4 and take the Fresh install path.
2. **Read the bundle.** `cat $SKILL/bundle.json`. Pick the path from the table in
   [Pick your upgrade path](#pick-your-upgrade-path). The `entry` key also sets
   `<runtime>` for the next step.
3. **Pause intake.** `<runtime> halt status`. Expect `"halted": true`. If it is not, run
   `<runtime> halt set --reason "migrating <host> to Rust"` and check again. Never
   clear the halt.
4. **Upgrade.**
   - Catch-up path: `$RT catch-up`. Expect exit 0 and a last line of
     `catch-up: done` or `catch-up: nothing new`. On a Mac older than `e137ba69`,
     use the `TMPDIR` form.
   - Fresh install path: the backup copy and the four commands under
     [Fresh install](#fresh-install-or-a-python-only-bundle). A fresh host has no
     skill to copy, so it skips the backup copy. Expect exit 0 and the skill path
     as the last line. On a fresh host, set `$SKILL` to that path. Then
     `$RT catch-up`, exit 0.
5. **Check the bundle.** `cat $SKILL/bundle.json`. Expect `"entry": ["runtime/factory"]`
   and a `binary_sha256`.
6. **Check catch-up is idle.** `$RT catch-up`. Expect exit 0 and
   `catch-up: nothing new` as the last line.
7. **Check pickup.** `$RT pickup --dry-run`. Expect exit 0.
8. **Install the daemon.** `$RT daemon setup --install`. Expect exit 0 and the
   line `daemon setup (<os>, <scheduler>): running`. If your harness blocks the crontab, launchctl
   or schtasks call, stop and give the person the exact command.
9. **Check the mirror.** `$RT job <id>` for any job on the board. Expect its JSON
   and well under a second.
10. **Find old callers.** Run each search in [Update old automations](#update-old-automations).
    Expect no matches. Update any match, then search again.
11. **Hand back.** Tell the person the host is migrated, and that they can clear
    the halt from the dashboard banner once every host is done.

## Troubleshooting

### `cannot safely install factory` on a Mac

Catch-up made its source folder under `/var/folders/...`. `/var` is a symlink to
`/private/var`, and the installer refuses any path with a symlinked parent. The
skill stays intact. Run catch-up once with
`TMPDIR="$HOME/.cache/factory-tmp/"`, as in [Catch-up](#catch-up). Builds from
`e137ba69` onward avoid it.

### `Bootstrap failed: 5: Input/output error`

launchd was still unloading the old agent when catch-up or `daemon setup` asked
it to load the new one. The new plist is on disk but not loaded. Wait a few
seconds and run `daemon setup --install` again. Builds from `e137ba69` onward
wait for the unload and retry once.

### The Windows build cannot find `dlltool`

The GNU Rust toolchain needs binutils from MSYS2. Install
`mingw-w64-x86_64-binutils`, append `C:\msys64\mingw64\bin` to the end of your
user `PATH`, and
open a new shell before you build again.

### CRLF files in a Windows checkout

Git for Windows defaults to `core.autocrlf=true`. Before `.gitattributes` landed
(`ad693b44`), that put CRLF files in the skill, and its tree hash differed from
main's. Pull main. Then, in a checkout with no local changes, rewrite the files
with LF endings by running `git rm -r -q --cached .` and `git reset --hard`.
Install again.

### `Access is denied` on `.build.lock` on Windows

Catch-up failed with `install: Rust build failed (... Access is denied ... \build-src\.build.lock)`.
Binaries older than `7568c536` opened the lock file append-only, and Windows
refuses a lock on that handle. Install once with `tools\install.ps1` from an
updated checkout. Later catch-ups work.

### Worker error 1102, `Worker exceeded CPU time limit`

Board writes failed now and then with HTTP 503. `3a4a3852` cut the CPU of a full
PUT, and `a1bba56c` stores board rows in the BoardEvents room, so a row PATCH
costs only its rows. A board write that still gets a 503 retries
up to three times, 1, 2 and 4 seconds apart. Before each retry it reads the
board, and a write that already landed does not run again. To check that the
Worker runs main, look for `worker: deployed at <commit>` in catch-up's output.

### A pickup timer is still installed beside the daemon

Before `7cc2941c`, catch-up could print
`pickup timer: no pickup timer installed: the daemon service runs pickup` while
the crontab still held the `*/5 ... pickup` line. Run `daemon setup --install`.
It backs up the crontab and prints `removed cron timer: crontab`. Current
catch-up removes it and prints `pickup timer: replaced by the daemon`.
