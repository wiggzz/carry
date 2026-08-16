#!/usr/bin/env python3
"""Plan a live SWE-bench run while keeping model execution fail-closed."""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import pathlib
import re
import shutil
import subprocess
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
    ("task", "/benchmark/task", "ro"),
    ("agent-output", "/benchmark/output", "rw"),
)
_EVALUATOR_MOUNTS = (
    ("task", "/benchmark/task", "ro"),
    ("evaluation-output", "/benchmark/output", "rw"),
)
_FORBIDDEN_HOST_PATHS = ("/home", "/var/run/docker.sock", "/runner", "/tmp")
_PINNED_IMAGE = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")


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
            ["OPENAI_API_KEY"],
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

    if agents.get("environment") != ["OPENAI_API_KEY"]:
        raise ValueError("agent environment must contain only OPENAI_API_KEY")
    if evaluator.get("environment") != []:
        raise ValueError("evaluator environment must be empty")
    actual_agent_mounts = {(item.get("source"), item.get("target"), item.get("mode")) for item in agents["mounts"]}
    actual_evaluator_mounts = {(item.get("source"), item.get("target"), item.get("mode")) for item in evaluator["mounts"]}
    if actual_agent_mounts != set(_AGENT_MOUNTS):
        raise ValueError("agent mounts do not match the reviewed input contract")
    if actual_evaluator_mounts != set(_EVALUATOR_MOUNTS):
        raise ValueError("evaluator mounts do not match the reviewed input contract")


def build_manifest(*, run_id: str, preset: str = "selected-50") -> dict[str, Any]:
    if not run_id or any(character not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-" for character in run_id):
        raise ValueError("run ID must contain only letters, digits, dot, underscore, and dash")
    selected = benchmark.load_selection()
    if preset == "smoke-5":
        instance_ids = selected[:5]
    elif preset == "selected-50":
        instance_ids = selected
    else:
        raise ValueError("preset must be smoke-5 or selected-50")
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
        "preset": preset,
        "selection": str(benchmark.DEFAULT_SELECTION.relative_to(ROOT)),
        "selection_sha256": benchmark.sha256_file(benchmark.DEFAULT_SELECTION),
        "task_count": len(instance_ids),
        "instance_ids": instance_ids,
        "methods": list(benchmark.METHODS),
        "record_count": len(slots),
        "slots": slots,
        "boundaries": build_boundaries(),
        "execution": "manual-protected-worker-only",
    }


def _validate_image(image: str) -> None:
    if not _PINNED_IMAGE.fullmatch(image):
        raise ValueError("container image must be pinned by a sha256 digest")


def _docker_base(*, task_dir: pathlib.Path, output_dir: pathlib.Path) -> list[str]:
    return [
        "run", "--rm", "--read-only", "--cap-drop=ALL",
        "--security-opt", "no-new-privileges",
        "--mount", f"type=bind,src={task_dir},dst=/benchmark/task,readonly",
        "--mount", f"type=bind,src={output_dir},dst=/benchmark/output",
    ]


def invoke(*, run_id: str, instance_id: str, method: str, task_dir: pathlib.Path,
           output_dir: pathlib.Path, agent_image: str, evaluator_image: str,
           docker_command: str = "docker") -> None:
    """Run one selected task/method through distinct external containers."""
    build_manifest(run_id=run_id)  # Revalidate the frozen 50 x 3 denominator.
    if instance_id not in benchmark.load_selection():
        raise ValueError("instance ID is not in the frozen selected-50 denominator")
    if method not in benchmark.METHODS:
        raise ValueError("method is not in the reviewed method denominator")
    _validate_image(agent_image)
    _validate_image(evaluator_image)
    if not os.environ.get("OPENAI_API_KEY"):
        raise ValueError("OPENAI_API_KEY is required for an agent invocation")

    task_dir = task_dir.resolve(strict=True)
    output_dir = output_dir.resolve(strict=True)
    if not task_dir.is_dir() or not output_dir.is_dir() or task_dir == output_dir:
        raise ValueError("task and output must be distinct existing directories")
    task_record = json.loads((task_dir / "task.json").read_text(encoding="utf-8"))
    if task_record.get("instance_id") != instance_id:
        raise ValueError("task record does not match requested instance ID")

    agent_output = output_dir / "agent"
    evaluator_output = output_dir / "evaluator"
    agent_output.mkdir(mode=0o700)
    evaluator_output.mkdir(mode=0o700)
    agent_command = [
        docker_command,
        *_docker_base(task_dir=task_dir, output_dir=agent_output),
        "--env", "OPENAI_API_KEY",
        agent_image, "run", "--method", method, "--instance-id", instance_id,
    ]
    subprocess.run(agent_command, check=True)

    patch = agent_output / "final.patch"
    if not patch.is_file():
        raise RuntimeError("agent container did not produce final.patch")
    evaluator_task = output_dir / "evaluator-task"
    shutil.copytree(task_dir, evaluator_task)
    patch_dir = evaluator_task / "agent-output"
    patch_dir.mkdir(mode=0o700)
    shutil.copyfile(patch, patch_dir / "final.patch")
    evaluator_command = [
        docker_command,
        *_docker_base(task_dir=evaluator_task, output_dir=evaluator_output),
        "--network", "none",
        evaluator_image, "evaluate", "--instance-id", instance_id,
        "--patch", "/benchmark/task/agent-output/final.patch",
    ]
    evaluator_env = {key: value for key, value in os.environ.items() if key != "OPENAI_API_KEY"}
    subprocess.run(evaluator_command, check=True, env=evaluator_env)
    metadata = {
        "schema": "carry.swe-bench-live-invocation.v1",
        "run_id": run_id,
        "instance_id": instance_id,
        "method": method,
        "selection_sha256": benchmark.sha256_file(benchmark.DEFAULT_SELECTION),
        "images": {"agent": agent_image, "evaluator": evaluator_image},
        "status": "containers-completed",
    }
    (output_dir / "run-metadata.json").write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    plan_parser = commands.add_parser("plan", help="write the fixed 150-slot run manifest")
    plan_parser.add_argument("--run-id", required=True)
    plan_parser.add_argument("--preset", choices=("smoke-5", "selected-50"), default="selected-50")
    plan_parser.add_argument("--output", type=pathlib.Path, required=True)
    invoke_parser = commands.add_parser("invoke", help="run one reviewed task/method on a disposable Docker worker")
    invoke_parser.add_argument("--run-id", required=True)
    invoke_parser.add_argument("--instance-id", required=True)
    invoke_parser.add_argument("--method", choices=benchmark.METHODS, required=True)
    invoke_parser.add_argument("--task-dir", type=pathlib.Path, required=True)
    invoke_parser.add_argument("--output-dir", type=pathlib.Path, required=True)
    invoke_parser.add_argument("--agent-image", required=True)
    invoke_parser.add_argument("--evaluator-image", required=True)
    invoke_parser.add_argument("--docker-command", default="docker", help=argparse.SUPPRESS)
    args = parser.parse_args()

    try:
        if args.command == "plan":
            payload = build_manifest(run_id=args.run_id, preset=args.preset)
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        else:
            invoke(
                run_id=args.run_id, instance_id=args.instance_id, method=args.method,
                task_dir=args.task_dir, output_dir=args.output_dir,
                agent_image=args.agent_image, evaluator_image=args.evaluator_image,
                docker_command=args.docker_command,
            )
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
