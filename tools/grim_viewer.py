#!/usr/bin/env python3
"""Helper to run grim_viewer interactively or headless.

This script revives the spirit of the retired `grim_mod` launcher so automated
processes (or remote teammates) can spin up the viewer without juggling cargo
arguments manually.
"""

from __future__ import annotations

import argparse
import os
import platform
import subprocess
import sys
from pathlib import Path
from typing import Iterable, List, Sequence
import shutil

ROOT_DIR = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT_DIR / "artifacts" / "manny_office_assets.json"
DEFAULT_ASSET = "mo_0_ddtws.bm"
DEFAULT_TIMELINE = ROOT_DIR / "tools" / "tests" / "manny_office_timeline.json"
DEFAULT_MOVEMENT_LOG = ROOT_DIR / "tools" / "tests" / "movement_log.json"
DEFAULT_EVENT_LOG = ROOT_DIR / "tools" / "tests" / "hotspot_events.json"
DEFAULT_LAYOUT_PRESET = ROOT_DIR / "grim_viewer" / "presets" / "manny_office_layout.json"
DEFAULT_GEOMETRY_SNAPSHOT = ROOT_DIR / "artifacts" / "run_cache" / "manny_geometry.json"


def main() -> None:
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument(
        "--manifest",
        default=str(DEFAULT_MANIFEST),
        help="Asset manifest JSON to load (default: artifacts/manny_office_assets.json)",
    )
    common.add_argument(
        "--asset",
        default=DEFAULT_ASSET,
        help="Bitmap asset to load (default: mo_0_ddtws.bm)",
    )
    common.add_argument(
        "--timeline",
        default=str(DEFAULT_TIMELINE),
        help="Boot timeline manifest to enumerate entities (pass 'none' to disable)",
    )
    common.add_argument(
        "--movement-log",
        default=str(DEFAULT_MOVEMENT_LOG),
        help="Movement log JSON to overlay (pass 'none' to disable)",
    )
    common.add_argument(
        "--event-log",
        default=str(DEFAULT_EVENT_LOG),
        help="Hotspot event log JSON to overlay (pass 'none' to disable)",
    )
    common.add_argument(
        "--lua-geometry-json",
        default=None,
        help=(
            "Lua geometry snapshot JSON to align Manny/desk/tube markers "
            "(default: autodetect artifacts/run_cache/manny_geometry.json)"
        ),
    )
    common.add_argument(
        "--dump-frame",
        default=None,
        help="Optional PNG path to save the decoded bitmap",
    )
    common.add_argument(
        "--release",
        action="store_true",
        help="Use the release profile instead of debug",
    )
    common.add_argument(
        "--use-binary",
        action="store_true",
        help="Run the pre-built binary in target/<profile> instead of cargo run",
    )
    common.add_argument(
        "--steam-run",
        action="store_true",
        help="Wrap the launch in steam-run to borrow Steam's GL/Vulkan runtime",
    )
    common.add_argument(
        "--env",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="Extra environment variables (may be repeated)",
    )

    parser = argparse.ArgumentParser(description=__doc__, parents=[common])
    parser.add_argument(
        "viewer_args",
        nargs=argparse.REMAINDER,
        help="Additional viewer CLI flags after '--'",
    )

    args = parser.parse_args()

    extra = normalize_tail(args.viewer_args)
    viewer_cmd = build_run_args(args, extra)

    exit_code = exec_viewer(args, viewer_cmd)
    sys.exit(exit_code)


def build_run_args(args, extra: Sequence[str]) -> List[str]:
    viewer_args = [
        "--manifest",
        args.manifest,
        "--asset",
        args.asset,
    ]
    timeline = normalize_optional_path(args.timeline)
    movement = normalize_optional_path(args.movement_log)
    event_log = normalize_optional_path(args.event_log)
    geometry = normalize_optional_path(args.lua_geometry_json)
    geometry_specified = args.lua_geometry_json is not None

    if timeline:
        viewer_args.extend(["--timeline", timeline])
    if args.dump_frame:
        viewer_args.extend(["--dump-frame", args.dump_frame])
    if movement:
        viewer_args.extend(["--movement-log", movement])
    if event_log:
        viewer_args.extend(["--event-log", event_log])
    ensure_layout_preset(viewer_args, extra)
    ensure_geometry_snapshot(viewer_args, extra, geometry, geometry_specified)
    viewer_args.extend(extra)
    return viewer_args


