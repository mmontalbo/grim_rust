#!/usr/bin/env python3
"""
Quick-and-dirty diff for retail vs Rust engine trace logs.

Reads two log files, extracts `engine=... vm_id=... seq=... event=...` lines,
and reports the first mismatch in ordering or field values.
"""
import argparse
import re
import sys
from itertools import zip_longest
from typing import Dict, Iterable, List, Optional, Sequence, Set, Tuple

TOKEN_RE = re.compile(r'(\S+=".*?(?<!\\)"|\S+)')


def parse_event_lines(path: str) -> Iterable[Dict[str, str]]:
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            parsed = parse_line(line)
            if parsed:
                yield parsed


def parse_line(line: str) -> Optional[Dict[str, str]]:
    # Strip the shim prefix when present.
    if "] " in line:
        line = line.split("] ", 1)[1]
    tokens = TOKEN_RE.findall(line.strip())
    if not tokens:
        return None
    fields: Dict[str, str] = {}
    for token in tokens:
        if "=" not in token:
            continue
        key, value = token.split("=", 1)
        if value.startswith('"') and value.endswith('"'):
            value = value[1:-1].replace('\\"', '"')
        fields[key] = value
    if "event" not in fields:
        return None
    return fields


def sanitize(event: Dict[str, str], ignore: Set[str]) -> Dict[str, str]:
    return {k: v for k, v in event.items() if k not in ignore}


def compare_streams(
    lhs: Sequence[Dict[str, str]],
    rhs: Sequence[Dict[str, str]],
    ignore: Set[str],
    context: int,
) -> int:
    for idx, (a, b) in enumerate(zip_longest(lhs, rhs), start=1):
        if a is None and b is None:
            return 0
        if a is None:
            print(f"lhs ended before rhs at event #{idx}")
            show_context(rhs, idx - 1, context, "rhs")
            return 1
        if b is None:
            print(f"rhs ended before lhs at event #{idx}")
            show_context(lhs, idx - 1, context, "lhs")
            return 1

        sa = sanitize(a, ignore)
        sb = sanitize(b, ignore)

        if sa == sb:
            continue

        diffs = diff_fields(sa, sb)
        print(f"mismatch at event #{idx}:")
        print(f"  lhs event={a.get('event')} label={a.get('label')} handle={a.get('handle')}")
        print(f"  rhs event={b.get('event')} label={b.get('label')} handle={b.get('handle')}")
        for field, va, vb in diffs:
            print(f"  field {field}: lhs={va} rhs={vb}")
        show_context(lhs, idx - 1, context, "lhs")
        show_context(rhs, idx - 1, context, "rhs")
        return 1
    return 0


def diff_fields(
    lhs: Dict[str, str], rhs: Dict[str, str]
) -> List[Tuple[str, Optional[str], Optional[str]]]:
    fields: Set[str] = set(lhs.keys()) | set(rhs.keys())
    diffs: List[Tuple[str, Optional[str], Optional[str]]] = []
    for field in sorted(fields):
        if lhs.get(field) != rhs.get(field):
            diffs.append((field, lhs.get(field), rhs.get(field)))
    return diffs


def show_context(events: Sequence[Dict[str, str]], center: int, window: int, label: str) -> None:
    start = max(center - window, 0)
    end = min(center + window + 1, len(events))
    if start >= end:
        return
    print(f"  {label} context [{start}:{end}):")
    for idx in range(start, end):
        marker = "->" if idx == center else "  "
        print(f"    {marker}#{idx+1} {format_event(events[idx])}")


def format_event(event: Dict[str, str]) -> str:
    parts = [
        event.get("event", "?"),
        f"label={event.get('label', '?')}",
        f"handle={event.get('handle', '?')}",
        f"seq={event.get('seq', '?')}",
        f"ts={event.get('ts', '?')}",
    ]
    return " ".join(parts)


def main() -> int:
    parser = argparse.ArgumentParser(description="diff retail vs rust trace logs")
    parser.add_argument("lhs", help="retail log path")
    parser.add_argument("rhs", help="rust log path")
    parser.add_argument(
        "--ignore",
        action="append",
        default=["seq", "ts"],
        help="field to ignore (repeatable). Defaults: seq, ts",
    )
    parser.add_argument(
        "--context",
        type=int,
        default=3,
        help="lines of context to show around a mismatch (default: 3)",
    )
    args = parser.parse_args()

    lhs_events = list(parse_event_lines(args.lhs))
    rhs_events = list(parse_event_lines(args.rhs))
    print(f"loaded {len(lhs_events)} lhs events and {len(rhs_events)} rhs events")
    return compare_streams(lhs_events, rhs_events, set(args.ignore), args.context)


if __name__ == "__main__":
    raise SystemExit(main())
