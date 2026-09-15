#!/usr/bin/env python3
"""Merge three artifact-gated independent SWE-bench 50 replications."""
from __future__ import annotations

import argparse
import importlib.util
import json
import pathlib
import re
import sys
from typing import Any


HARNESSES = ("carry", "codex", "pi")
EXPECTED_ATTEMPTS = (1, 2, 3)


def load_worker() -> Any:
    path = pathlib.Path(__file__).with_name("swebench_smoke.py")
    spec = importlib.util.spec_from_file_location("swebench_smoke", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load SWE-bench worker finalizer")
    worker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(worker)
    return worker


def selected_harnesses(value: str) -> tuple[str, ...]:
    if value == "all":
        return HARNESSES
    if value not in HARNESSES:
        raise ValueError("harness must be carry, codex, pi, or all")
    return (value,)


def task_ids(path: pathlib.Path) -> list[str]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    values = payload.get("instance_ids") if isinstance(payload, dict) else None
    if not isinstance(values, list) or len(values) != 50 or len(set(values)) != 50:
        raise ValueError("manifest must contain exactly 50 unique instance IDs")
    if not all(isinstance(value, str) and value for value in values):
        raise ValueError("manifest instance IDs must be nonempty strings")
    return values


def load_attempt_artifacts(root: pathlib.Path, *, tasks: list[dict[str, str]],
                           harnesses: tuple[str, ...], worker: Any) -> tuple[list[dict[str, Any]], list[str], str]:
    paths = sorted(root.rglob("records.json"))
    if len(paths) != len(EXPECTED_ATTEMPTS):
        raise ValueError("expected exactly three attempt artifacts")
    records: list[dict[str, Any]] = []
    observed_attempts: set[int] = set()
    candidate_commits: set[str] = set()
    sources: list[str] = []
    expected_denominator = len(tasks) * len(harnesses)
    for records_path in paths:
        report_path = records_path.with_name("report.json")
        if not report_path.is_file():
            raise ValueError(f"attempt artifacts must include report.json: {records_path}")
        report = json.loads(report_path.read_text(encoding="utf-8"))
        provenance = report.get("provenance") if isinstance(report, dict) else None
        replication = provenance.get("replication") if isinstance(provenance, dict) else None
        attempt = replication.get("attempt") if isinstance(replication, dict) else None
        source_commit = provenance.get("source_commit") if isinstance(provenance, dict) else None
        if (report.get("denominator") != expected_denominator
                or not isinstance(provenance, dict)
                or provenance.get("mode") != "replicated-50"
                or provenance.get("phase") != "complete"
                or not isinstance(replication, dict)
                or replication.get("attempts_per_task_harness") != 3
                or not isinstance(source_commit, str)
                or not re.fullmatch(r"[0-9a-f]{40}", source_commit)
                or attempt not in EXPECTED_ATTEMPTS):
            raise ValueError(f"invalid replication provenance: {records_path}")
        payload = json.loads(records_path.read_text(encoding="utf-8"))
        if not isinstance(payload, list):
            raise ValueError(f"attempt artifact records must be a list: {records_path}")
        worker._validate_records(tasks, payload, harnesses, (attempt,))
        if attempt in observed_attempts:
            raise ValueError("duplicate attempt artifacts")
        observed_attempts.add(attempt)
        candidate_commits.add(source_commit)
        records.extend(payload)
        sources.append(records_path.relative_to(root).as_posix())
    if observed_attempts != set(EXPECTED_ATTEMPTS):
        raise ValueError("attempt artifacts must contain attempts 1, 2, and 3 exactly once")
    if len(candidate_commits) != 1:
        raise ValueError("attempt artifacts must have identical candidate commits")
    return records, sources, candidate_commits.pop()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=pathlib.Path, required=True)
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--harness", required=True, choices=(*HARNESSES, "all"))
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    try:
        worker = load_worker()
        ids = task_ids(args.manifest)
        harnesses = selected_harnesses(args.harness)
        tasks = [{"instance_id": instance_id} for instance_id in ids]
        records, sources, source_commit = load_attempt_artifacts(
            args.artifacts, tasks=tasks, harnesses=harnesses, worker=worker,
        )
        worker.finalize(
            tasks=tasks, records=records, output=args.out,
            provenance={
                "mode": "replicated-50", "phase": "complete", "source_commit": source_commit,
                "attempts_per_task_harness": 3,
                "source_record_artifacts": sources,
            },
            harnesses=harnesses, attempt_numbers=EXPECTED_ATTEMPTS,
        )
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"replication merge failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
