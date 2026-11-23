"""Startup check: the binary's median no-argument run must take at most a tenth of Python's.

python3 tests/parity/startup.py [--bin PATH] [--runs N]
"""
import argparse
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "tests" / "cli"))
from support import FACTORY_BIN, SKILL

PYTHON = [sys.executable, "-B", str(SKILL / "runtime/factory.py")]


def median_ms(command, runs):
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True, timeout=60)
        times.append((time.perf_counter() - start) * 1000)
    return statistics.median(times)


def main(argv):
    parser = argparse.ArgumentParser(prog="startup.py")
    parser.add_argument("--bin", default=FACTORY_BIN)
    parser.add_argument("--runs", type=int, default=20)
    args = parser.parse_args(argv)
    if not args.bin:
        print("startup: --bin or FACTORY_BIN required", file=sys.stderr)
        return 2
    python = median_ms(PYTHON, args.runs)
    binary = median_ms([os.path.abspath(args.bin)], args.runs)
    print(f"startup: python {python:.0f} ms, bin {binary:.0f} ms (median of {args.runs})")
    return 0 if binary <= python / 10 else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
