#!/usr/bin/env python3
"""Validate a commit message against the project template.

By default the script checks .git/COMMIT_EDITMSG. Pass --file to point at a
different message (for example the most recent commit via `git log -1 --pretty=%B`).
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

COMPONENT_PATTERN = re.compile(r"^[a-z][a-z0-9_]*$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--file",
        type=Path,
        default=Path(".git/COMMIT_EDITMSG"),
        help="path to the commit message to validate (default: %(default)s)",
    )
    return parser.parse_args()


def fail(message: str) -> int:
    sys.stderr.write(f"commit lint error: {message}\n")
    return 1


def lint_message(text: str) -> int:
    lines = text.splitlines()
    # Allow a trailing newline which produces an empty last entry
    if lines and lines[-1] == "":
        lines = lines[:-1]

    if not lines:
        return fail("commit message is empty")

    first_line = lines[0]
    match = re.match(r"^([a-z][a-z0-9_]*): (.+)$", first_line)
    if not match:
        return fail("first line must match '<component>: <summary>' with lowercase component")
    component, summary = match.groups()
    if not COMPONENT_PATTERN.match(component):
        return fail("component must be lowercase alphanumeric/underscore (e.g. grim_viewer, docs, tools)")
    if not summary.strip():
        return fail("summary must not be empty")

    if len(lines) < 3 or lines[1] != "":
        return fail("expected blank line after subject")

    if lines[2] != "Why:":
        return fail("expected 'Why:' section header on line 3")

    idx = 3
    why_items = []
    while idx < len(lines) and lines[idx]:
        line = lines[idx]
        if not line.startswith("- "):
            return fail("each Why bullet must start with '- ' and be contiguous")
        why_items.append(line)
        idx += 1
    if not why_items:
        return fail("Why section must contain at least one bullet")
    if idx >= len(lines) or lines[idx] != "":
        return fail("expected blank line between Why and What sections")
    idx += 1

    if idx >= len(lines) or lines[idx] != "What:":
        return fail("expected 'What:' section header after Why section")
    idx += 1

    what_items = []
    while idx < len(lines) and lines[idx]:
        line = lines[idx]
        if not line.startswith("- "):
            return fail("each What bullet must start with '- ' and be contiguous")
        what_items.append(line)
        idx += 1
    if not what_items:
        return fail("What section must contain at least one bullet")

    # Ensure no stray non-empty content after the What bullets.
    if any(line.strip() for line in lines[idx:]):
        return fail("unexpected content after What section")

    return 0


def main() -> int:
    args = parse_args()
    try:
        text = args.file.read_text(encoding="utf-8")
    except FileNotFoundError:
        return fail(f"cannot read commit message file: {args.file}")
    return lint_message(text)


if __name__ == "__main__":
    raise SystemExit(main())
