#!/bin/sh
# Deprecated stub, kept for one release: the factory CLI is the binary runtime/factory (factory.exe on
# Windows) beside this file. Old cron lines, launchd plists and prompts that run
# `python3 <skill>/runtime/factory.py` or `sh <skill>/runtime/factory.py` still reach the binary through it.
# The Python runtime and FACTORY_RUNTIME=python are gone; to roll back, restore
# <home>/backups/catch-up-*/skill/factory (docs/upgrade.md).
''':'
binary="$(dirname "$0")/factory"
if [ ! -x "$binary" ]; then
  echo "factory.py: no factory binary at $binary; from a factory checkout run sh tools/install.sh <skills folder> (tools\\install.ps1 on Windows)" >&2
  exit 127
fi
echo "factory.py is deprecated and runs $binary; call the binary directly. FACTORY_RUNTIME=python is gone; to roll back, restore <home>/backups/catch-up-*/skill/factory" >&2
exec "$binary" "$@"
'''
import os, subprocess, sys
binary = os.path.join(os.path.dirname(os.path.abspath(__file__)), "factory.exe" if os.name == "nt" else "factory")
if not os.path.isfile(binary):
    print(f"factory.py: no factory binary at {binary}; from a factory checkout run sh tools/install.sh <skills folder> "
          "(tools\\install.ps1 on Windows)", file=sys.stderr)
    sys.exit(127)
print(f"factory.py is deprecated and runs {binary}; call the binary directly. FACTORY_RUNTIME=python is gone; "
      "to roll back, restore <home>/backups/catch-up-*/skill/factory", file=sys.stderr, flush=True)
sys.exit(subprocess.call([binary] + sys.argv[1:]))
