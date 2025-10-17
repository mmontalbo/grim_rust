#!/usr/bin/env python3
"""Launch retail capture, grim_engine, and grim_viewer together for live preview."""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, List, Optional, Sequence, Tuple

try:
    import termios  # type: ignore
except ImportError:  # pragma: no cover - unavailable on non-Unix platforms
    termios = None  # type: ignore[assignment]

ROOT_DIR = Path(__file__).resolve().parents[1]
LIVE_PREVIEW_LOG = Path("/tmp/live_preview.log")

VIEWER_PADDING = 24  # keep in sync with grim_viewer/src/layout.rs
VIEWER_LABEL_HEIGHT = 32  # keep in sync with grim_viewer/src/layout.rs::LABEL_HEIGHT
VIEWER_LABEL_GAP = 8  # keep in sync with grim_viewer/src/layout.rs::LABEL_GAP
DEFAULT_RETAIL_TIMEOUT_SECONDS = 20.0
VIEWER_READY_TIMEOUT_SECONDS = 20.0


def current_log_offset(path: Path) -> int:
    try:
        return path.stat().st_size
    except FileNotFoundError:
        return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--retail-addr", default="127.0.0.1:17400", help="GrimStream address for retail capture")
    parser.add_argument("--engine-addr", default="127.0.0.1:17500", help="GrimStream address for grim_engine")
    parser.add_argument("--width", type=int, default=1280, help="Captured framebuffer width")
    parser.add_argument("--height", type=int, default=720, help="Captured framebuffer height")
    parser.add_argument("--fps", type=float, default=30.0, help="Captured framerate")
    parser.add_argument("--ffmpeg", default="ffmpeg", help="ffmpeg executable for capture")
    parser.add_argument(
        "--retail-window-id",
        help="X11 window id for the retail game (hex like 0x3a00007 or decimal). "
        "When omitted we auto-detect the window by title.",
    )
    parser.add_argument(
        "--retail-window-title",
        default="Grim Fandango",
        help="Window title (substring or regex) used to auto-detect the retail window.",
    )
    parser.add_argument(
        "--retail-window-retries",
        type=int,
        default=30,
        help="How many times to poll for the retail window when auto-detecting.",
    )
    parser.add_argument(
        "--retail-window-wait",
        type=float,
        default=1.0,
        help="Seconds to wait between retail window discovery attempts.",
    )
    parser.add_argument(
        "--viewer-debug-width",
        type=int,
        default=400,
        help="Extra horizontal space reserved for debug UI inside the viewer window.",
    )
    parser.add_argument(
        "--viewer-debug-height",
        type=int,
        default=300,
        help="Extra vertical space reserved for debug UI inside the viewer window.",
    )
    parser.add_argument(
        "--retail-timeout",
        help="Override timeout passed to tools/run_dev_install.sh when using --launch-retail "
        "(examples: 60s, 5m).",
    )
    parser.add_argument(
        "--retail-no-timeout",
        action="store_true",
        help="Disable the run_dev_install timeout when launching retail via --launch-retail.",
    )

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
    if args.retail_no_timeout and args.retail_timeout:
        parser.error("--retail-no-timeout cannot be used together with --retail-timeout")

    procs: list[ManagedProcess] = []
    ready_path: Optional[Path] = None
    viewer_process: Optional[ManagedProcess] = None
    engine_process: Optional[ManagedProcess] = None
    capture_process: Optional[ManagedProcess] = None
    retail_process: Optional[ManagedProcess] = None
    exit_code: int = 0
    viewer_ready_offset = current_log_offset(LIVE_PREVIEW_LOG) if not args.no_engine else 0
    layout_ready = False
    tty_state = capture_tty_state()
    try:
        if not args.no_engine and not args.no_capture:
            ready_path = Path(tempfile.gettempdir()) / f"grim_live_ready_{os.getpid()}_{int(time.time() * 1000)}"
            if ready_path.exists():
                try:
                    ready_path.unlink()
                except OSError as err:
                    print(
                        f"[run_live_preview] warning: failed to remove stale stream ready marker {ready_path}: {err}",
                        file=sys.stderr,
                    )
            args.stream_ready_file = ready_path
        else:
            args.stream_ready_file = None

        # Seed default viewer sizing; updated later if we discover the retail window.
        args.viewer_window_width = compute_viewer_width(args.width, args.viewer_debug_width)
        args.viewer_window_height = compute_viewer_height(args.height, args.viewer_debug_height)
        args.window_id = None

        try:
            prepare_layout(args)
            layout_ready = True
        except RuntimeError as err:
            if args.launch_retail and args.retail_window_id is None:
                print(
                    "[run_live_preview] retail window not detected yet; "
                    "deferring capture layout until after the game launches"
                )
                print(f"[run_live_preview]   detail: {err}", file=sys.stderr)
            else:
                raise

        viewer_cmd = build_viewer_command(args)
        viewer_process = spawn("grim_viewer", viewer_cmd, inherit_env=True)
        procs.append(viewer_process)
        wait_for_viewer_ready(viewer_process)

        if not args.no_engine:
            engine_cmd = build_engine_command(args)
            engine_process = spawn("grim_engine", engine_cmd)
            procs.append(engine_process)
            wait_for_process_healthy(engine_process, "grim_engine", warmup_seconds=2.0)

        if not args.no_engine and not args.no_capture:
            wait_for_viewer_handshake(viewer_ready_offset, VIEWER_READY_TIMEOUT_SECONDS)

        if args.launch_retail:
            ensure_process_running(viewer_process, "grim_viewer")
            if engine_process is not None:
                ensure_process_running(engine_process, "grim_engine")
            retail_process = launch_retail_game(args)
            if retail_process:
                procs.append(retail_process)
            if not layout_ready and not args.no_capture:
                wait_for_retail_layout(args, args.retail_window_retries, args.retail_window_wait)
                layout_ready = True
                resize_viewer_window(args.viewer_window_width, args.viewer_window_height)

        if not args.no_capture:
            if not layout_ready:
                wait_for_retail_layout(args, args.retail_window_retries, args.retail_window_wait)
                layout_ready = True
                resize_viewer_window(args.viewer_window_width, args.viewer_window_height)
            capture_cmd = build_capture_command(args)
            capture_process = spawn("retail_capture", capture_cmd)
            procs.append(capture_process)

        if engine_process is None and not args.no_engine:
            raise RuntimeError("grim_engine failed to launch")
        if viewer_process is None:
            raise RuntimeError("grim_viewer failed to launch")

        session_timeout = resolve_session_timeout_seconds(args)
        watchers: List[Tuple[str, Optional[ManagedProcess]]] = [
            ("grim_engine", engine_process),
            ("retail_capture", capture_process),
            ("retail_game", retail_process),
        ]
        exit_code = wait_for_session_completion(viewer_process, watchers, session_timeout)
    except KeyboardInterrupt:
        print("[run_live_preview] interrupted; shutting down children")
        exit_code = 130
    except RuntimeError as err:
        print(f"[run_live_preview] error: {err}", file=sys.stderr)
        exit_code = 1
    finally:
        shutdown_processes(procs)
        if ready_path and ready_path.exists():
            try:
                ready_path.unlink()
            except OSError as err:
                print(
                    f"[run_live_preview] warning: failed to remove stream ready marker {ready_path}: {err}",
                    file=sys.stderr,
                )
        restore_tty_state(tty_state)

    sys.exit(exit_code)