def exec_viewer(args, viewer_args: Sequence[str]) -> int:
    env = os.environ.copy()
    for entry in args.env:
        key, value = parse_env(entry)
        env[key] = value

    tmpdir = env.get("TMPDIR")
    if tmpdir and not Path(tmpdir).exists():
        fallback = "/tmp"
        print(f"[grim_viewer] TMPDIR '{tmpdir}' missing; using {fallback} instead")
        env["TMPDIR"] = fallback

    command: List[str]
    if args.use_binary:
        binary = resolve_binary(args.release)
        command = [str(binary)]
    else:
        command = ["cargo", "run"]
        if args.release:
            command.append("--release")
        command.extend(["-p", "grim_viewer", "--"])
    command.extend(viewer_args)

    if args.steam_run:
        if shutil.which("steam-run") is None:
            raise RuntimeError("steam-run requested but not found on PATH")
        command = ["steam-run", *command]

    print(f"[grim_viewer] launching: {' '.join(command)}")
    completed = subprocess.run(command, env=env)
    return completed.returncode


def ensure_layout_preset(viewer_args: List[str], extra: Sequence[str]) -> None:
    if has_layout_flag(viewer_args) or has_layout_flag(extra):
        return
    if DEFAULT_LAYOUT_PRESET.exists():
        viewer_args.extend(["--layout-preset", str(DEFAULT_LAYOUT_PRESET)])


def has_layout_flag(args: Sequence[str]) -> bool:
    for entry in args:
        if entry == "--layout-preset" or entry.startswith("--layout-preset="):
            return True
    return False


def ensure_geometry_snapshot(
    viewer_args: List[str],
    extra: Sequence[str],
    user_path: str | None,
    user_specified: bool,
) -> None:
    if has_geometry_flag(viewer_args) or has_geometry_flag(extra):
        return
    if user_path:
        viewer_args.extend(["--lua-geometry-json", user_path])
        return
    if user_specified:
        return
    if DEFAULT_GEOMETRY_SNAPSHOT.exists():
        viewer_args.extend(["--lua-geometry-json", str(DEFAULT_GEOMETRY_SNAPSHOT)])


def has_geometry_flag(args: Sequence[str]) -> bool:
    for entry in args:
        if entry == "--lua-geometry-json" or entry.startswith("--lua-geometry-json="):
            return True
    return False


def normalize_tail(tail: Iterable[str] | None) -> List[str]:
    if not tail:
        return []
    tail = list(tail)
    if tail and tail[0] == "--":
        return tail[1:]
    return tail


def normalize_optional_path(value: str | None) -> str | None:
    if value is None:
        return None
    trimmed = value.strip()
    if not trimmed or trimmed.lower() in {"none", "off", "disable"}:
        return None
    return trimmed


def parse_env(entry: str) -> tuple[str, str]:
    if "=" not in entry:
        raise ValueError(f"Environment override must be KEY=VALUE (got: {entry})")
    key, value = entry.split("=", 1)
    return key, value


def resolve_binary(release: bool) -> Path:
    suffix = ".exe" if platform.system().lower().startswith("win") else ""
    profile = "release" if release else "debug"
    binary = ROOT_DIR / "target" / profile / f"grim_viewer{suffix}"
    if not binary.exists():
        raise FileNotFoundError(
            f"Built binary not found at {binary}. Run 'cargo build -p grim_viewer{' --release' if release else ''}' first."
        )
    return binary


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # pragma: no cover - CLI surfacing
        print(f"[grim_viewer] ERROR: {exc}", file=sys.stderr)
        sys.exit(1)
