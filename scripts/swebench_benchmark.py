#!/usr/bin/env python3
"""Portable, deterministic planning and result-merging support for SWE-bench runs.

This module intentionally keeps the benchmark selection separate from task data:
selection files contain only public instance IDs, never gold patches. A worker must
fetch full SWE-bench records at runtime and keep them outside agent workspaces.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
from collections import Counter
from typing import Any, Iterable

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_SELECTION = ROOT / "benchmarks" / "swe-bench-verified-50.json"
METHODS = ("carry", "codex")


def sha256_file(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_selection(path: pathlib.Path = DEFAULT_SELECTION) -> list[str]:
    data = json.loads(path.read_text(encoding="utf-8"))
    ids = data.get("instance_ids")
    if not isinstance(ids, list) or len(ids) != 50:
        raise ValueError("selection must contain exactly 50 instance IDs")
    if any(not isinstance(value, str) or not value for value in ids):
        raise ValueError("selection contains an invalid instance ID")
    if len(set(ids)) != len(ids):
        raise ValueError("selection contains duplicate instance IDs")
    return ids


def select_shard(tasks: list[dict[str, Any]], index: int, count: int) -> list[dict[str, Any]]:
    """Return a contiguous, deterministic shard; concatenated shards retain input order."""
    if count < 1 or not 0 <= index < count:
        raise ValueError("shard index must be in [0, shard count)")
    start = len(tasks) * index // count
    end = len(tasks) * (index + 1) // count
    return tasks[start:end]


def validate_merged_records(
    tasks: list[dict[str, Any]], records: Iterable[dict[str, Any]], methods: tuple[str, ...] = METHODS
) -> None:
    task_ids = [task.get("instance_id") for task in tasks]
    if any(not isinstance(instance_id, str) or not instance_id for instance_id in task_ids):
        raise ValueError("task input contains an invalid instance ID")
    if len(set(task_ids)) != len(task_ids):
        raise ValueError("task input contains a duplicate task ID")
    if len(set(methods)) != len(methods):
        raise ValueError("method input contains a duplicate method")
    expected = {(instance_id, method) for instance_id in task_ids for method in methods}
    actual = [(record.get("instance_id"), record.get("method")) for record in records]
    counts = Counter(actual)
    if len(actual) != len(expected) or set(actual) != expected or any(count != 1 for count in counts.values()):
        raise ValueError(
            f"expected exactly {len(expected)} unique task/method records; got {len(actual)} records and {len(set(actual))} unique pairs"
        )


def plan(selection: pathlib.Path, shard_index: int, shard_count: int, methods: tuple[str, ...]) -> dict[str, Any]:
    ids = load_selection(selection)
    tasks = [{"instance_id": instance_id} for instance_id in ids]
    shard = select_shard(tasks, shard_index, shard_count)
    return {
        "selection": str(selection),
        "selection_sha256": sha256_file(selection),
        "task_count": len(ids),
        "shard_index": shard_index,
        "shard_count": shard_count,
        "methods": list(methods),
        "instance_ids": [task["instance_id"] for task in shard],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--plan", action="store_true", help="emit one deterministic task shard; no model or task setup")
    modes.add_argument("--merge", action="store_true", help="validate and merge JSON-array worker records")
    parser.add_argument("--selection", type=pathlib.Path, default=DEFAULT_SELECTION)
    parser.add_argument("--shard-index", type=int, default=0)
    parser.add_argument("--shard-count", type=int, default=5)
    parser.add_argument("--method", choices=METHODS, action="append", dest="methods")
    parser.add_argument("--records", type=pathlib.Path, action="append", default=[])
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    methods = tuple(args.methods or METHODS)

    if args.plan:
        payload = plan(args.selection, args.shard_index, args.shard_count, methods)
    else:
        records: list[dict[str, Any]] = []
        for path in args.records:
            loaded = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(loaded, list):
                raise ValueError(f"{path} does not contain a JSON record array")
            records.extend(loaded)
        tasks = [{"instance_id": instance_id} for instance_id in load_selection(args.selection)]
        validate_merged_records(tasks, records, methods)
        payload = {
            "selection": str(args.selection),
            "selection_sha256": sha256_file(args.selection),
            "task_count": len(tasks),
            "methods": list(methods),
            "records": records,
        }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
