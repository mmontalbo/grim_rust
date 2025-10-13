#!/usr/bin/env python3
"""Format a commit message that follows the project template.

Usage:
    python tools/format_commit.py grim_viewer "short summary" \
        --why "reason one" --why "reason two" \
        --what "path/file: change description" [...]

Use --write <path> to write the formatted commit message to a file (for example
Git's .git/COMMIT_EDITMSG). Without --write, the formatted message is printed
to stdout.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

COMPONENT_PATTERN = re.compile(r"^[a-z][a-z0-9_]*$")


def validate_component(value: str) -> str:
    if not COMPONENT_PATTERN.match(value):
        raise argparse.ArgumentTypeError(
            "component must be lowercase alphanumeric/underscore (e.g. grim_viewer, docs, tools)"
        )
    return value


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("component", type=validate_component, help="component name (crate, docs, or tools)")
    parser.add_argument("summary", help="short change summary")
    parser.add_argument(
        "--why",
        action="append",
        required=True,
        metavar="TEXT",
        help="reason for the change (repeatable)",
    )
    parser.add_argument(
        "--what",
        action="append",
        required=True,
        metavar="TEXT",
        help="files/edits included in the change (repeatable)",
    )
    parser.add_argument(
        "--write",
        type=Path,
        metavar="PATH",
        help="write the formatted message to PATH instead of stdout",
    )
    return parser.parse_args()


def format_message(component: str, summary: str, why: list[str], what: list[str]) -> str:
    lines = [f"{component}: {summary}", "", "Why:"]
    lines.extend(f"- {item}" for item in why)
    lines.append("")
    lines.append("What:")
    lines.extend(f"- {item}" for item in what)
    return "\n".join(lines) + "\n"


def main() -> int:
    args = parse_args()
    message = format_message(args.component, args.summary, args.why, args.what)
    if args.write:
        args.write.write_text(message, encoding="utf-8")
    else:
        sys.stdout.write(message)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
