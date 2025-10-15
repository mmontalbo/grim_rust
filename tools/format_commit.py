#!/usr/bin/env python3
"""Format a commit message that follows the project template.

Usage:
    python tools/format_commit.py grim_viewer "short summary" \
        --why "reason one" --why "reason two" \
        --what "path/file: change description" [...]

Use --write <path> to write the formatted commit message to a file (for example
Git's .git/COMMIT_EDITMSG). Without --write, the formatted message is printed
to stdout. Pass --commit to invoke `git commit` with the formatted message,
and repeat --commit-arg to forward custom arguments (for example --commit-arg
--amend).
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path


def _load_workspace_members() -> set[str]:
    root = Path(__file__).resolve().parent.parent
    cargo_toml = root / "Cargo.toml"
    try:
        cargo_text = cargo_toml.read_text(encoding="utf-8")
    except OSError:
        return set()

    members_block = re.search(r"members\s*=\s*\[(?P<body>[^]]*)\]", cargo_text, re.S)
    if not members_block:
        return set()

    members: set[str] = set()
    for line in members_block.group("body").splitlines():
        entry = line.strip().rstrip(",").strip()
        if not entry or entry.startswith("#"):
            continue
        if entry.startswith('"') and entry.endswith('"'):
            members.add(entry[1:-1])
    return members


ALLOWED_COMPONENTS = _load_workspace_members().union({"tools", "docs"})


def validate_component(value: str) -> str:
    if value not in ALLOWED_COMPONENTS:
        allowed = ", ".join(sorted(ALLOWED_COMPONENTS))
        raise argparse.ArgumentTypeError(
            f"component must be one of: {allowed} (got {value!r})"
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
    parser.add_argument(
        "--commit",
        action="store_true",
        help="run `git commit` with the formatted message",
    )
    parser.add_argument(
        "--commit-arg",
        action="append",
        default=[],
        metavar="ARG",
        help="additional argument to pass to `git commit` (repeatable)",
    )
    return parser.parse_args()


def format_message(component: str, summary: str, why: list[str], what: list[str]) -> str:
    root = Path(__file__).resolve().parent.parent

    summary = summary.strip()
    if summary and summary[0].isalpha():
        summary = summary[0].lower() + summary[1:]

    if not why:
        raise ValueError("at least one --why entry is required")
    for item in why:
        if not item.strip():
            raise ValueError("--why entries must be non-empty")

    if not what:
        raise ValueError("at least one --what entry is required")
    for item in what:
        if ":" not in item:
            raise ValueError(f"--what entry must include 'path: description' (got {item!r})")
        path_label, description = item.split(":", 1)
        path = path_label.strip()
        if not path:
            raise ValueError(f"--what entry missing path before colon (got {item!r})")
        if not description.strip():
            raise ValueError(f"--what entry missing description after colon (got {item!r})")
        file_path = (root / path).resolve()
        try:
            file_path.relative_to(root)
        except ValueError as exc:
            raise ValueError(f"--what path must be within repo: {path}") from exc
        if not file_path.exists():
            raise ValueError(f"--what path does not exist: {path}")

    lines = [f"{component}: {summary}", "", "Why:"]
    lines.extend(f"- {item}" for item in why)
    lines.append("")
    lines.append("What:")
    lines.extend(f"- {item}" for item in what)
    return "\n".join(lines) + "\n"


def run_git_commit(message: str, extra_args: list[str]) -> int:
    with tempfile.NamedTemporaryFile("w", delete=False, encoding="utf-8") as handle:
        handle.write(message)
        temp_path = Path(handle.name)
    try:
        result = subprocess.run(
            ["git", "commit", "-F", str(temp_path), *extra_args],
            check=False,
        )
        if result.returncode != 0:
            sys.stderr.write(
                f"`git commit` failed with exit code {result.returncode}\n"
            )
        return result.returncode
    finally:
        try:
            temp_path.unlink()
        except OSError:
            pass


def main() -> int:
    args = parse_args()
    message = format_message(args.component, args.summary, args.why, args.what)
    if args.write:
        args.write.write_text(message, encoding="utf-8")
    else:
        sys.stdout.write(message)
    if args.commit:
        return run_git_commit(message, args.commit_arg)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
