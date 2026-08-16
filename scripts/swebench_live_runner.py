#!/usr/bin/env python3
"""Plan a live SWE-bench run while keeping model execution fail-closed."""
from __future__ import annotations

import argparse
import importlib.util
import json
import pathlib
import sys
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[1]
BENCHMARK_SCRIPT = pathlib.Path(__file__).with_name("swebench_benchmark.py")


def _load_benchmark() -> Any:
    spec = importlib.util.spec_from_file_location("swebench_benchmark", BENCHMARK_SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load benchmark planning support")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


benchmark = _load_benchmark()

_AGENT_MOUNTS = (
    ("checkout", "/benchmark/repo", "rw"),
    ("task", "/benchmark/task.json", "ro"),
    ("agent-output", "/benchmark/output", "rw"),
)
_EVALUATOR_MOUNTS = (
    ("evaluation-checkout", "/benchmark/repo", "rw"),
    ("task", "/benchmark/task.json", "ro"),
    ("patch", "/benchmark/patch.diff", "ro"),
    ("evaluation-output", "/benchmark/output", "rw"),
)
_FORBIDDEN_HOST_PATHS = ("/home", "/var/run/docker.sock", "/runner", "/tmp")


def _boundary(boundary_id: str, mounts: tuple[tuple[str, str, str], ...], environment: list[str]) -> dict[str, Any]:
    return {
        "boundary_id": boundary_id,
        "kind": "external-container",
        "mounts": [
            {"source": source, "target": target, "mode": mode}
            for source, target, mode in mounts
        ],
        "environment": environment,
        "host_home": "none",
        "docker_socket": "none",
        "workflow_temp": "none",
    }


def build_boundaries() -> dict[str, dict[str, Any]]:
    """Return the minimum reviewed inputs for the two execution boundaries."""
    boundaries = {
        "agents": _boundary(
            "model-agent-container",
            _AGENT_MOUNTS,
            ["CREDENTIAL_BROKER_SOCKET"],
        ),
        "evaluator": _boundary("swebench-evaluator-container", _EVALUATOR_MOUNTS, []),
    }
    validate_boundaries(boundaries)
    return boundaries


def validate_boundaries(boundaries: dict[str, dict[str, Any]]) -> None:
    """Reject boundary plans that could expose host state or credentials."""
    if set(boundaries) != {"agents", "evaluator"}:
        raise ValueError("exactly agent and evaluator boundaries are required")
    agents, evaluator = boundaries["agents"], boundaries["evaluator"]
    if agents.get("kind") != "external-container" or evaluator.get("kind") != "external-container":
        raise ValueError("both lanes require an external container boundary")
    if not agents.get("boundary_id") or agents["boundary_id"] == evaluator.get("boundary_id"):
        raise ValueError("agent and evaluator boundaries must be distinct")

    for name, boundary in boundaries.items():
        for field in ("host_home", "docker_socket", "workflow_temp"):
            if boundary.get(field) != "none":
                raise ValueError(f"{name} boundary exposes forbidden host state: {field}")
        for mount in boundary.get("mounts", []):
            source = mount.get("source")
            if not isinstance(source, str) or not source:
                raise ValueError(f"{name} boundary has an invalid mount source")
            if source.startswith(_FORBIDDEN_HOST_PATHS) or pathlib.PurePath(source).is_absolute():
                raise ValueError(f"{name} boundary uses forbidden host path: {source}")

    if agents.get("environment") != ["CREDENTIAL_BROKER_SOCKET"]:
        raise ValueError("agent environment must contain only the credential broker socket")
    if evaluator.get("environment") != []:
        raise ValueError("evaluator environment must be empty")
    actual_agent_mounts = {(item.get("source"), item.get("target"), item.get("mode")) for item in agents["mounts"]}
    actual_evaluator_mounts = {(item.get("source"), item.get("target"), item.get("mode")) for item in evaluator["mounts"]}
    if actual_agent_mounts != set(_AGENT_MOUNTS):
        raise ValueError("agent mounts do not match the reviewed input contract")
    if actual_evaluator_mounts != set(_EVALUATOR_MOUNTS):
        raise ValueError("evaluator mounts do not match the reviewed input contract")


def build_manifest(*, run_id: str) -> dict[str, Any]:
    if not run_id or any(character not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-" for character in run_id):
        raise ValueError("run ID must contain only letters, digits, dot, underscore, and dash")
    instance_ids = benchmark.load_selection()
    slots = [
        {"ordinal": ordinal, "instance_id": instance_id, "method": method}
        for ordinal, (instance_id, method) in enumerate(
            (instance_id, method)
            for instance_id in instance_ids
            for method in benchmark.METHODS
        )
    ]
    benchmark.validate_merged_records(
        [{"instance_id": instance_id} for instance_id in instance_ids],
        slots,
    )
    return {
        "schema": "carry.swe-bench-live-plan.v1",
        "run_id": run_id,
        "selection": str(benchmark.DEFAULT_SELECTION.relative_to(ROOT)),
        "selection_sha256": benchmark.sha256_file(benchmark.DEFAULT_SELECTION),
        "task_count": len(instance_ids),
        "methods": list(benchmark.METHODS),
        "record_count": len(slots),
        "slots": slots,
        "boundaries": build_boundaries(),
        "execution": "blocked-pending-credential-broker",
    }


def authorize_live() -> None:
    """Permanent fail-closed gate for v1; intentionally has no credential input."""
    raise RuntimeError(
        "live model execution is disabled: a reviewed credential broker integration is not implemented"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    plan_parser = commands.add_parser("plan", help="write the fixed 150-slot run manifest")
    plan_parser.add_argument("--run-id", required=True)
    plan_parser.add_argument("--output", type=pathlib.Path, required=True)
    commands.add_parser("authorize-live", help="fail until a reviewed credential broker is implemented")
    args = parser.parse_args()

    try:
        if args.command == "authorize-live":
            authorize_live()
        else:
            payload = build_manifest(run_id=args.run_id)
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    except (RuntimeError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
