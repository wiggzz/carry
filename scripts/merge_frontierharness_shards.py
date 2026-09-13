#!/usr/bin/env python3
"""Validate and combine evidence emitted by hosted FrontierHarness task shards."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import sys


def task_ids(path: Path) -> list[str]:
    values = [line.split("#", 1)[0].strip() for line in path.read_text(encoding="utf-8").splitlines()]
    values = [value for value in values if value]
    if not values or len(values) != len(set(values)):
        raise ValueError("expected task manifest must contain unique task IDs")
    return values


def run_configs(shards: Path) -> list[dict[str, object]]:
    configs: list[dict[str, object]] = []
    for path in shards.rglob("run.json"):
        try:
            configs.append(json.loads(path.read_text(encoding="utf-8")))
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid shard run metadata: {path}: {error}") from error
    if not configs:
        raise ValueError("no shard run.json files found")
    baseline = configs[0]
    fields = ("checkpoint", "harness", "model", "provider", "timeout_seconds", "egress_policy")
    for config in configs[1:]:
        if any(config.get(field) != baseline.get(field) for field in fields):
            raise ValueError("shard run metadata differs")
    return configs


def merge(expected: list[str], shards: Path, output: Path) -> dict[str, object]:
    configs = run_configs(shards)
    expected_set = set(expected)
    trials: dict[str, Path] = {}
    for trial_path in shards.rglob("trial.json"):
        if trial_path.parent.parent.name != "trials":
            continue
        try:
            trial = json.loads(trial_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid trial evidence: {trial_path}: {error}") from error
        task = trial.get("id")
        if task not in expected_set:
            raise ValueError(f"unexpected trial evidence: {task!r}")
        if task in trials:
            raise ValueError(f"duplicate trial evidence: {task}")
        trials[task] = trial_path
    missing = [task for task in expected if task not in trials]
    if missing:
        raise ValueError(f"missing trial evidence: {', '.join(missing)}")

    if output.exists():
        shutil.rmtree(output)
    trials_dir = output / "trials"
    trials_dir.mkdir(parents=True)
    for task in expected:
        shutil.copytree(trials[task].parent, trials_dir / task.replace("/", "-"))
    run = dict(configs[0])
    run["run_id"] = "combined"
    (output / "run.json").write_text(json.dumps(run, sort_keys=True) + "\n", encoding="utf-8")
    return {"task_count": len(expected), "run": run}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--expected", type=Path, required=True)
    parser.add_argument("--shards", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = merge(task_ids(args.expected), args.shards, args.out)
    except ValueError as error:
        parser.error(str(error))
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
