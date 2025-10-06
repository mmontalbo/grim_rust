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
                "Familiarise yourself with README.md and the sections below, then pick up the next item in this summary and work it through to a committable state (tests/docs updated, WIP refreshed, commit created).",
                "List all available workstreams with: python tools/wip_summary.py --json (or refer to the ## Workstreams section).",
                "For a specific thread, run: python tools/wip_summary.py --workstream <slug> (for example --workstream codec3_regression).",
                "Use python tools/grim_viewer.py run to load the Manny baseline overlays (add -- --headless on machines without a windowing environment).",
                "The Manny office regression artefacts we reference live under tools/tests/ (movement_log.json, hotspot_events.json, etc.); skim them the first time so you know what the viewer overlays are visualising.",
            ],
        },
        {
            "title": "Current Focus",
            "body": [
                "Validate the Manny office minimap and plate overlays against the trimmed Manny/desk/tube/props list so names, colours, and highlights stay in sync.",
                "Prototype an egui-powered overlay inspector so the Taffy layout can be tweaked interactively without wgpu math spelunking.",
                "Rev the Manny baseline overlays once the minimap orientation is locked so the perspective and top-down views use the same regression artefacts.",
                "Run the Manny computer loop end-to-end in grim_engine/grim_viewer, fixing load→navigate→interact blockers (pathing, dialogue triggers, audio) until the first-playable loop is reliable.",
            ],
        },
        {
            "title": "Upcoming Targets",
            "body": [
                "Add a minimap toggle and quick legend in grim_viewer once playback validation passes so the panel remains useful without overwhelming the first-playable workflow.",
                "Spot-check the minimap axis heuristic in other Manny office captures and document the Rust layout abstraction so future overlays reuse it instead of ad-hoc NDC math.",
                "Keep the Manny computer hotspot regression artefacts (movement, audio, timeline, depth, event log) current so cargo test -p grim_engine -- runtime_regression remains green; hotspot events now backfill the opening frames and approach markers anchor to the first movement frame, so refresh the full capture set together when behaviour shifts.",
                "Maintain codec3 colour/depth parity while we iterate on tooling so Manny's office rendering never regresses.",
            ],
        },
        {
            "title": "Commit Conventions",
            "body": [
                "Format commits as: <area>: <short change summary> on the first line, then blank line, followed by 'Why:' and 'What:' bullet blocks summarising intent and implementation.",
                "Keep the bullet phrasing tight (hyphen bullets preferred) so reviewers see the rationale/changes without hunting through diffs.",
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
            "slug": "runtime_regression",
            "title": "Runtime regression harness",
            "description": "Lock Manny hotspot + movement baselines",
            "prompt": "Objective: keep the CLI hotspot demo capturing movement/audio/timeline/depth artefacts that mirror Manny's office. Regenerate artefacts when intent changes, update docs/tests, and ensure cargo test -p grim_engine -- runtime_regression remains green.",
        },
        {
            "slug": "hotspot_overlay",
            "title": "Hotspot overlay integration",
            "description": "Surface hotspot traces in viewer",
            "prompt": "Objective: build on the movement overlay by wiring hotspot/timeline selections into grim_viewer so geometry/head-targeting debugging stays aligned with the runtime captures. With movement/head markers now matching the regression frames via the recovered camera projection, layer in playback controls and per-event focus so scrubbing the desk interaction stays synced across tooling.",
        },
        {
            "slug": "codec3_regression",
            "title": "Codec3 regression",
            "description": "Harden Manny office texture decode",
            "prompt": "Objective: keep Manny's office rendering faithful by matching codec3 behaviour between colour .bm plates and .zbm depth maps. Ensure seeded windows mirror the original engine, expose depth ranges for validation, and land regression tests or tooling (prefer automated snapshots) that prevent the half-black regression from returning.",
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