@dataclass
class ManagedProcess:
    name: str
    process: subprocess.Popen


@dataclass
class WindowCaptureLayout:
    window_id_hex: str
    window_id_decimal: int
    capture_width: int
    capture_height: int
    viewer_width: int
    viewer_height: int


def ensure_process_running(managed: Optional[ManagedProcess], label: str) -> None:
    if managed is None:
        raise RuntimeError(f"{label} process handle is unavailable")
    code = managed.process.poll()
    if code is not None:
        raise RuntimeError(f"{label} exited early with status {code}")


def wait_for_process_healthy(
    managed: ManagedProcess,
    label: str,
    *,
    warmup_seconds: float = 1.0,
    interval: float = 0.1,
) -> None:
    deadline = time.monotonic() + max(0.0, warmup_seconds)
    interval = max(0.01, interval)
    if warmup_seconds > 0.0:
        print(
            f"[run_live_preview] waiting up to {warmup_seconds:.1f}s for {label} "
            "to report healthy"
        )
    while time.monotonic() < deadline:
        ensure_process_running(managed, label)
        time.sleep(interval)
    ensure_process_running(managed, label)
    if warmup_seconds > 0.0:
        print(f"[run_live_preview] {label} is running")


def wait_for_viewer_ready(managed: ManagedProcess) -> None:
    ensure_process_running(managed, "grim_viewer")
    try:
        print("[run_live_preview] waiting for grim_viewer window …")
        window_id = poll_for_window_id("Grim Viewer", 40, 0.25)
    except RuntimeError as err:
        if managed.process.poll() is not None:
            raise RuntimeError("grim_viewer exited before creating a window") from err
        print(
            f"[run_live_preview] warning: viewer window detection skipped ({err})",
            file=sys.stderr,
        )
        wait_for_process_healthy(managed, "grim_viewer", warmup_seconds=1.0)
    else:
        print(f"[run_live_preview] viewer window ready ({window_id})")
        wait_for_process_healthy(managed, "grim_viewer", warmup_seconds=0.5)


