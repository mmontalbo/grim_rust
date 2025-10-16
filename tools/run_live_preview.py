#!/usr/bin/env python3
"""Launch retail capture, grim_engine, and grim_viewer together for live preview."""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import List, Sequence

ROOT_DIR = Path(__file__).resolve().parents[1]

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--retail-addr", default="127.0.0.1:17400", help="GrimStream address for retail capture")
    parser.add_argument("--engine-addr", default="127.0.0.1:17500", help="GrimStream address for grim_engine")
    parser.add_argument("--width", type=int, default=1280, help="Captured framebuffer width")
    parser.add_argument("--height", type=int, default=720, help="Captured framebuffer height")
    parser.add_argument("--fps", type=float, default=30.0, help="Captured framerate")
    parser.add_argument("--ffmpeg", default="ffmpeg", help="ffmpeg executable for capture")

    parser.add_argument("--release", action="store_true", help="Run all cargo commands with --release")
    parser.add_argument("--steam-run", action="store_true", help="Wrap the viewer invocation in steam-run")
    parser.add_argument("--no-capture", action="store_true", help="Skip launching live_retail_capture")
    parser.add_argument("--no-engine", action="store_true", help="Skip launching grim_engine")
    parser.add_argument(
        "--viewer-only",
        action="store_true",
        help="Only launch the viewer (alias for --no-capture --no-engine)",
    )
    parser.add_argument(
        "--viewer-extra",
        nargs=argparse.REMAINDER,
        default=[],
        help="Extra arguments passed directly to grim_viewer after '--'",
    )
    parser.add_argument(
        "--launch-retail",
        action="store_true",
        help="Launch tools/run_dev_install.sh before starting capture",
    )

    args = parser.parse_args()

    if args.viewer_only:
        args.no_capture = True
        args.no_engine = True

    procs: list[ManagedProcess] = []
    try:
        if args.launch_retail:
            launch_retail_game(args.release)

        if not args.no_capture:
            capture_cmd = build_capture_command(args)
            procs.append(spawn("retail_capture", capture_cmd))
            time.sleep(0.5)

        if not args.no_engine:
            engine_cmd = build_engine_command(args)
            procs.append(spawn("grim_engine", engine_cmd))
            time.sleep(0.5)

        viewer_cmd = build_viewer_command(args)
        viewer_process = spawn("grim_viewer", viewer_cmd, inherit_env=True)
        procs.append(viewer_process)
        exit_code = viewer_process.process.wait()
    except KeyboardInterrupt:
        print("[run_live_preview] interrupted; shutting down children")
        exit_code = 130
    finally:
        shutdown_processes(procs)

    sys.exit(exit_code)


@dataclass
class ManagedProcess:
    name: str
    process: subprocess.Popen


def build_capture_command(args) -> List[str]:
    command = ["cargo", "run"]
    if args.release:
        command.append("--release")
    command.extend(
        [
            "-p",
            "live_retail_capture",
            "--",
            "--stream-addr",
            args.retail_addr,
            "--width",
            str(args.width),
            "--height",
            str(args.height),
            "--fps",
            str(args.fps),
            "--ffmpeg",
            args.ffmpeg,
        ]
    )
    return command


def build_engine_command(args) -> List[str]:
    command = ["cargo", "run"]
    if args.release:
        command.append("--release")
    command.extend(["-p", "grim_engine", "--", "--run-lua", "--stream-bind", args.engine_addr])
    return command


def build_viewer_command(args) -> List[str]:
    viewer_args: List[str] = [
        "--retail-stream",
        args.retail_addr,
        "--window-width",
        str(args.width),
        "--window-height",
        str(args.height),
    ]

    if not args.no_engine:
        viewer_args.extend(["--engine-stream", args.engine_addr])

    extra = list(args.viewer_extra)
    if extra and extra[0] == "--":
        extra = extra[1:]
    viewer_args.extend(extra)

    command: List[str] = []
    if args.steam_run:
        command.append("steam-run")

    command.extend(["cargo", "run"])
    if args.release:
        command.append("--release")
    command.extend(["-p", "grim_viewer", "--", *viewer_args])
    return command


def spawn(name: str, command: Sequence[str], foreground: bool = False, inherit_env: bool = False) -> ManagedProcess:
    env = os.environ.copy() if inherit_env else None
    print(f"[run_live_preview] launching {name}: {' '.join(command)}")
    proc = subprocess.Popen(command, cwd=ROOT_DIR, env=env)
    if foreground:
        return ManagedProcess(name, proc)
    return ManagedProcess(name, proc)


def launch_retail_game(release: bool) -> None:
    env = os.environ.copy()
    command: List[str] = ["bash", "./tools/run_dev_install.sh"]
    if "DISPLAY" in env:
        env.setdefault("DISPLAY", env["DISPLAY"])
    if "WAYLAND_DISPLAY" in env:
        env.setdefault("WAYLAND_DISPLAY", env["WAYLAND_DISPLAY"])
    if release:
        env["GRIM_BUILD_PROFILE"] = "release"
    print(f"[run_live_preview] launching retail game: {' '.join(command)}")
    subprocess.Popen(command, cwd=ROOT_DIR, env=env)


def shutdown_processes(processes: Sequence[ManagedProcess]) -> None:
    for managed in processes:
        proc = managed.process
        if proc.poll() is not None:
            continue
        print(f"[run_live_preview] terminating {managed.name}")
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
            print(f"[run_live_preview] force killing {managed.name}")
            try:
                proc.kill()
            except ProcessLookupError:
                pass
if __name__ == "__main__":
    main()
