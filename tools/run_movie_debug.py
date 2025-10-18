#!/usr/bin/env python3
"""Quickly launch grim_engine and grim_viewer for movie playback debugging."""

from __future__ import annotations

import argparse
import os
import shlex
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import List, Optional, Sequence

ROOT_DIR = Path(__file__).resolve().parents[1]
LIVE_PREVIEW_LOG = Path("/tmp/live_preview.log")


def current_log_offset(path: Path) -> int:
    try:
        return path.stat().st_size
    except FileNotFoundError:
        return 0


@dataclass
class ManagedProcess:
    name: str
    process: subprocess.Popen


def spawn(name: str, command: Sequence[str], *, inherit_env: bool = False) -> ManagedProcess:
    env = os.environ.copy() if inherit_env else None
    print(f"[run_movie_debug] launching {name}: {' '.join(command)}")
    proc = subprocess.Popen(command, cwd=ROOT_DIR, env=env)
    return ManagedProcess(name, proc)


def wait_for_handshake(offset: int, timeout: float) -> None:
    deadline = time.monotonic() + max(0.1, timeout)
    offset = max(0, offset)
    print("[run_movie_debug] waiting for viewer handshake …")
    while time.monotonic() < deadline:
        try:
            with LIVE_PREVIEW_LOG.open("rb") as handle:
                handle.seek(offset)
                data = handle.read()
                offset = handle.tell()
                if b"viewer_ready.open" in data:
                    print("[run_movie_debug] viewer handshake acknowledged")
                    return
        except FileNotFoundError:
            pass
        time.sleep(0.2)
    raise RuntimeError("viewer handshake timed out")


def wait_for_process_exit(monitored: Sequence[ManagedProcess]) -> int:
    poll_interval = 0.25
    while True:
        for managed in monitored:
            code = managed.process.poll()
            if code is not None:
                print(
                    f"[run_movie_debug] {managed.name} exited with status {code}; shutting down session"
                )
                return code
        time.sleep(poll_interval)


def shutdown_processes(processes: Sequence[ManagedProcess]) -> None:
    for managed in processes:
        proc = managed.process
        if proc.poll() is not None:
            continue
        print(f"[run_movie_debug] terminating {managed.name}")
        try:
            proc.send_signal(signal.SIGTERM)
        except ProcessLookupError:
            continue
    for managed in processes:
        proc = managed.process
        if proc.poll() is not None:
            continue
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            print(f"[run_movie_debug] force killing {managed.name}")
            try:
                proc.kill()
            except ProcessLookupError:
                pass


def build_engine_command(args: argparse.Namespace) -> List[str]:
    command: List[str] = ["cargo", "run"]
    if args.release:
        command.append("--release")
    command.extend(["-p", "grim_engine", "--", "--stream-bind", args.engine_bind])
    if args.engine_verbose:
        command.append("--verbose")
    if args.engine_extra:
        command.extend(shlex.split(args.engine_extra))
    return command


def build_viewer_command(args: argparse.Namespace) -> List[str]:
    command: List[str] = ["cargo", "run"]
    if args.release:
        command.append("--release")
    command.extend(
        [
            "-p",
            "grim_viewer",
            "--",
            "--engine-stream",
            args.engine_bind,
            "--window-width",
            str(args.window_width),
            "--window-height",
            str(args.window_height),
            "--no-retail",
        ]
    )
    if args.viewer_extra:
        command.extend(shlex.split(args.viewer_extra))
    return command


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--engine-bind",
        default="127.0.0.1:17500",
        help="Address for grim_engine to bind (viewer connects here)",
    )
    parser.add_argument(
        "--window-width",
        type=int,
        default=1280,
        help="Initial grim_viewer window width",
    )
    parser.add_argument(
        "--window-height",
        type=int,
        default=720,
        help="Initial grim_viewer window height",
    )
    parser.add_argument(
        "--engine-verbose",
        action="store_true",
        help="Start grim_engine with verbose Lua logging",
    )
    parser.add_argument(
        "--engine-extra",
        default="",
        help="Additional arguments passed to grim_engine (quoted string, appended after '--')",
    )
    parser.add_argument(
        "--viewer-extra",
        default="",
        help="Additional arguments passed to grim_viewer (quoted string, appended after '--')",
    )
    parser.add_argument(
        "--release",
        action="store_true",
        help="Run both binaries with cargo --release",
    )
    parser.add_argument(
        "--no-handshake-wait",
        action="store_true",
        help="Do not wait for the viewer_ready handshake before returning control",
    )
    parser.add_argument(
        "--handshake-timeout",
        type=float,
        default=10.0,
        help="Seconds to wait for the viewer_ready handshake (default: 10s)",
    )

    args = parser.parse_args()

    procs: List[ManagedProcess] = []
    handshake_offset = 0

    try:
        if not args.no_handshake_wait:
            handshake_offset = current_log_offset(LIVE_PREVIEW_LOG)

        engine_cmd = build_engine_command(args)
        engine_proc = spawn("grim_engine", engine_cmd)
        procs.append(engine_proc)

        # Give the engine a brief moment to bind the socket before the viewer connects.
        time.sleep(0.5)

        viewer_cmd = build_viewer_command(args)
        viewer_proc = spawn("grim_viewer", viewer_cmd, inherit_env=True)
        procs.append(viewer_proc)

        if not args.no_handshake_wait:
            wait_for_handshake(handshake_offset, args.handshake_timeout)

        print("[run_movie_debug] session running (Ctrl+C to stop)")
        exit_code = wait_for_process_exit(procs)
    except KeyboardInterrupt:
        print("[run_movie_debug] interrupted; shutting down children")
        exit_code = 130
    except RuntimeError as err:
        print(f"[run_movie_debug] error: {err}", file=sys.stderr)
        exit_code = 1
    finally:
        shutdown_processes(procs)

    sys.exit(exit_code)


if __name__ == "__main__":
    main()
