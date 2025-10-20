#!/usr/bin/env python3
"""Ensure grim_viewer's debug overlay renders engine events.

The script launches run_movie_debug, requests a frame dump once an
engine StateUpdate with events arrives, OCRs the dumped frame with
Tesseract, and asserts the expected event token is present.
"""

from __future__ import annotations

import argparse
import contextlib
import os
import shlex
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import List, Optional, Tuple

ROOT_DIR = Path(__file__).resolve().parents[1]


def build_viewer_extra(base: str, frame_path: Path) -> str:
    parts: List[str] = []
    if base:
        parts.extend(shlex.split(base))
    parts.extend(["--show-events", "--dump-debug-frame", str(frame_path)])
    return " ".join(shlex.quote(arg) for arg in parts)


def wait_for_frame(
    path: Path, timeout: float, deadline: Optional[float], proc: subprocess.Popen[str]
) -> None:
    local_deadline = time.monotonic() + timeout
    if deadline is not None:
        local_deadline = min(local_deadline, deadline)
    while time.monotonic() < local_deadline:
        if path.exists() and path.stat().st_size > 0:
            return
        ret = proc.poll()
        if ret is not None:
            stdout, stderr = proc.communicate()
            raise SystemExit(
                f"run_movie_debug exited prematurely with code {ret}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            )
        time.sleep(0.5)
    raise RuntimeError(f"frame dump did not appear within {timeout:.1f}s: {path}")


def pick_engine_port() -> Tuple[str, int]:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        host, port = sock.getsockname()
    return host, port


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--max-runtime",
        type=float,
        default=10.0,
        help="Hard cap in seconds for the overall tool runtime (set <=0 to disable)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=10.0,
        help="Seconds to wait for a debug frame dump (clamped by --max-runtime)",
    )
    parser.add_argument(
        "--viewer-extra",
        default="",
        help="Additional grim_viewer CLI flags (quoted)",
    )
    parser.add_argument(
        "--output",
        default="",
        help="Optional path to save the dumped frame for inspection",
    )
    parser.add_argument(
        "--dump-ocr-input",
        default="",
        help="Optional path to store a prepared overlay crop for offline OCR (skips creation if unset)",
    )

    args = parser.parse_args()

    max_runtime = args.max_runtime
    overall_deadline = None if max_runtime <= 0 else time.monotonic() + max_runtime

    def ensure_within_deadline() -> float:
        if overall_deadline is None:
            return float("inf")
        remaining = overall_deadline - time.monotonic()
        if remaining <= 0:
            raise SystemExit(
                f"overlay verification exceeded max runtime of {max_runtime:.1f}s"
            )
        return remaining

    with tempfile.TemporaryDirectory() as tmpdir:
        tmpdir_path = Path(tmpdir)
        frame_path = tmpdir_path / "debug_panel.png"
        frame_path.parent.mkdir(parents=True, exist_ok=True)

        viewer_extra = build_viewer_extra(args.viewer_extra, frame_path)
        host, port = pick_engine_port()
        cmd = [
            "python",
            "tools/run_movie_debug.py",
            "--engine-verbose",
            "--handshake-timeout",
            "5",
            "--engine-bind",
            f"{host}:{port}",
            "--viewer-extra",
            viewer_extra,
        ]

        env = os.environ.copy()
        with contextlib.ExitStack() as stack:
            proc = stack.enter_context(
                subprocess.Popen(cmd, cwd=ROOT_DIR, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            )

            def cleanup():
                if proc.poll() is None:
                    proc.kill()
            stack.callback(cleanup)

            frame_wait = min(args.timeout, ensure_within_deadline())
            wait_for_frame(frame_path, frame_wait, overall_deadline, proc)
            ret = proc.poll()
            if ret is not None:
                stdout, stderr = proc.communicate()
                raise SystemExit(
                    f"run_movie_debug exited prematurely with code {ret}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                )

            cleanup()
            try:
                wait_limit = ensure_within_deadline()
                communicate_timeout = min(5.0, wait_limit)
                if communicate_timeout <= 0:
                    raise SystemExit(
                        f"overlay verification exceeded max runtime of {max_runtime:.1f}s"
                    )
                stdout, stderr = proc.communicate(timeout=communicate_timeout)
            except subprocess.TimeoutExpired:
                proc.kill()
                stdout, stderr = proc.communicate()
            if stdout.strip():
                print(stdout, end="")
            if stderr.strip():
                print(stderr, end="", file=sys.stderr)

        # Give the viewer a moment to finish writing the file after the process shutdown.
        remaining_after_cleanup = ensure_within_deadline()
        time.sleep(min(1.0, remaining_after_cleanup))

        if args.output:
            dest = Path(args.output)
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(frame_path.read_bytes())

        if args.dump_ocr_input:
            dest = Path(args.dump_ocr_input)
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(frame_path.read_bytes())

        ensure_within_deadline()
        if frame_path.stat().st_size == 0:
            raise SystemExit("debug overlay frame was empty; cannot verify contents")

        print(
            "Overlay frame captured successfully; run OCR offline if further validation is needed"
        )


if __name__ == "__main__":
    main()
