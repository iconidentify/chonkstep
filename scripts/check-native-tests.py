#!/usr/bin/env python3
"""Run a native libtest gate and require named regressions to actually pass."""

import argparse
import re
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--require", action="append", required=True,
                        help="fully qualified test name that must report ok")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        parser.error("a test command is required after --")

    started = time.monotonic()
    passed = set()
    # Stream diagnostics so a compile or native assertion failure remains
    # visible. Retain only names, not the potentially large build/test log.
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          text=True) as child:
        for line in child.stdout:
            print(line, end="", flush=True)
            match = re.fullmatch(r"test (\S+) \.\.\. ok\n?", line)
            if match:
                passed.add(match[1])
        status = child.wait()

    print(f"Native gate elapsed: {time.monotonic() - started:.2f}s", flush=True)
    if status:
        return status if status > 0 else 1
    missing = sorted(set(args.require) - passed)
    if missing:
        print("Native gate did not pass required tests: " + ", ".join(missing), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
