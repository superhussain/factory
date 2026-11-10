#!/usr/bin/env python3
"""Compare the live review and build notes with docs/repo-security.md.

Run from the repository root: factory.py pipeline factory | python3 tools/stage-notes-check.py
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


def live_note(prompts, stage):
    match = re.search(
        rf'<operator-preferences source="pipeline:factory@\d+" stage="{stage}" trust="data">\n(.*?)\n</operator-preferences>',
        prompts.get(stage, ""),
        re.S,
    )
    return match.group(1) if match else None


def main():
    try:
        view = json.loads(sys.stdin.buffer.read())
    except ValueError:
        view = None
    prompts = view.get("prompts", {}) if isinstance(view, dict) else None
    if not (isinstance(prompts, dict) and all(isinstance(prompts.get(s, ""), str) for s in NOTES)):
        print("stage-notes-check: stdin is not a factory.py pipeline view", file=sys.stderr)
        return 2
    doc = DOC.read_text(encoding="utf8")
    status = 0
    for stage, heading in NOTES.items():
        committed = doc_note(doc, heading)
        live = live_note(prompts, stage)
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
