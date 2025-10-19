#!/usr/bin/env python3
"""
Run grim_engine headlessly through the intro cutscene and verify that Manny's
office resumes once the movie completes.

Usage:
    nix-shell --run 'python tools/check_intro_resume.py'
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Iterable

ROOT = Path(__file__).resolve().parents[1]
ENGINE_COMMAND = [
    "cargo",
    "run",
    "-p",
    "grim_engine",
    "--",
    "--headless",
    "--verbose",
]

# Events that confirm intro completion and office activation.
REQUIRED_MARKERS = [
    "manny_office.resume",
    "cut_scene.fullscreen.end intro",
    "actor.mo.tube.interest_actor.complete_chore",
]

TIMEOUT_SECONDS = 20.0


def stream_output(proc: subprocess.Popen) -> Iterable[str]:
    assert proc.stdout is not None
    for line in proc.stdout:
        yield line.rstrip("\n")


def main() -> int:
    env = os.environ.copy()
    start = time.monotonic()
    command = ["timeout", "15"] + ENGINE_COMMAND

    proc = subprocess.Popen(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    matched = {marker: False for marker in REQUIRED_MARKERS}

    try:
        for line in stream_output(proc):
            print(line)
            for marker in matched:
                if (not matched[marker]) and marker in line:
                    matched[marker] = True
                    print(f"[check_intro_resume] observed marker: {marker}")

            if all(matched.values()):
                print("[check_intro_resume] all markers observed; stopping engine")
                break

            if time.monotonic() - start > TIMEOUT_SECONDS:
                print("[check_intro_resume] timed out before all markers appeared", file=sys.stderr)
                break
    finally:
        if proc.poll() is None:
            try:
                proc.send_signal(signal.SIGINT)
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()

    success = all(matched.values())
    if not success:
        missing = [marker for marker, seen in matched.items() if not seen]
        print(f"[check_intro_resume] missing markers: {', '.join(missing)}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
