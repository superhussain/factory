#!/usr/bin/env python3
"""Report and archive Paseo sessions stalled on a Claude usage limit, for factory pickup.

  paseo_stalls.py list            print {"version": 1, "stalls": [...]} as JSON on stdout
  paseo_stalls.py archive <id>    archive one session after re-checking that it is still stalled

Stdlib only, Python 3.8+. Exit codes: 0 ok, 1 failure, 2 usage, 3 archive refused because
the session is no longer stalled. It never imports factory. See references/pickup.md.
"""

import json
import os
import re
import shutil
import subprocess
import sys
from datetime import datetime, timedelta, timezone

try:
    import zoneinfo
except ImportError:  # Python 3.8
    zoneinfo = None

PASEO_TIMEOUT_SECONDS = 20
MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun",
          "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
LIMIT_RE = re.compile(
    "You['’]ve hit your (session|weekly) limit · resets "
    "(?:(" + "|".join(MONTHS) + r") (\d{1,2}), )?"
    r"(\d{1,2})(?::(\d{2}))?(am|pm) "
    r"\(([A-Za-z_]+(?:/[A-Za-z0-9_+-]+)*)\)"
)
ID_RE = re.compile(r"[0-9a-fA-F][0-9a-fA-F-]{7,63}")
UTC = timezone.utc
USAGE = "usage: paseo_stalls.py list | archive <id>"


def parse_iso(text):
    # fromisoformat on 3.8 takes 3 or 6 fraction digits, which paseo emits.
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def iso_utc(dt):
    return dt.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def resolve_reset(m, message_at):
    """The reset instant from the limit line and the message time, or None when zoneinfo
    is missing or the zone is unknown: that agent is left out."""
    _, mon, day, hour, minute, ampm, tz = m.groups()
    if zoneinfo is None:
        return None
    try:
        zone = zoneinfo.ZoneInfo(tz)
    except (KeyError, ValueError, OSError):
        return None
    h = int(hour) % 12 + (12 if ampm == "pm" else 0)
    mi = int(minute or 0)
    wall = message_at.astimezone(zone).replace(tzinfo=None)

    def instant(local):
        return local.replace(tzinfo=zone).astimezone(UTC)
    if mon is None:
        reset = instant(wall.replace(hour=h, minute=mi, second=0, microsecond=0))
        if reset <= message_at:
            reset = instant((wall + timedelta(days=1)).replace(hour=h, minute=mi, second=0, microsecond=0))
        return reset
    month = MONTHS.index(mon) + 1
    reset = instant(datetime(wall.year, month, int(day), h, mi))
    if reset < message_at - timedelta(days=1):
        reset = instant(datetime(wall.year + 1, month, int(day), h, mi))
    return reset


def call(argv):
    """(exit code, stdout) from one paseo call, or nonzero when paseo fails or is slow."""
    program = shutil.which("paseo")
    if program is None:
        print("paseo_stalls.py: paseo is not on PATH", file=sys.stderr)
        sys.exit(1)
    if os.environ.get("PASEO_HOME"):
        argv = ["--home", os.environ["PASEO_HOME"]] + argv
    try:
        p = subprocess.run([program] + argv, capture_output=True, text=True,
                           timeout=PASEO_TIMEOUT_SECONDS)
    except (subprocess.TimeoutExpired, OSError):
        return 1, ""
    return p.returncode, p.stdout


def stalled(agent_id):
    """(match, info) while the agent sits idle and unarchived on a Claude limit line, else None."""
    code, text = call(["logs", agent_id, "--tail", "1", "--filter", "text"])
    if code != 0:
        return None
    m = LIMIT_RE.fullmatch(text.strip())
    if not m:
        return None
    code, raw = call(["inspect", agent_id, "--json"])
    if code != 0:
        return None
    try:
        info = json.loads(raw)
    except ValueError:
        return None
    if not isinstance(info, dict) or info.get("Status") != "idle" or info.get("Archived") is not False:
        return None
    return m, info


def list_stalls():
    """One JSON document on stdout naming every idle, unarchived Claude limit stall."""
    code, raw = call(["ls", "-g", "--json"])
    try:
        agents = json.loads(raw) if code == 0 else None
    except ValueError:
        agents = None
    if not isinstance(agents, list):
        print("paseo_stalls.py: paseo ls failed", file=sys.stderr)
        sys.exit(1)
    stalls = []
    for agent in agents:
        if not isinstance(agent, dict):
            continue
        agent_id = agent.get("id")
        provider = agent.get("provider")
        if agent.get("status") != "idle" or not isinstance(provider, str) \
                or provider.split("/", 1)[0] != "claude" or not isinstance(agent_id, str) \
                or not ID_RE.fullmatch(agent_id):
            continue
        found = stalled(agent_id)
        if not found:
            continue
        m, info = found
        try:
            message_at = parse_iso(info["UpdatedAt"])
        except (KeyError, TypeError, ValueError):
            continue
        reset = resolve_reset(m, message_at)
        if reset is None:
            continue
        stalls.append({"session": agent_id, "reset": iso_utc(reset),
                       "provider": info.get("Provider"), "model": info.get("Model") or ""})
    print(json.dumps({"version": 1, "stalls": stalls}))


def archive(agent_id):
    """Archive only a session that is still stalled; a woken one exits 3 and is kept."""
    if stalled(agent_id) is None:
        sys.exit(3)
    code, _ = call(["archive", agent_id])
    if code != 0:
        print(f"paseo_stalls.py: paseo archive exit {code}", file=sys.stderr)
        sys.exit(1)


def main(argv):
    if len(argv) == 1 and argv[0] == "list":
        list_stalls()
    elif len(argv) == 2 and argv[0] == "archive" and ID_RE.fullmatch(argv[1]):
        archive(argv[1])
    else:
        print(USAGE, file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main(sys.argv[1:])
