#!/usr/bin/env python3
"""Worker-side SWE-bench smoke orchestration and artifact validation."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import subprocess
from typing import Any, Mapping

METHODS = ("carry", "codex", "pi")
DATASET = "princeton-nlp/SWE-bench_Verified"
DATASET_REVISION = "c104f840cc67f8b6eec6f759ebc8b2693d585d4a"
VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
DIGEST_IMAGE = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")


def validate_config(values: Mapping[str, str]) -> dict[str, str]:
    required = ("BASE_IMAGE", "CODEX_VERSION", "PI_VERSION", "CODEX_COMMAND", "PI_COMMAND", "MODEL", "REASONING")
    config = {key: values.get(key, "") for key in required}
    if not DIGEST_IMAGE.fullmatch(config["BASE_IMAGE"]):
        raise ValueError("BASE_IMAGE must use an immutable sha256 digest")
    for key in ("CODEX_VERSION", "PI_VERSION"):
        if not VERSION.fullmatch(config[key]):
            raise ValueError(f"{key} must be an exact package version")
    if config["PI_VERSION"] != "0.84.2":
        raise ValueError("PI_VERSION must be 0.84.2")
    for key in ("CODEX_COMMAND", "PI_COMMAND"):
        if not config[key] or "{model}" not in config[key] or "{prompt}" not in config[key]:
            raise ValueError(f"{key} must explicitly contain model and prompt placeholders")
    if not config["MODEL"] or not config["REASONING"]:
        raise ValueError("model and reasoning configuration are required")
    return config


def agent_docker_command(*, image: str, method: str, repo: pathlib.Path, task_input: pathlib.Path,
                         output: pathlib.Path, model: str, reasoning: str) -> list[str]:
    if method not in METHODS:
        raise ValueError("unknown method")
    return [
        "docker", "run", "--rm", "--read-only", "--cap-drop=ALL",
        "--security-opt", "no-new-privileges", "--env", "OPENAI_API_KEY",
        "--mount", f"type=bind,src={repo.resolve()},dst=/workspace",
        "--mount", f"type=bind,src={task_input.resolve()},dst=/benchmark/input,readonly",
        "--mount", f"type=bind,src={output.resolve()},dst=/benchmark/output",
        "--workdir", "/workspace", image, "run", "--model", model,
        "--reasoning", reasoning, "--prompt", "/benchmark/input/task.md",
        "--output", "/benchmark/output",
    ]


def run_agent(*, instance_id: str, method: str, image: str, repo: pathlib.Path,
              task_input: pathlib.Path, output: pathlib.Path, model: str, reasoning: str) -> dict[str, Any]:
    command = agent_docker_command(
        image=image, method=method, repo=repo, task_input=task_input, output=output,
        model=model, reasoning=reasoning,
    )
    try:
        subprocess.run(command, check=True)
        patch_file = output / "final.patch"
        patch = patch_file.read_text(encoding="utf-8") if patch_file.is_file() else ""
        if not patch_file.is_file():
            raise RuntimeError("agent did not produce final.patch")
        return {"instance_id": instance_id, "method": method, "status": "agent-completed", "patch": patch,
                "error": None, "attempts": 1, "retries": 0}
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        patch_file = output / "final.patch"
        patch = patch_file.read_text(encoding="utf-8") if patch_file.is_file() else ""
        return {"instance_id": instance_id, "method": method, "status": "agent-failed", "patch": patch,
                "error": str(error), "attempts": 1, "retries": 0}


def run_official_evaluation(*, predictions: pathlib.Path, canonical_dataset: pathlib.Path,
                            instance_ids: list[str], run_id: str, output: pathlib.Path,
                            environment: Mapping[str, str] | None = None) -> None:
    env = dict(environment if environment is not None else os.environ)
    for key in list(env):
        if key.startswith("OPENAI_"):
            del env[key]
    command = [
        os.environ.get("PYTHON", "python3"), "-m", "swebench.harness.run_evaluation",
        "--dataset_name", str(canonical_dataset),
        "--split", "test", "--predictions_path", str(predictions),
        "--run_id", run_id, "--report_dir", str(output), "--max_workers", "1",
        "--instance_ids", *instance_ids,
    ]
    subprocess.run(command, check=True, env=env, cwd=output)


def load_resolved_ids(report_dir: pathlib.Path) -> set[str]:
    for path in sorted(report_dir.rglob("*.json")):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(payload, dict):
            continue
        for key in ("resolved_ids", "resolved_instances"):
            values = payload.get(key)
            if isinstance(values, list) and all(isinstance(value, str) for value in values):
                return set(values)
    raise RuntimeError("official evaluator report did not contain resolved IDs")


def _clone(repo: str, commit: str, destination: pathlib.Path) -> None:
    subprocess.run(["git", "clone", "--quiet", f"https://github.com/{repo}.git", str(destination)], check=True)
    subprocess.run(["git", "-C", str(destination), "checkout", "--quiet", "--detach", commit], check=True)


def materialize(*, records: list[dict[str, Any]], selected_ids: list[str], root: pathlib.Path,
                clone: Any = _clone) -> list[dict[str, Any]]:
    by_id = {record.get("instance_id"): record for record in records}
    if len(selected_ids) != 5 or len(set(selected_ids)) != 5 or any(instance_id not in by_id for instance_id in selected_ids):
        raise ValueError("canonical dataset does not contain the exact smoke-5 selection")
    selected = [by_id[instance_id] for instance_id in selected_ids]
    root.mkdir(parents=True, exist_ok=True)
    (root / "canonical-dataset.json").write_text(json.dumps(selected, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tasks = []
    for record in selected:
        instance_id = record["instance_id"]
        task_root = root / "tasks" / instance_id
        input_dir = task_root / "input"
        input_dir.mkdir(parents=True)
        public = {
            "instance_id": instance_id, "repo": record["repo"],
            "base_commit": record["base_commit"], "problem_statement": record["problem_statement"],
        }
        (input_dir / "task.json").write_text(json.dumps(public, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        (input_dir / "task.md").write_text(record["problem_statement"] + "\n", encoding="utf-8")
        for method in METHODS:
            clone(record["repo"], record["base_commit"], task_root / method / "repo")
        tasks.append(public)
    return tasks


def build_images(*, source: pathlib.Path, run_id: str, config: Mapping[str, str], execute: Any = subprocess.run) -> dict[str, Any]:
    validated = validate_config(config)
    carry_base = config.get("CARRY_BASE_IMAGE", "")
    if not DIGEST_IMAGE.fullmatch(carry_base):
        raise ValueError("CARRY_BASE_IMAGE must use an immutable sha256 digest")
    method_dir = source / "containers" / "swebench-method"
    specifications = {
        "carry": {
            "dockerfile": source / "containers" / "swebench-method" / "Dockerfile.carry",
            "context": source, "base": carry_base, "package_version": config.get("SOURCE_COMMIT", "current-source"),
            "args": [],
        },
        "codex": {
            "dockerfile": method_dir / "Dockerfile.node", "context": method_dir,
            "base": validated["BASE_IMAGE"], "package_version": validated["CODEX_VERSION"],
            "args": ["PACKAGE=@openai/codex", f"PACKAGE_VERSION={validated['CODEX_VERSION']}", f"AGENT_COMMAND={validated['CODEX_COMMAND']}"],
        },
        "pi": {
            "dockerfile": method_dir / "Dockerfile.node", "context": method_dir,
            "base": validated["BASE_IMAGE"], "package_version": validated["PI_VERSION"],
            "args": ["PACKAGE=@earendil-works/pi-coding-agent", f"PACKAGE_VERSION={validated['PI_VERSION']}", f"AGENT_COMMAND={validated['PI_COMMAND']}"],
        },
    }
    result = {}
    for method, spec in specifications.items():
        tag = f"swebench-{run_id}-{method}"
        command = ["docker", "build", "--pull", "--file", str(spec["dockerfile"]), "--tag", tag,
                   "--build-arg", f"BASE_IMAGE={spec['base']}"]
        for argument in spec["args"]:
            command.extend(("--build-arg", argument))
        command.append(str(spec["context"]))
        execute(command, check=True, text=True, capture_output=True)
        inspected = execute(["docker", "image", "inspect", "--format", "{{.Id}}", tag], check=True, text=True, capture_output=True)
        result[method] = {
            "tag": tag, "image_id": inspected.stdout.strip(), "base_resolved_digest": spec["base"],
            "package_version": spec["package_version"],
            "dockerfile_sha256": hashlib.sha256(spec["dockerfile"].read_bytes()).hexdigest(),
        }
    return result


def _validate_records(tasks: list[dict[str, Any]], records: list[dict[str, Any]]) -> None:
    expected = {(task["instance_id"], method) for task in tasks for method in METHODS}
    actual = [(record.get("instance_id"), record.get("method")) for record in records]
    if len(expected) != 15 or len(actual) != 15 or set(actual) != expected or len(set(actual)) != 15:
        raise ValueError("expected exactly 15 unique task/method records")


def finalize(*, tasks: list[dict[str, Any]], records: list[dict[str, Any]], output: pathlib.Path,
             provenance: dict[str, Any]) -> None:
    _validate_records(tasks, records)
    output.mkdir(parents=True, exist_ok=True)
    normalized = []
    predictions = []
    for record in records:
        item = dict(record)
        item.setdefault("patch", "")
        item.setdefault("error", None)
        item.setdefault("resolved", False)
        normalized.append(item)
        predictions.append({
            "instance_id": item["instance_id"],
            "model_name_or_path": item["method"],
            "model_patch": item["patch"],
        })
    (output / "records.json").write_text(json.dumps(normalized, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output / "predictions.jsonl").write_text(
        "".join(json.dumps(item, sort_keys=True) + "\n" for item in predictions), encoding="utf-8"
    )
    completed = sum(item["status"] == "evaluated" for item in normalized)
    resolved = sum(bool(item["resolved"]) for item in normalized)
    report = {"denominator": 15, "completed": completed, "resolved": resolved, "provenance": provenance}
    (output / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output / "report.md").write_text(
        "# SWE-bench Verified baseline\n\n"
        f"- Denominator: 15\n- Completed: {completed}\n- Resolved: {resolved}\n",
        encoding="utf-8",
    )


def execute_smoke(*, source: pathlib.Path, work: pathlib.Path, output: pathlib.Path,
                  config: Mapping[str, str]) -> None:
    validated = validate_config(config)
    selection = json.loads((source / "benchmarks" / "swe-bench-verified-50.json").read_text(encoding="utf-8"))["instance_ids"][:5]
    from datasets import load_dataset  # installed only on the disposable worker
    dataset = load_dataset(DATASET, split="test", revision=DATASET_REVISION)
    all_records = [dict(record) for record in dataset]
    tasks = materialize(records=all_records, selected_ids=selection, root=work)
    provenance = build_images(source=source, run_id=config["RUN_ID"], config=config)
    records = []
    for task in tasks:
        task_root = work / "tasks" / task["instance_id"]
        for method in METHODS:
            slot_output = output / "slots" / task["instance_id"] / method
            slot_output.mkdir(parents=True, exist_ok=True)
            record = run_agent(
                instance_id=task["instance_id"], method=method, image=provenance[method]["tag"],
                repo=task_root / method / "repo", task_input=task_root / "input", output=slot_output,
                model=validated["MODEL"], reasoning=validated["REASONING"],
            )
            record["model"] = validated["MODEL"]
            record["reasoning"] = validated["REASONING"]
            records.append(record)

    # Model credentials cease to exist before the evaluator can launch Docker.
    os.environ.pop("OPENAI_API_KEY", None)
    secret_file = os.environ.pop("OPENAI_SECRET_FILE", "")
    if secret_file:
        pathlib.Path(secret_file).unlink(missing_ok=True)

    official_root = output / "official"
    for method in METHODS:
        method_records = [record for record in records if record["method"] == method]
        prediction_file = official_root / method / "predictions.jsonl"
        prediction_file.parent.mkdir(parents=True, exist_ok=True)
        prediction_file.write_text("".join(json.dumps({
            "instance_id": record["instance_id"], "model_name_or_path": method, "model_patch": record["patch"],
        }, sort_keys=True) + "\n" for record in method_records), encoding="utf-8")
        try:
            run_official_evaluation(
                predictions=prediction_file, canonical_dataset=work / "canonical-dataset.json",
                instance_ids=selection, run_id=f"{config['RUN_ID']}-{method}", output=prediction_file.parent,
            )
            resolved_ids = load_resolved_ids(prediction_file.parent)
            for record in method_records:
                record["status"] = "evaluated"
                record["resolved"] = record["instance_id"] in resolved_ids
        except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
            for record in method_records:
                record["status"] = "evaluator-failed"
                record["error"] = str(error)
    provenance_payload = {
        "dataset": DATASET, "dataset_revision": DATASET_REVISION,
        "swebench_version": "4.1.0", "model": validated["MODEL"],
        "reasoning": validated["REASONING"], "images": provenance,
    }
    finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload)
    for record in records:
        slot = output / "slots" / record["instance_id"] / record["method"]
        (slot / "metadata.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--validate-records", type=pathlib.Path)
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--source", type=pathlib.Path)
    parser.add_argument("--work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    if args.validate_records:
        payload = json.loads(args.validate_records.read_text(encoding="utf-8"))
        _validate_records(payload["tasks"], payload["records"])
    elif args.run:
        if not args.source or not args.work or not args.output:
            parser.error("--run requires --source, --work, and --output")
        execute_smoke(source=args.source, work=args.work, output=args.output, config=os.environ)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
