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
            "title": "Current Direction",
            "body": [
                "Boot simulation now stubs Manny's desk cut-scene so the Lua host logs memo retrieval and computer interactions without the original precompiled scripts.",
                "Viewer overlays now render audio and boot timeline metadata directly from --audio-log-json / --timeline so hook sequencing stays visible while iterating.",
                "Stabilise codec3 decoding so Manny's office colour plates and .zbm depth stay faithful in engine captures.",
                "Movement regression harness now guards Manny's walk path and the computer hotspot demo drives the first interaction; next focus shifts to sharing movement/timeline data across tooling.",
            ],
        },
        {
            "title": "Active Threads",
            "body": [
                "Geometry-driven head targeting now records real sector hits; upcoming work focuses on surfacing those cues to downstream tooling.",
                "Timeline overlay highlight now ships; next up is feeding it hotspot/movement traces so interactive regressions line up with stage sequencing.",
                "Validate room bootstrap (scene assets, walkboxes, dialogues) inside the modern runtime and log gaps to close.",
                "Carry the new computer hotspot demo into the regression harness so movement and timeline traces ship together.",
            ],
        },
        {
            "title": "Next Steps",
            "body": [
                "Marry the timeline overlay with the movement logger so stage changes and sector hits travel in one regression artifact alongside the hotspot run.",
                "Surface overlay selection data during hotspot playback so Manny's first interaction run can assert both geometry and hook sequencing.",
                "With the movement harness in place, demo entering Manny's office from boot with one interactive hotspot and capture the flow alongside the movement log in a reusable regression script.",
                "Lock in regression coverage for codec3 colour + depth paths and the Manny hotspot/movement smoke tests so the scene stays stable once playable.",
            ],
        },
        {
            "title": "Commit Conventions",
            "body": [
                "Format commits as: <area>: <short change summary> on the first line, then blank line, followed by 'Why:' and 'What:' bullet blocks summarising intent and implementation.",
                "Keep the bullet phrasing tight (hyphen bullets preferred) so reviewers see the rationale/changes without hunting through diffs.",
                "Avoid blank lines between Why/What bullet entries so the commit template stays compact and scannable.",
            ],
        },
    ],
    "workstreams": [
        {
            "slug": "hotspot_demo",
            "title": "Hotspot interaction demo",
            "description": "Computer hotspot storyline demoed end-to-end",
            "prompt": "Objective: wire the computer hotspot so Manny approaches, uses the verb, and plays the associated dialogue. Document the shims that keep WalkActorTo/SetActorRot viable and leave follow-ups for additional hotspots or harness integration.",
        },
        {
            "slug": "runtime_regression_harness",
            "title": "Runtime regression harness",
            "description": "Automate the Manny smoke test",
            "prompt": "Objective: capture a headless or CLI-driven run that walks Manny to the chosen hotspot and verifies expected audio/geometry signals. Store artefacts under tools/tests, wire it into the workflow docs, and ensure cargo test -p grim_engine exercises the harness.",
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
