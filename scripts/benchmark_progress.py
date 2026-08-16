#!/usr/bin/env python3
"""Render new benchmark progress events from repeated EC2 console snapshots."""
import argparse
import hashlib
import json
import pathlib
import re
import sys


MARKER = "BENCHMARK_PROGRESS "
SAFE_VALUE = re.compile(r"^[A-Za-z0-9_.-]{1,160}$")


def load_seen(path: pathlib.Path) -> set[str]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return set()
    return {item for item in value if isinstance(item, str)} if isinstance(value, list) else set()


def render(event: dict[str, object]) -> str | None:
    instance_id = event.get("instance_id")
    method = event.get("method")
    state = event.get("state")
    if (not isinstance(instance_id, str) or not SAFE_VALUE.fullmatch(instance_id)
            or method not in ("carry", "codex", "pi")
            or state not in ("started", "completed", "grading", "graded")):
        return None
    prefix = f"[{method}] {instance_id} {state}"
    if state in ("completed", "graded"):
        status = event.get("status")
        elapsed = event.get("elapsed_seconds")
        if isinstance(status, str) and SAFE_VALUE.fullmatch(status):
            prefix += f": {status}"
        if isinstance(elapsed, (int, float)) and not isinstance(elapsed, bool) and elapsed >= 0:
            prefix += f" in {elapsed:.3f}s"
    return prefix


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--state", type=pathlib.Path, required=True)
    args = parser.parse_args()
    seen = load_seen(args.state)
    for line in sys.stdin:
        if MARKER not in line:
            continue
        payload = line.partition(MARKER)[2].strip()
        try:
            event = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        rendered = render(event)
        if rendered is None:
            continue
        identity = hashlib.sha256(
            json.dumps(event, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        if identity in seen:
            continue
        seen.add(identity)
        print(rendered)
    args.state.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.state.with_suffix(args.state.suffix + ".tmp")
    temporary.write_text(json.dumps(sorted(seen)) + "\n", encoding="utf-8")
    temporary.replace(args.state)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
