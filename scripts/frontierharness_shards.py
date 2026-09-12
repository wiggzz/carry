#!/usr/bin/env python3
"""Emit a deterministic GitHub Actions matrix for FrontierHarness task shards."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

TASK_ID = re.compile(r"^(?:terminal-bench|datacurve)/[A-Za-z0-9_-]+$")


def read_tasks(path: Path) -> list[str]:
    tasks: list[str] = []
    seen: set[str] = set()
    for raw in path.read_text(encoding="utf-8").splitlines():
        task = raw.split("#", 1)[0].strip()
        if not task:
            continue
        if not TASK_ID.fullmatch(task):
            raise ValueError(f"invalid FrontierHarness task ID: {task!r}")
        if task in seen:
            raise ValueError(f"duplicate FrontierHarness task ID: {task}")
        seen.add(task)
        tasks.append(task)
    if not tasks:
        raise ValueError("task manifest is empty")
    return tasks


def shard(tasks: list[str], batch_size: int) -> list[dict[str, object]]:
    if batch_size < 1:
        raise ValueError("batch size must be positive")
    return [
        {"index": index, "tasks": tasks[start : start + batch_size]}
        for index, start in enumerate(range(0, len(tasks), batch_size))
    ]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tasks", type=Path, required=True)
    parser.add_argument("--batch-size", type=int, required=True)
    args = parser.parse_args()
    try:
        payload = {"include": shard(read_tasks(args.tasks), args.batch_size)}
    except ValueError as error:
        parser.error(str(error))
    json.dump(payload, sys.stdout, separators=(",", ":"))
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
