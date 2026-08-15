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
DEFAULT_VERIFIED_CONFIG = ROOT / "benchmarks" / "swe-bench-verified.json"
METHODS = ("carry", "codex")


def sha256_file(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_verified_config(path: pathlib.Path = DEFAULT_VERIFIED_CONFIG) -> dict[str, Any]:
    config = json.loads(path.read_text(encoding="utf-8"))
    dataset = config.get("dataset") if isinstance(config, dict) else None
    presets = config.get("presets") if isinstance(config, dict) else None
    if not isinstance(dataset, dict) or not isinstance(presets, dict):
        raise ValueError("verified benchmark config requires dataset and presets objects")
    if dataset.get("name") != "SWE-bench/SWE-bench_Verified":
        raise ValueError("verified benchmark config has an unexpected dataset")
    if not isinstance(dataset.get("revision"), str) or not dataset["revision"]:
        raise ValueError("verified benchmark config requires an immutable dataset revision")
    if dataset.get("task_count") != 500:
        raise ValueError("verified benchmark config must declare 500 tasks")
    required_presets = {"smoke-5", "selected-50", "verified-full"}
    if set(presets) != required_presets:
        raise ValueError("verified benchmark config has unexpected presets")
    return config


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


def select_seeded_subset(instance_ids: Iterable[str], *, task_count: int, seed: str) -> list[str]:
    """Select an order-independent deterministic subset using a published seed."""
    ids = list(instance_ids)
    if not isinstance(seed, str) or not seed:
        raise ValueError("selection seed must be a non-empty string")
    if any(not isinstance(instance_id, str) or not instance_id for instance_id in ids):
        raise ValueError("instance IDs contain an invalid value")
    if len(set(ids)) != len(ids):
        raise ValueError("instance IDs contain a duplicate")
    if not 1 <= task_count <= len(ids):
        raise ValueError(f"task count must be between 1 and {len(ids)}")
    ranked = sorted(
        ids,
        key=lambda instance_id: (hashlib.sha256(f"{seed}\0{instance_id}".encode("utf-8")).hexdigest(), instance_id),
    )
    return ranked[:task_count]


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
    actual: list[tuple[str, str]] = []
    for record in records:
        if not isinstance(record, dict):
            raise ValueError("record input must be an object")
        instance_id, method = record.get("instance_id"), record.get("method")
        if not isinstance(instance_id, str) or not instance_id:
            raise ValueError("record input contains an invalid instance_id")
        if not isinstance(method, str) or not method:
            raise ValueError("record input contains an invalid method")
        actual.append((instance_id, method))
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