def wait_for_viewer_handshake(offset: int, timeout: float) -> None:
    deadline = time.monotonic() + max(0.1, timeout)
    offset = max(0, offset)
    print("[run_live_preview] waiting for viewer-ready handshake …")
    while time.monotonic() < deadline:
        try:
            with LIVE_PREVIEW_LOG.open("rb") as handle:
                handle.seek(offset)
                data = handle.read()
                offset = handle.tell()
                if b"viewer_ready.open" in data:
                    print("[run_live_preview] viewer handshake acknowledged")
                    return
        except FileNotFoundError:
            pass
        time.sleep(0.2)
    raise RuntimeError("viewer handshake timed out")


def wait_for_retail_layout(args: argparse.Namespace, retries: int, wait_seconds: float) -> None:
    retries = max(1, retries)
    wait_seconds = max(0.1, wait_seconds)
    last_error: Optional[RuntimeError] = None
    for attempt in range(1, retries + 1):
        print(
            f"[run_live_preview] waiting for retail window "
            f"(attempt {attempt}/{retries}, delay {wait_seconds:.1f}s)"
        )
        try:
            prepare_layout(args)
        except RuntimeError as err:
            last_error = err
            if attempt < retries:
                time.sleep(wait_seconds)
                continue
        else:
            print(
                "[run_live_preview] retail window detected "
                f"(id {args.window_id}, size {args.width}x{args.height})"
            )
            return
    message = "retail window detection timed out"
    if last_error:
        message += f" ({last_error})"
    raise RuntimeError(message)


