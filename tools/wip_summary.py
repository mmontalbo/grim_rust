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
                """Deliver a first-playable Manny office where the viewer renders in-world markers and Manny’s actual geometry, the ability to walk around and
                interact with the world, and placeholders for transitioning to other scenes""",
                "Every task should accelerate implementing the initial office gameplay, before thinking about work keep this ultimate goal in mind",
            ],
        },
        {
            "title": "Critical Path",
            "body": [
                "Bootstrap a depth-aware 3D marker/mesh pass in the viewer that shares camera math with runtime playback.",
                "Decode Manny’s mesh and transforms from the LAB assets and stage a reusable geometry cache for the viewer.",
                "Keep the desk/computer interaction script stable and the runtime regression harness green as we layer in 3D rendering.",
            ],
        },
        {
            "title": "Immediate Focus",
            "body": [
                "Exercise the new primitive mesh pass with Manny, desk, and tube anchors so we can confirm depth/parallax and selection highlights before swapping in decoded assets.",
                "Hook the new costume parser into an extractor that pulls Manny’s 3DO geometry/rig into artifacts/run_cache and documents any format assumptions we make.",
                "Run the Manny office interaction traces after each rendering change to ensure computer triggers and fallback handling remain stable.",
            ],
        },
        {
            "title": "Execution Notes",
            "body": [
                "Iterate with python tools/grim_viewer.py -- --headless to quickly validate the new 3D marker pass alongside existing overlays.",
                "Document decoded asset formats in docs/runtime_smoke_tests.md or a new decoder README so refresh steps stay reproducible.",
                "Use `cargo run -p grim_formats --bin cos_dump -- <costume>` to inspect costume component lists before wiring up 3DO decoding.",
                "After regenerating meshes or Lua snapshots, run cargo test -p grim_engine -- runtime_regression before committing.",
                "With the flat scene markers retired, spot-check the gold selection pointer and minimap overlays so interactions stay readable.",
                "Leverage the in-view axis gizmo to confirm world orientation when debugging new 3D markers or meshes.",
                "Document the primitive mesh legend (cones/spheres/cubes) and call out that overlap is expected until decoded meshes replace the proxies.",
            ],
        },
    ],
    "workstreams": [
        {
            "slug": "viewer-3d-renderer",
            "title": "Viewer 3D renderer",
            "description": "Give the viewer a depth-aware render path that can draw markers and meshes in world space.",
            "prompt": (
                "Use the instanced primitive meshes to mirror Manny/desk/tube anchors and verify depth and lighting in grim_viewer. "
                "Keep iterating on the gold selection pointer while we retire the flat overlay pass, then move toward swapping in decoded assets."
            ),
        },
        {
            "slug": "manny-mesh-decode",
            "title": "Decode Manny mesh",
            "description": "Extract Manny’s geometry/rig from LAB archives and stage it for the viewer.",
            "prompt": (
                "Build a decoder that reads the remastered LAB data, exports Manny’s mesh (and transforms if available) into artifacts/run_cache, "
                "and document the refresh command. Leave hooks for expanding to desk/tube assets next."
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
