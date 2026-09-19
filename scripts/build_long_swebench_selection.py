#!/usr/bin/env python3
"""Freeze SWE-bench Verified long-trajectory manifests from public trace metadata.

The input directory contains public `*.traj.json` files published by a single
SWE-bench Verified submission.  Only `instance_id` and `info.model_stats`
metadata are read; model messages and patches are never written to manifests.
"""
import argparse
import hashlib
import json
import pathlib
from typing import Any

SCHEMA = "carry.swe-bench-long-trajectory-selection.v1"
SMOKE_SCHEMA = "carry.swe-bench-long-trajectory-smoke.v1"
RANK_ORDER = "api_calls desc, instance_id asc"


def canonical(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")


def measurements_from(directory: pathlib.Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    seen: set[str] = set()
    for path in sorted(directory.rglob("*.traj.json")):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
            instance_id = payload["instance_id"]
            stats = payload["info"]["model_stats"]
            calls = stats["api_calls"]
            cost = stats["instance_cost"]
        except (OSError, TypeError, KeyError, ValueError, json.JSONDecodeError) as error:
            raise ValueError(f"invalid trajectory metadata: {path}") from error
        if not isinstance(instance_id, str) or not instance_id:
            raise ValueError(f"invalid instance_id: {path}")
        if instance_id in seen:
            raise ValueError(f"duplicate instance_id: {instance_id}")
        if isinstance(calls, bool) or not isinstance(calls, int) or calls < 0:
            raise ValueError(f"invalid api_calls for {instance_id}")
        if isinstance(cost, bool) or not isinstance(cost, (int, float)) or cost < 0:
            raise ValueError(f"invalid instance_cost for {instance_id}")
        seen.add(instance_id)
        rows.append({"instance_id": instance_id, "api_calls": calls, "instance_cost_usd": cost})
    if not rows:
        raise ValueError("no trajectory metadata found")
    return rows


def build_manifests(directory: pathlib.Path, *, source_dataset: str,
                    source_manifest_sha256: str, selection_size: int = 50,
                    smoke_size: int = 5) -> tuple[dict[str, Any], dict[str, Any]]:
    if len(source_manifest_sha256) != 64 or any(character not in "0123456789abcdef" for character in source_manifest_sha256):
        raise ValueError("source_manifest_sha256 must be 64 lowercase hex characters")
    if not 1 <= smoke_size <= selection_size:
        raise ValueError("smoke_size must be positive and no greater than selection_size")
    rows = measurements_from(directory)
    ordered = sorted(rows, key=lambda row: (-row["api_calls"], row["instance_id"]))
    if len(ordered) < selection_size:
        raise ValueError("trajectory metadata has fewer rows than selection_size")
    measurement_digest = hashlib.sha256(canonical(sorted(rows, key=lambda row: row["instance_id"]))).hexdigest()
    ranking = [dict(row, rank=index) for index, row in enumerate(ordered[:selection_size], start=1)]
    instance_ids = [row["instance_id"] for row in ranking]
    source = {
        "metric": "info.model_stats.api_calls",
        "rank_order": RANK_ORDER,
        "measurement_count": len(rows),
        "measurement_sha256": measurement_digest,
        "trajectory_format": "mini-swe-agent-1.1",
    }
    manifest = {
        "schema": SCHEMA,
        "source_dataset": source_dataset,
        "source_manifest_sha256": source_manifest_sha256,
        "trajectory_source": source,
        "instance_ids": instance_ids,
        "ranking": ranking,
    }
    smoke = {
        "schema": SMOKE_SCHEMA,
        "parent_selection_sha256": hashlib.sha256(canonical(manifest)).hexdigest(),
        "instance_ids": instance_ids[:smoke_size],
    }
    return manifest, smoke


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--trajectory-dir", type=pathlib.Path, required=True)
    parser.add_argument("--selection-output", type=pathlib.Path, required=True)
    parser.add_argument("--smoke-output", type=pathlib.Path, required=True)
    parser.add_argument("--source-dataset", default="princeton-nlp/SWE-bench_Verified")
    parser.add_argument("--source-manifest-sha256", required=True)
    args = parser.parse_args()
    manifest, smoke = build_manifests(args.trajectory_dir, source_dataset=args.source_dataset,
                                      source_manifest_sha256=args.source_manifest_sha256)
    for path, value in ((args.selection_output, manifest), (args.smoke_output, smoke)):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