def resize_viewer_window(width: int, height: int) -> None:
    try:
        window_id = poll_for_window_id("Grim Viewer", 10, 0.2)
    except RuntimeError as err:
        print(
            f"[run_live_preview] warning: unable to resize viewer window ({err})",
            file=sys.stderr,
        )
        return

    try:
        subprocess.run(
            ["xdotool", "windowsize", window_id, str(width), str(height)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        print(
            f"[run_live_preview] resized viewer window {window_id} to {width}x{height}"
        )
    except FileNotFoundError:
        print(
            "[run_live_preview] warning: xdotool not available; viewer window size unchanged",
            file=sys.stderr,
        )
    except subprocess.CalledProcessError as err:
        print(
            f"[run_live_preview] warning: failed to resize viewer window {window_id}: {err}",
            file=sys.stderr,
        )


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
    window_id = getattr(args, "window_id", None)
    if window_id:
        command.extend(["--window-id", window_id])
    stream_ready = getattr(args, "stream_ready_file", None)
    if stream_ready:
        command.extend(["--ready-notify", str(stream_ready)])
    return command


def build_engine_command(args) -> List[str]:
    command = ["cargo", "run"]
    if args.release:
        command.append("--release")
    command.extend(["-p", "grim_engine", "--", "--stream-bind", args.engine_addr])
    stream_ready = getattr(args, "stream_ready_file", None)
    if stream_ready:
        command.extend(["--stream-ready-file", str(stream_ready)])
    return command


def build_viewer_command(args) -> List[str]:
    viewer_width = getattr(args, "viewer_window_width", args.width)
    viewer_height = getattr(args, "viewer_window_height", args.height)
    viewer_args: List[str] = [
        "--retail-stream",
        args.retail_addr,
        "--window-width",
        str(viewer_width),
        "--window-height",
        str(viewer_height),
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


def prepare_layout(args: argparse.Namespace) -> None:
    capture_layout: Optional[WindowCaptureLayout] = None
    require_window = not args.no_capture or args.retail_window_id is not None

    if require_window:
        capture_layout = discover_window_layout(args)

    if capture_layout:
        args.width = capture_layout.capture_width
        args.height = capture_layout.capture_height
        args.window_id = capture_layout.window_id_hex
        args.viewer_window_width = capture_layout.viewer_width
        args.viewer_window_height = capture_layout.viewer_height
        print(
            "[run_live_preview] configured retail capture window "
            f"{capture_layout.window_id_hex} (decimal {capture_layout.window_id_decimal}) "
            f"size {capture_layout.capture_width}x{capture_layout.capture_height}"
        )
        print(
            "[run_live_preview] viewer window sized to "
            f"{capture_layout.viewer_width}x{capture_layout.viewer_height} "
            "(room for retail + engine viewports and debug UI)"
        )
    else:
        args.window_id = None
        args.viewer_window_width = compute_viewer_width(args.width, args.viewer_debug_width)
        args.viewer_window_height = compute_viewer_height(args.height, args.viewer_debug_height)
        print(
            "[run_live_preview] retail window detection skipped; "
            f"using viewer size {args.viewer_window_width}x{args.viewer_window_height}"
        )


def discover_window_layout(args: argparse.Namespace) -> WindowCaptureLayout:
    raw_id = args.retail_window_id
    if raw_id:
        print(f"[run_live_preview] using provided retail window id {raw_id}")
    else:
        raw_id = poll_for_window_id(args.retail_window_title, args.retail_window_retries, args.retail_window_wait)

    window_id_hex, window_id_decimal = normalize_window_id(raw_id)
    width, height = query_window_geometry(raw_id)
    if width <= 0 or height <= 0:
        raise RuntimeError(f"window {window_id_hex} reported invalid size {width}x{height}")

    viewer_width = compute_viewer_width(width, args.viewer_debug_width)
    viewer_height = compute_viewer_height(height, args.viewer_debug_height)

    return WindowCaptureLayout(
        window_id_hex=window_id_hex,
        window_id_decimal=window_id_decimal,
        capture_width=width,
        capture_height=height,
        viewer_width=viewer_width,
        viewer_height=viewer_height,
    )


def compute_viewer_width(base_width: int, debug_width: int) -> int:
    base_width = max(1, base_width)
    debug_width = max(0, debug_width)
    return base_width * 2 + debug_width + VIEWER_PADDING * 3


def compute_viewer_height(base_height: int, debug_height: int) -> int:
    base_height = max(1, base_height)
    debug_height = max(0, debug_height)
    top_offset = VIEWER_PADDING + VIEWER_LABEL_HEIGHT + VIEWER_LABEL_GAP
    bottom_offset = VIEWER_PADDING + debug_height
    return base_height + top_offset + bottom_offset


def poll_for_window_id(title: str, retries: int, wait_seconds: float) -> str:
    retries = max(1, retries)
    wait_seconds = max(0.0, wait_seconds)
    search_cmd = ["xdotool", "search", "--name", title]
    last_error: Optional[subprocess.CalledProcessError] = None

    for attempt in range(1, retries + 1):
        try:
            proc = subprocess.run(
                search_cmd,
                check=True,
                capture_output=True,
                text=True,
            )
        except FileNotFoundError as err:
            raise RuntimeError(
                "xdotool not found on PATH; ensure xdotool is installed (add pkgs.xdotool to shell.nix)"
            ) from err
        except subprocess.CalledProcessError as err:
            last_error = err
        else:
            window_ids = [line.strip() for line in proc.stdout.splitlines() if line.strip()]
            if window_ids:
                if len(window_ids) > 1:
                    print(
                        "[run_live_preview] multiple windows matched title pattern; "
                        f"using the most recent (attempt {attempt}/{retries})"
                    )
                return window_ids[-1]

        if attempt < retries:
            time.sleep(wait_seconds)

    message = f"failed to locate a window matching title '{title}' after {retries} attempts"
    if last_error:
        message += f" (last error code {last_error.returncode})"
    raise RuntimeError(message)


def query_window_geometry(window_id: str) -> tuple[int, int]:
    try:
        proc = subprocess.run(
            ["xwininfo", "-id", window_id],
            check=True,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError as err:
        raise RuntimeError(
            "xwininfo not found on PATH; ensure xorg.xwininfo is installed (see shell.nix)"
        ) from err
    except subprocess.CalledProcessError as err:
        raise RuntimeError(f"xwininfo failed for window {window_id}: {err}") from err

    width = height = None
    for line in proc.stdout.splitlines():
        line = line.strip()
        if line.startswith("Width:"):
            width = parse_int_from_line(line)
        elif line.startswith("Height:"):
            height = parse_int_from_line(line)
        if width is not None and height is not None:
            break

    if width is None or height is None:
        raise RuntimeError(f"could not parse window dimensions from xwininfo output for window {window_id}")
    return width, height


def parse_int_from_line(line: str) -> int:
    _, _, value = line.partition(":")
    try:
        return int(value.strip())
    except ValueError as err:
        raise RuntimeError(f"unexpected numeric value in xwininfo output line '{line}'") from err


def normalize_window_id(raw_id: str) -> tuple[str, int]:
    try:
        value = int(raw_id, 0)
    except ValueError as err:
        raise RuntimeError(f"invalid window id '{raw_id}' (expected hex like 0x3a00007 or decimal)") from err
    return f"0x{value:x}", value


def resolve_session_timeout_seconds(args: argparse.Namespace) -> Optional[float]:
    if not args.launch_retail or args.retail_no_timeout:
        return None
    if args.retail_timeout:
        try:
            return parse_timeout_to_seconds(args.retail_timeout)
        except ValueError as err:
            raise RuntimeError(f"invalid --retail-timeout value '{args.retail_timeout}': {err}") from err
    return DEFAULT_RETAIL_TIMEOUT_SECONDS


def parse_timeout_to_seconds(value: str) -> float:
    stripped = value.strip().lower()
    if not stripped:
        raise ValueError("timeout string is empty")
    unit_factor = 1.0
    if stripped[-1].isalpha():
        suffix = stripped[-1]
        number_text = stripped[:-1]
        unit_map = {"s": 1.0, "m": 60.0, "h": 3600.0}
        if suffix not in unit_map:
            raise ValueError(f"unsupported unit '{suffix}' (expected s, m, or h)")
        unit_factor = unit_map[suffix]
    else:
        number_text = stripped
    try:
        magnitude = float(number_text)
    except ValueError as err:
        raise ValueError("failed to parse numeric portion") from err
    if magnitude < 0:
        raise ValueError("timeout must be non-negative")
    return magnitude * unit_factor


def wait_for_session_completion(
    viewer: ManagedProcess,
    watchers: Sequence[Tuple[str, Optional[ManagedProcess]]],
    timeout_seconds: Optional[float],
) -> int:
    deadline = time.monotonic() + timeout_seconds if timeout_seconds is not None else None
    poll_interval = 0.25
    while True:
        viewer_code = viewer.process.poll()
        if viewer_code is not None:
            return viewer_code
        for label, managed in watchers:
            if managed is None:
                continue
            code = managed.process.poll()
            if code is not None:
                print(
                    f"[run_live_preview] {label} exited with status {code}; terminating viewer session"
                )
                return code
        if deadline is not None and time.monotonic() >= deadline:
            print(
                f"[run_live_preview] session timed out after {timeout_seconds:.1f}s; terminating viewer session"
            )
            return 124
        time.sleep(poll_interval)


def capture_tty_state() -> Optional[Any]:
    if termios is None:
        return None
    if not sys.stdin.isatty():
        return None
    try:
        return termios.tcgetattr(sys.stdin.fileno())
    except termios.error:
        return None


def restore_tty_state(state: Optional[Any]) -> None:
    if not sys.stdin.isatty():
        return
    if termios is None:
        subprocess.run(["stty", "sane"], check=False)
        return
    if state is None:
        subprocess.run(["stty", "sane"], check=False)
        return
    try:
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSADRAIN, state)
    except termios.error:
        subprocess.run(["stty", "sane"], check=False)


def spawn(name: str, command: Sequence[str], foreground: bool = False, inherit_env: bool = False) -> ManagedProcess:
    env = os.environ.copy() if inherit_env else None
    print(f"[run_live_preview] launching {name}: {' '.join(command)}")
    proc = subprocess.Popen(command, cwd=ROOT_DIR, env=env)
    if foreground:
        return ManagedProcess(name, proc)
    return ManagedProcess(name, proc)


def launch_retail_game(args: argparse.Namespace) -> Optional[ManagedProcess]:
    env = os.environ.copy()
    command: List[str] = ["bash", "./tools/run_dev_install.sh"]
    if "DISPLAY" in env:
        env.setdefault("DISPLAY", env["DISPLAY"])
    if "WAYLAND_DISPLAY" in env:
        env.setdefault("WAYLAND_DISPLAY", env["WAYLAND_DISPLAY"])
    if args.retail_no_timeout:
        command.append("--no-timeout")
    elif args.retail_timeout:
        command.extend(["--timeout", args.retail_timeout])
    if args.release:
        env["GRIM_BUILD_PROFILE"] = "release"
    print(f"[run_live_preview] launching retail game: {' '.join(command)}")
    proc = subprocess.Popen(command, cwd=ROOT_DIR, env=env)
    return ManagedProcess("retail_game", proc)


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


def current_log_offset(path: Path) -> int:
    try:
        return path.stat().st_size
    except FileNotFoundError:
        return 0
