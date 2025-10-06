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
                "Viewer now renders the audio overlay direct from --audio-log-json; the next milestone focuses on projecting boot timeline metadata alongside the cue stream.",
                "Stabilise codec3 decoding so Manny's office colour plates and .zbm depth stay faithful in engine captures.",
                "Stand up a minimal game loop that loads Manny's office scripts, hooks input, and keeps hotspots functional.",
            ],
        },
        {
            "title": "Active Threads",
            "body": [
                "Geometry-driven head targeting now records real sector hits; upcoming work focuses on surfacing those cues to downstream tooling.",
                "Timeline overlay plumbing is queued so the viewer can surface hook sequencing while geometry instrumentation continues to evolve.",
                "Validate room bootstrap (scene assets, walkboxes, dialogues) inside the modern runtime and log gaps to close.",
                "Confirm Manny's office depth metadata drives correct draw ordering once interactions kick in.",
            ],
        },
        {
            "title": "Next Steps",
            "body": [
                "Project Manny's office boot timeline overlays into the viewer to close out milestone 1 instrumentation.",
                "Feed the overlay with hook sequencing and selection affordances so geometry and timeline views stay in sync.",
                "Demo entering Manny's office from boot with walk-able Manny and one interactive hotspot.",
                "Lock in regression coverage for codec3 colour + depth paths so the scene stays stable once playable.",
            ],
        },
    ],
    "workstreams": [
        {
            "slug": "viewer_timeline_overlay",
            "title": "Timeline overlay instrumentation",
            "description": "Timeline overlay instrumentation",
            "prompt": "Objective: layer Manny's office boot timeline metadata into grim_viewer so hook sequencing and entity focus appear directly in the HUD. Consume the existing --timeline JSON manifest, project stage labels and hook indices alongside the marker grid, and let ←/→ cycling highlight the corresponding overlay entry. Preserve behaviour when --timeline is absent. Document the flag pairing in docs/startup_overview.md, add targeted unit coverage for any timeline parsing helpers, and run cargo fmt && cargo test -p grim_viewer before handing off.",
        },
        {
            "slug": "scene_bootstrap",
            "title": "Scene bootstrap",
            "description": "Load Manny's office and basic gameplay loop",
            "prompt": "Objective: wire up the startup path that enters Manny's office with scripts, walkboxes, and camera state so we can move Manny and trigger baseline interactions. Capture blockers that prevent first-playable and co-ordinate fixes quickly.",
        },
        {
            "slug": "interaction_patchset",
            "title": "Interaction patchset",
            "description": "Keep office hotspots playable",
            "prompt": "Objective: ensure Manny can interact with a representative hotspot (e.g. pneumatic tube or desk) including dialogue playback, so the milestone demo feels alive. Trim scope to essentials and defer complex puzzle logic until after the first playable.",
        },
        {
            "slug": "codec3_regression",
            "title": "Codec3 regression",
            "description": "Harden Manny office texture decode",
            "prompt": "Objective: keep Manny's office rendering faithful by matching codec3 behaviour between colour .bm plates and .zbm depth maps. Ensure seeded windows mirror the original engine, expose depth ranges for validation, and land regression tests or tooling that prevent the half-black regression from returning.",
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
