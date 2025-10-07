#!/usr/bin/env python3
"""Render the current WIP plan from embedded data."""

import argparse
import json
import textwrap
from typing import Any, Dict, List

WIP_DATA: Dict[str, Any] = {
    "title": "Work in Progress",
    "sections": [
        {
            "title": "Milestone Priority",
            "body": [
                "Milestone 1: Enable gameplay for the initial Manny office scene (load, navigate, interact).",
                "All active work should unblock first-playable experience before tackling broader engine polish.",
            ],
        },
        {
            "title": "Getting Started",
            "body": [
                "Familiarise yourself with README.md and the Current Focus items, then drive the next task to a committable state (tests/docs updated, WIP refreshed, commit created).",
                'Treat commits as the definition of "done" for a work cycle—merge-ready changes should be committed before you step away.',
                "Regenerate this summary with python tools/wip_summary.py whenever priorities shift so the team stays aligned.",
                "Use python tools/grim_viewer.py to load the Manny baseline overlays (add -- --headless on machines without a windowing environment).",
                "The Manny office regression artefacts we reference live under tools/tests/ (movement_log.json, hotspot_events.json, etc.); skim them the first time so you know what the viewer overlays are visualising.",
                "Run cargo test -p grim_engine -- runtime_regression after asset or script changes that touch Manny's office to confirm the harness stays green.",
            ],
        },
        {
            "title": "Current Focus",
            "body": [
                "Keep the Manny office regression fixtures (movement_log.json, hotspot_events.json, manny_office_depth_stats.json) in sync with intent; rerun the runtime harness whenever gameplay changes affect them.",
                "Verify Manny’s office loads and plays through (walk path + computer interaction) with modular viewer overlays, updating geometry snapshots or assets if markers drift.",
                "Only regenerate artifacts/run_cache/manny_geometry.json when Manny’s markers drift or scripts move set anchors; otherwise note the check-in that validated keeping the existing snapshot.",
                "Feed the latest Lua geometry snapshot through the scene builder so Manny/desk/tube anchors remain aligned in both runtime and viewer contexts.",
                "Document any workflow quirks in docs/grim_viewer_modules.md so the path to first-playable remains clear for Milestone 1.",
                "Flag the recent viewer/scene module split when sharing context so contributors land changes in the right files.",
            ],
        },
        {
            "title": "Commit Conventions",
            "body": [
                "Format commits as: <area>: <short change summary> on the first line, then blank line, followed by 'Why:' and 'What:' bullet blocks summarising intent and implementation.",
                "Keep the bullet phrasing tight (hyphen bullets preferred) so reviewers see the rationale/changes without hunting through diffs.",
                "Aim to keep each logical change under roughly 500 lines so reviews stay manageable; call out regenerated assets or automation when the diff must exceed that threshold.",
                "Avoid blank lines between Why/What bullet entries so the commit template stays compact and scannable.",
                "Example (with template configured): git commit -m 'grim_viewer: refresh overlay layout' -m $'Why:\\n- unblock minimap verification' -m $'What:\\n- grim_viewer/src/ui_layout.rs: add Taffy helper'",
                "Use top-level directories for the <area> prefix (grim_engine, grim_viewer, grim_formats, grim_analysis, tools, docs, etc.) instead of generic labels like runtime; split work across commits when multiple components need separate context.",
                "List one 'What' bullet per file touched using <path>: <brief change> so reviewers can map intent to diffs quickly.",
                "Re-read the formatted commit after it lands; amend immediately if the prefix, Why/What blocks, or content drift from the guidelines.",
            ],
        },
    ],
    "workstreams": [
        {
            "slug": "manny-geometry-refresh",
            "title": "Manny geometry + regression refresh",
            "description": "Verify Manny’s office overlays stay locked to the baseline and refresh artefacts when they drift.",
            "prompt": (
                "Run python tools/grim_viewer.py -- --headless to check Manny/desk/tube alignment; "
                "if markers drift, follow docs/runtime_smoke_tests.md to regenerate movement, hotspot, "
                "audio, depth, and geometry artefacts together; rerun cargo test -p grim_engine -- "
                "runtime_regression to confirm the refreshed baselines stay green; update "
                "docs/grim_viewer_modules.md with any workflow notes discovered while refreshing."
            ),
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
    if workstreams := data.get("workstreams"):
        parts.append("")
        parts.append("## Workstreams")
        for stream in workstreams:
            header = f"- {stream.get('title', stream.get('slug', 'workstream'))}"
            desc = stream.get("description")
            if desc:
                header += f" — {desc}"
            parts.append(
                textwrap.fill(
                    header,
                    width=88,
                    initial_indent="",
                    subsequent_indent="  ",
                )
            )
            prompt = stream.get("prompt")
            if prompt:
                wrapped_prompt = textwrap.fill(
                    f"  {prompt}",
                    width=88,
                    subsequent_indent="  ",
                )
                parts.append(wrapped_prompt)
    return "\n".join(parts)


def print_workstream(data: Dict[str, Any], slug: str) -> bool:
    for stream in data.get("workstreams", []):
        if stream.get("slug") == slug:
            title = stream.get("title", slug)
            print(f"{title} ({slug})")
            description = stream.get("description")
            if description:
                print(textwrap.fill(description, width=88))
            prompt = stream.get("prompt")
            if prompt:
                print()
                print(textwrap.fill(prompt, width=88))
            return True
    return False


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="dump the raw WIP data as JSON")
    parser.add_argument(
        "--workstream",
        metavar="SLUG",
        help="print details for a single workstream",
    )
    args = parser.parse_args()

    if args.json:
        print(json.dumps(WIP_DATA, indent=2, sort_keys=True))
        return

    if args.workstream:
        if not print_workstream(WIP_DATA, args.workstream):
            parser.error(f"unknown workstream: {args.workstream}")
        return

    print(summarise(WIP_DATA))


if __name__ == "__main__":
    main()
