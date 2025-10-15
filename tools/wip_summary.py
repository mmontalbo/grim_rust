#!/usr/bin/env python3
"""Render the current WIP plan from embedded data."""

import argparse
import json
import textwrap
from typing import Any, Dict, List

WIP_DATA: Dict[str, Any] = {
    "title": "Milestone 1 Push",
    "sections": [
        {
            "title": "Milestone Goal",
            "body": [
                "Deliver a first-playable Manny office where the viewer renders initial Manny's office scene.",
                "User should be able to provide input to the viewer and have game state update in the same exact way the real game does",
                "Every task should accelerate implementing the initial office gameplay, before thinking about work keep this ultimate goal in mind",
            ],
        },
        {
            "title": "Execution Notes",
            "body": [
                "We are trying to re-create game logic in rust, prefer to fail fast rather than providing fallback for behavior that does not match the real game",
                "If making progress on a work item is slow or uncertain, evaluate if there are opportunites to simplify or clarify the related components",
                "When committing, use python tools/format_commit.py to generate the message and python tools/lint_commit.py to validate it before pushing.",
            ],
        },
    ],
}


def format_section(title: str, lines: List[str]) -> str:
    header = f"## {title}"
    body: List[str] = []
    for entry in lines:
        body.append(
            textwrap.fill(
                entry,
                width=88,
                initial_indent="- ",
                subsequent_indent="  ",
            )
        )
    return "\n".join([header, *body])


def summarise(data: Dict[str, Any]) -> str:
    parts: List[str] = [data.get("title", "Work in Progress")]
    for section in data.get("sections", []):
        raw_title = section.get("title", "Section")
        title = raw_title.strip()
        body = section.get("body", [])
        if not body:
            continue
        parts.append("")
        parts.append(format_section(title, body))
    return "\n".join(parts)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="dump the raw WIP data as JSON")
    args = parser.parse_args()

    if args.json:
        print(json.dumps(WIP_DATA, indent=2, sort_keys=True))
        return

    print(summarise(WIP_DATA))


if __name__ == "__main__":
    main()
