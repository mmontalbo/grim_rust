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
                "Deliver a first-playable Manny office: load the scene, walk Manny to the desk, and complete the computer interaction loop without regressions.",
                "Every task should move us closer to confidently demoing that flow end-to-end.",
            ],
        },
        {
            "title": "Critical Path",
            "body": [
                "Lock Manny’s geometry overlays so the viewer markers and runtime anchors match after every asset/script tweak.",
                "Stabilise hotspot scripting for desk/computer interactions; confirm triggers fire with the captured regression traces.",
                "Keep the runtime regression harness green after any artifact refresh so we trust the first-playable build.",
            ],
        },
        {
            "title": "Immediate Focus",
            "body": [
                "Validate the latest geometry snapshot (artifacts/run_cache/manny_geometry.json) against movement_log.json to catch drift early.",
                "Watch for hotspot.demo.fallback in hotspot_events.json and push Manny’s scripts so the computer interaction returns to suit without needing the fallback path.",
                "Spin up a dedicated minimap marker pipeline so HUD rendering cannot occlude the scene pass; keep layout docs updated with the split once it lands.",
            ],
        },
        {
            "title": "Execution Notes",
            "body": [
                "Run python tools/grim_viewer.py -- --headless to spot-check overlays whenever geometry or scripts move.",
                "Follow docs/runtime_smoke_tests.md for the one-button artifact refresh; the harness writes all Manny baselines in one pass.",
                "After regenerating assets or Lua snapshots, run cargo test -p grim_engine -- runtime_regression before committing.",
            ],
        },
    ],
    "workstreams": [
        {
            "slug": "manny-geometry-lock",
            "title": "Lock Manny geometry",
            "description": "Keep viewer overlays aligned with runtime anchors across script edits.",
            "prompt": (
                "Run the headless viewer to verify Manny/desk/tube markers. If drift appears, rerun the runtime smoke tests to regenerate "
                "movement, hotspot, depth, and geometry snapshots together, then restage the refreshed fixtures."
            ),
        },
        {
            "slug": "desk-interaction",
            "title": "Desk/computer interaction parity",
            "description": "Ensure hotspot scripting and fixtures replay the full interaction cleanly.",
            "prompt": (
                "Review hotspot_events.json alongside docs/runtime_smoke_tests.md expectations. Patch scripts or fixtures so Manny reaches the desk, "
                "triggers the computer, and exits without invoking hotspot.demo.fallback; update regression assets and rerun runtime_regression to lock it in."
            ),
        },
        {
            "slug": "runtime-stability",
            "title": "Runtime harness stability",
            "description": "Maintain passing runtime_regression results after artifact or script changes.",
            "prompt": (
                "After any refresh, run cargo test -p grim_engine -- runtime_regression and note the check-in that validated the baselines. "
                "Document failures immediately so we do not mask blockers on the path to first-playable."
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
