"""Replay stub for gh, paseo and git in parity runs.

bin/<tool> runs `stub.py <tool> ARGS...`. It appends {"argv": ARGS} to
$FACTORY_STUB_DIR/<tool>.calls.jsonl, then replays the first entry in
$FACTORY_STUB_DIR/<tool>.json whose argv equals ARGS, or exits 127.
"""
import json
import os
import shlex
import sys
from pathlib import Path


def launcher(bin_dir, tool):
    path = Path(bin_dir) / tool
    command = " ".join(shlex.quote(part) for part in (sys.executable, "-I", str(Path(__file__).resolve()), tool))
    path.write_text(f"#!/bin/sh\nexec {command} \"$@\"\n")
    path.chmod(0o755)


def main(argv):
    tool, args = argv[0], argv[1:]
    stub = Path(os.environ["FACTORY_STUB_DIR"])
    with open(stub / f"{tool}.calls.jsonl", "a", encoding="utf-8") as log:
        log.write(json.dumps({"argv": args}) + "\n")
    recorded = stub / f"{tool}.json"
    entries = json.loads(recorded.read_text(encoding="utf-8")) if recorded.exists() else []
    for entry in entries:
        if entry["argv"] == args:
            sys.stdout.buffer.write(entry["stdout"].encode("utf-8"))
            sys.stderr.buffer.write(entry["stderr"].encode("utf-8"))
            return entry["code"]
    sys.stderr.write(f"stub: no recorded response for {' '.join([tool] + args)}\n")
    return 127


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
