#!/usr/bin/env python3
"""Compare the live review and build notes with docs/repo-security.md.

Usage: factory.py pipeline factory | python3 tools/stage-notes-check.py
Prints one line per stage. Exit 0 if both match, 1 if not, 2 if stdin is not a pipeline view.
"""
import json
import re
import sys
from pathlib import Path

DOC = Path(__file__).resolve().parent.parent / "docs" / "repo-security.md"
NOTES = {"review": "Review stage note", "build": "Build stage note"}


def doc_note(text, heading):
    match = re.search(rf"^### {heading}\n\n```text\n(.*?)\n```$", text, re.M | re.S)
    return match.group(1) if match else None


def live_note(view, stage):
    match = re.search(
        rf'<operator-preferences source="pipeline:factory@\d+" stage="{stage}" trust="data">\n(.*?)\n</operator-preferences>',
        view.get("prompts", {}).get(stage, ""),
        re.S,
    )
    return match.group(1) if match else None


def main(argv=None):
    try:
        view = json.load(sys.stdin)
    except json.JSONDecodeError:
        view = None
    if not isinstance(view, dict):
        print("stage-notes-check: stdin is not a factory.py pipeline view", file=sys.stderr)
        return 2
    doc = DOC.read_text(encoding="utf8")
    status = 0
    for stage, heading in NOTES.items():
        committed = doc_note(doc, heading)
        live = live_note(view, stage)
        if committed is None:
            line = f"{stage}: no note in docs/repo-security.md"
        elif live is None:
            line = f"{stage}: missing from the live pipeline"
        elif live != committed:
            line = f"{stage}: differs from docs/repo-security.md"
        else:
            line = f"{stage}: matches"
        if not line.endswith("matches"):
            status = 1
        print(line)
    return status


if __name__ == "__main__":
    sys.exit(main())
