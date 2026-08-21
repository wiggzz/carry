#!/usr/bin/env python3
"""Worker-side SWE-bench smoke orchestration and artifact validation."""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import ipaddress
import json
import math
import os
import pathlib
import re
import shutil
import subprocess
import time
from collections import Counter
from typing import Any, Mapping

HARNESSES = ("carry", "codex", "pi")


def selected_harnesses(values: Mapping[str, str]) -> tuple[str, ...]:
    harness = values.get("BENCHMARK_HARNESS", "carry")
    if harness not in HARNESSES:
        raise ValueError(f"BENCHMARK_HARNESS must be one of {', '.join(HARNESSES)}")
    return (harness,)


MODEL_PRICING_USD_PER_MILLION = {
    "gpt-5.6-luna": {
        "input": 0.20,
        "cached_input": 0.02,
        "cache_write_input": 0.25,
        "output": 1.20,
    },
}
USAGE_KEYS = (
    "input_tokens", "cached_input_tokens", "cache_write_input_tokens",
    "output_tokens", "reasoning_tokens", "total_tokens",
)


def empty_usage() -> dict[str, int]:
    return {key: 0 for key in USAGE_KEYS}


def _nonnegative_int(value: Any) -> int:
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else 0


def load_agent_usage(harness: str, output: pathlib.Path) -> dict[str, int]:
    """Normalize cumulative token usage emitted by each pinned harness."""
    usage = empty_usage()
    if harness == "carry":
        path = output / "result.json"
        if not path.is_file():
            return usage
        try:
            raw = json.loads(path.read_text(encoding="utf-8")).get("usage", {})
        except (OSError, json.JSONDecodeError):
            return usage
        if isinstance(raw, dict):
            return {key: _nonnegative_int(raw.get(key)) for key in USAGE_KEYS}
        return usage

    path = output / "trace.log"
    if not path.is_file():
        return usage
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return usage
    for line in lines:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        if harness == "codex" and event.get("type") == "turn.completed":
            raw = event.get("usage", {})
            if isinstance(raw, dict):
                usage = {
                    "input_tokens": _nonnegative_int(raw.get("input_tokens")),
                    "cached_input_tokens": _nonnegative_int(raw.get("cached_input_tokens")),
                    "cache_write_input_tokens": _nonnegative_int(raw.get("cache_write_input_tokens")),
                    "output_tokens": _nonnegative_int(raw.get("output_tokens")),
                    "reasoning_tokens": _nonnegative_int(raw.get("reasoning_output_tokens")),
                    "total_tokens": 0,
                }
                usage["total_tokens"] = usage["input_tokens"] + usage["output_tokens"]
        elif harness == "pi" and event.get("type") == "message_end":
            message = event.get("message", {})
            if not isinstance(message, dict) or message.get("role") != "assistant":
                continue
            raw = message.get("usage", {})
            if not isinstance(raw, dict):
                continue
            input_tokens = sum(_nonnegative_int(raw.get(key)) for key in ("input", "cacheRead", "cacheWrite"))
            additions = {
                "input_tokens": input_tokens,
                "cached_input_tokens": _nonnegative_int(raw.get("cacheRead")),
                "cache_write_input_tokens": _nonnegative_int(raw.get("cacheWrite")),
                "output_tokens": _nonnegative_int(raw.get("output")),
                "reasoning_tokens": _nonnegative_int(raw.get("reasoning")),
                "total_tokens": _nonnegative_int(raw.get("totalTokens")),
            }
            for key in USAGE_KEYS:
                usage[key] += additions[key]
    return usage


def pricing_for_model(model: str) -> dict[str, float] | None:
    pricing = MODEL_PRICING_USD_PER_MILLION.get(model)
    return dict(pricing) if pricing is not None else None


def estimate_cost_usd(usage: Mapping[str, int], pricing: Mapping[str, float] | None) -> float | None:
    if pricing is None:
        return None
    cached = min(usage["cached_input_tokens"], usage["input_tokens"])
    cache_write = min(
        usage["cache_write_input_tokens"], usage["input_tokens"] - cached,
    )
    ordinary = usage["input_tokens"] - cached - cache_write
    cost = (
        ordinary * pricing["input"]
        + cached * pricing["cached_input"]
        + cache_write * pricing["cache_write_input"]
        + usage["output_tokens"] * pricing["output"]
    ) / 1_000_000
    return round(cost, 6)


class ContainerCleanupError(RuntimeError):
    """A timed-out Docker workload could not be proven stopped."""


def docker_name_regex_literal(value: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", value):
        raise ContainerCleanupError("invalid Docker name fragment")
    # Docker's name filter uses Go regular expressions; unlike Python's
    # re.escape output, '-' must remain unescaped outside a character class.
    return value.replace(".", r"\.")


def force_remove_container(reference: str, *, exact_name: bool = False) -> None:
    """Remove one container and fail unless Docker proves it is absent."""
    name_literal = ""
    if exact_name:
        name_literal = docker_name_regex_literal(reference)
    if not exact_name and not re.fullmatch(r"[0-9a-f]{12,64}", reference):
        raise ContainerCleanupError("invalid container ID")
    filter_value = f"name=^/{name_literal}$" if exact_name else f"id={reference}"
    last_error = "container remained present"
    for attempt in range(3):
        removed = subprocess.run(
            ["docker", "rm", "--force", reference], check=False,
            capture_output=True, text=True, timeout=30,
        )
        listed = subprocess.run(
            ["docker", "ps", "--all", "--quiet", "--filter", filter_value],
            check=False, capture_output=True, text=True, timeout=30,
        )
        if listed.returncode == 0 and not listed.stdout.strip():
            return
        last_error = (
            f"docker rm status {removed.returncode}; "
            f"verification status {listed.returncode}"
        )
        if attempt < 2:
            time.sleep(0.2)
    raise ContainerCleanupError(f"could not prove container stopped: {last_error}")


DATASET = "princeton-nlp/SWE-bench_Verified"
DATASET_REVISION = "c104f840cc67f8b6eec6f759ebc8b2693d585d4a"
VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
DIGEST_IMAGE = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
CODEX_COMMAND = (
    "codex exec --dangerously-bypass-approvals-and-sandbox --model {model} "
    "--config model_reasoning_effort={reasoning} --json {prompt_text}"
)
PI_COMMAND = (
    "pi --mode json --provider openai-benchmark --model {model} "
    "--thinking {reasoning} --no-session {prompt_text}"
)


def official_phase_limits(values: Mapping[str, str] | None = None) -> dict[str, int]:
    source = values if values is not None else os.environ
    limits = {
        "worker_seconds": int(source.get("OFFICIAL_WORKER_SECONDS", "18000")),
        "agent_seconds": int(source.get("OFFICIAL_AGENT_PHASE_SECONDS", "11400")),
        "evaluation_seconds": int(source.get("OFFICIAL_EVALUATION_PHASE_SECONDS", "5400")),
        "setup_reserve_seconds": int(source.get("OFFICIAL_SETUP_RESERVE_SECONDS", "1200")),
    }
    if any(value <= 0 for value in limits.values()):
        raise ValueError("official phase limits must be positive")
    if (limits["agent_seconds"] + limits["evaluation_seconds"]
            + limits["setup_reserve_seconds"] != limits["worker_seconds"]):
        raise ValueError("official phase limits must exactly partition the worker budget")
    return limits


def validate_config(values: Mapping[str, str]) -> dict[str, str]:
    required = ("BASE_IMAGE", "CODEX_VERSION", "PI_VERSION", "MODEL", "REASONING")
    config = {key: values.get(key, "") for key in required}
    if not DIGEST_IMAGE.fullmatch(config["BASE_IMAGE"]):
        raise ValueError("BASE_IMAGE must use an immutable sha256 digest")
    for key in ("CODEX_VERSION", "PI_VERSION"):
        if not VERSION.fullmatch(config[key]):
            raise ValueError(f"{key} must be an exact package version")
    if config["PI_VERSION"] != "0.84.2":
        raise ValueError("PI_VERSION must be 0.84.2")

    if not config["MODEL"] or not config["REASONING"]:
        raise ValueError("model and reasoning configuration are required")
    return config


def start_agent_network(*, identity: str, proxy_image: str, proxy_script: pathlib.Path,
                        execute: Any = subprocess.run) -> dict[str, str]:
    digest = hashlib.sha256(identity.encode()).hexdigest()[:16]
    network = {
        "internal": f"carry-agent-internal-{digest}",
        "egress": f"carry-agent-egress-{digest}",
        "proxy": f"carry-openai-proxy-{digest}",
        "api_base": "http://openai-proxy:8080/v1",
    }
    try:
        execute(["docker", "network", "create", "--internal", network["internal"]], check=True)
        execute(["docker", "network", "create", network["egress"]], check=True)
        execute(
            [
                "docker", "run", "--detach", "--name", network["proxy"],
                "--network", network["egress"], "--read-only", "--cap-drop=ALL",
                "--security-opt", "no-new-privileges", "--tmpfs", "/tmp:rw,nosuid,nodev,size=16m",
                "--mount", f"type=bind,src={proxy_script.resolve()},dst=/proxy/openai_proxy.js,readonly",
                "--entrypoint", "node", proxy_image, "/proxy/openai_proxy.js",
            ],
            check=True,
        )
        execute(
            ["docker", "network", "connect", "--alias", "openai-proxy",
             network["internal"], network["proxy"]],
            check=True,
        )
        proxy_ip_result = execute(
            [
                "docker", "inspect", "--format",
                f"{{{{(index .NetworkSettings.Networks {json.dumps(network['internal'])}).IPAddress}}}}",
                network["proxy"],
            ],
            check=True, capture_output=True, text=True, timeout=30,
        )
        proxy_ip = proxy_ip_result.stdout.strip()
        try:
            if ipaddress.ip_address(proxy_ip).version != 4:
                raise ValueError("not IPv4")
        except ValueError as error:
            raise RuntimeError("OpenAI proxy has no valid internal IPv4 address") from error
        network["proxy_ip"] = proxy_ip
        isolated_network_args = [
            "--network", network["internal"], "--dns", "127.0.0.1",
            "--add-host", f"openai-proxy:{proxy_ip}",
        ]
        direct_probe_script = """
const net = require('node:net');
const dns = require('node:dns').promises;
function connects(host) {
  return new Promise(resolve => {
    const socket = net.connect({host, port: 443});
    socket.setTimeout(5000);
    socket.once('connect', () => { socket.destroy(); resolve(true); });
    socket.once('timeout', () => { socket.destroy(); resolve(false); });
    socket.once('error', () => resolve(false));
  });
}
Promise.all([
  fetch('https://github.com', {signal: AbortSignal.timeout(5000)}).then(() => true).catch(() => false),
  connects('1.1.1.1'),
  connects('2606:4700:4700::1111'),
  dns.resolve4('github.com').then(() => true).catch(() => false),
]).then(results => process.exit(results.some(Boolean) ? 42 : 0));
"""
        direct_probe = execute(
            [
                "docker", "run", "--rm", *isolated_network_args,
                "--entrypoint", "node", proxy_image, "-e", direct_probe_script,
            ],
            check=False, timeout=15,
        )
        if direct_probe.returncode != 0:
            raise RuntimeError("agent network permits direct internet access")
        health_script = (
            "fetch('http://openai-proxy:8080/healthz', {signal: AbortSignal.timeout(2000)})"
            ".then(r => process.exit(r.ok ? 0 : 1)).catch(() => process.exit(1))"
        )
        healthy = False
        for _ in range(10):
            health = execute(
                [
                    "docker", "run", "--rm", *isolated_network_args,
                    "--entrypoint", "node", proxy_image, "-e", health_script,
                ],
                check=False, timeout=10,
            )
            if health.returncode == 0:
                healthy = True
                break
            time.sleep(1)
        if not healthy:
            raise RuntimeError("OpenAI-only proxy did not become reachable")
        return network
    except Exception:
        cleanup_agent_network(network, execute=execute)
        raise


def cleanup_agent_network(network: Mapping[str, str], execute: Any = subprocess.run) -> None:
    leftovers: list[str] = []
    for attempt in range(3):
        execute(["docker", "rm", "--force", network["proxy"]], check=False, timeout=30)
        execute(["docker", "network", "rm", network["internal"]], check=False, timeout=30)
        execute(["docker", "network", "rm", network["egress"]], check=False, timeout=30)
        proxy = execute(
            ["docker", "inspect", "--type", "container", network["proxy"]],
            check=False, capture_output=True, text=True, timeout=30,
        )
        internal = execute(
            ["docker", "network", "inspect", network["internal"]],
            check=False, capture_output=True, text=True, timeout=30,
        )
        egress = execute(
            ["docker", "network", "inspect", network["egress"]],
            check=False, capture_output=True, text=True, timeout=30,
        )
        leftovers = []
        if proxy.returncode == 0:
            leftovers.append("proxy container remains")
        if internal.returncode == 0:
            leftovers.append("internal network remains")
        if egress.returncode == 0:
            leftovers.append("egress network remains")
        if not leftovers:
            return
        if attempt < 2:
            time.sleep(1)
    raise ContainerCleanupError("agent network cleanup failed: " + ", ".join(leftovers))


def agent_docker_command(*, image: str, harness: str, repo: pathlib.Path, task_input: pathlib.Path,
                         output: pathlib.Path, model: str, reasoning: str,
                         container_name: str, agent_timeout_seconds: int,
                         network: str, proxy_ip: str, api_base: str) -> list[str]:
    if harness not in HARNESSES:
        raise ValueError("unknown harness")
    return [
        "docker", "run", "--rm", "--name", container_name, "--stop-timeout", "10",
        "--network", network, "--dns", "127.0.0.1",
        "--add-host", f"openai-proxy:{proxy_ip}",
        "--read-only", "--cap-drop=ALL",
        "--security-opt", "no-new-privileges", "--env", "OPENAI_API_KEY",
        "--env", f"OPENAI_BASE_URL={api_base}",
        "--env", f"AGENT_TIMEOUT_SECONDS={agent_timeout_seconds}",
        "--env", "HOME=/agent-home", "--env", "XDG_CONFIG_HOME=/agent-home/.config",
        "--tmpfs", "/agent-home:rw,nosuid,nodev,size=256m",
        "--tmpfs", "/tmp:rw,nosuid,nodev,size=512m",
        "--mount", f"type=bind,src={repo.resolve()},dst=/workspace",
        "--mount", f"type=bind,src={task_input.resolve()},dst=/benchmark/input,readonly",
        "--mount", f"type=bind,src={output.resolve()},dst=/benchmark/output",
        "--workdir", "/workspace", image, "run", "--model", model,
        "--reasoning", reasoning, "--prompt", "/benchmark/input/task.md",
        "--output", "/benchmark/output",
    ]


def run_isolated_agent(*, instance_id: str, harness: str, image: str, proxy_image: str,
                       proxy_script: pathlib.Path, repo: pathlib.Path,
                       task_input: pathlib.Path, output: pathlib.Path, model: str, reasoning: str,
                       timeout_seconds: int | None = None,
                       pricing: Mapping[str, float] | None = None) -> dict[str, Any]:
    identity = f"{instance_id}\0{harness}\0{output.resolve()}"
    network = start_agent_network(
        identity=identity, proxy_image=proxy_image, proxy_script=proxy_script,
    )
    try:
        return run_agent(
            instance_id=instance_id, harness=harness, image=image,
            repo=repo, task_input=task_input, output=output,
            model=model, reasoning=reasoning, timeout_seconds=timeout_seconds,
            pricing=pricing, network=network["internal"], proxy_ip=network["proxy_ip"],
            api_base=network["api_base"],
        )
    finally:
        cleanup_agent_network(network)


def run_agent(*, instance_id: str, harness: str, image: str, repo: pathlib.Path,
              task_input: pathlib.Path, output: pathlib.Path, model: str, reasoning: str,
              network: str, proxy_ip: str, api_base: str, timeout_seconds: int | None = None,
              pricing: Mapping[str, float] | None = None) -> dict[str, Any]:
    slot_timeout = (
        timeout_seconds if timeout_seconds is not None
        else int(os.environ.get("AGENT_TIMEOUT_SECONDS", "1200"))
    )
    if slot_timeout < 1:
        raise ValueError("agent timeout must be positive")
    identity = f"{instance_id}\0{harness}\0{output.resolve()}".encode()
    container_name = f"carry-agent-{harness}-{hashlib.sha256(identity).hexdigest()[:16]}"
    in_container_timeout = max(1, slot_timeout - 45)
    command = agent_docker_command(
        image=image, harness=harness, repo=repo, task_input=task_input, output=output,
        model=model, reasoning=reasoning, container_name=container_name,
        agent_timeout_seconds=in_container_timeout, network=network,
        proxy_ip=proxy_ip, api_base=api_base,
    )
    started = time.monotonic()
    print("BENCHMARK_PROGRESS " + json.dumps({
        "instance_id": instance_id, "harness": harness, "state": "started",
    }, sort_keys=True), flush=True)
    try:
        subprocess.run(command, check=True, timeout=slot_timeout)
        patch_file = output / "final.patch"
        patch = patch_file.read_text(encoding="utf-8") if patch_file.is_file() else ""
        if not patch_file.is_file():
            raise RuntimeError("agent did not produce final.patch")
        response_retries = 0
        if harness == "carry":
            result_file = output / "result.json"
            if not result_file.is_file():
                raise RuntimeError("Carry did not produce result.json metrics")
            result = json.loads(result_file.read_text(encoding="utf-8"))
            response_retries = result.get("response_retries")
            if (isinstance(response_retries, bool) or not isinstance(response_retries, int)
                    or response_retries < 0):
                raise RuntimeError("Carry result.json has invalid response_retries")
        record = {"instance_id": instance_id, "harness": harness, "status": "agent-completed",
                  "patch": patch, "error": None, "attempts": 1, "retries": 0,
                  "response_retries": response_retries}
    except (OSError, RuntimeError, json.JSONDecodeError,
            subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        if isinstance(error, subprocess.TimeoutExpired):
            force_remove_container(container_name, exact_name=True)
        patch_file = output / "final.patch"
        patch = patch_file.read_text(encoding="utf-8") if patch_file.is_file() else ""
        record = {"instance_id": instance_id, "harness": harness, "status": "agent-failed",
                  "patch": patch, "error": str(error), "attempts": 1, "retries": 0,
                  "response_retries": 0,
                  "timed_out": isinstance(error, subprocess.TimeoutExpired)}
    record["elapsed_seconds"] = round(time.monotonic() - started, 3)
    record["usage"] = load_agent_usage(harness, output)
    record["estimated_cost_usd"] = estimate_cost_usd(record["usage"], pricing)
    print("BENCHMARK_PROGRESS " + json.dumps({
        "elapsed_seconds": record["elapsed_seconds"], "instance_id": instance_id,
        "harness": harness, "state": "completed", "status": record["status"],
    }, sort_keys=True), flush=True)
    return record


def cleanup_evaluator_containers(run_id: str) -> None:
    """Prove no evaluator container for one exact SWE-bench run remains."""
    listed = subprocess.run(
        ["docker", "ps", "--all", "--quiet", "--filter",
         f"name=\\.{docker_name_regex_literal(run_id)}$"],
        check=False, capture_output=True, text=True, timeout=30,
    )
    if listed.returncode != 0:
        raise ContainerCleanupError(
            f"could not enumerate evaluator containers: docker ps status {listed.returncode}"
        )
    container_ids = [
        value for value in listed.stdout.splitlines()
        if re.fullmatch(r"[0-9a-f]{12,64}", value)
    ]
    for container_id in container_ids:
        force_remove_container(container_id)


def run_official_evaluation(*, predictions: pathlib.Path, canonical_dataset: pathlib.Path,
                            instance_ids: list[str], run_id: str, output: pathlib.Path,
                            environment: Mapping[str, str] | None = None,
                            process_timeout_seconds: int | None = None,
                            max_workers: int = 5) -> None:
    if max_workers < 1 or max_workers > 5:
        raise ValueError("evaluator max_workers must be between 1 and 5")
    env = dict(environment if environment is not None else os.environ)
    for key in list(env):
        if key.startswith("OPENAI_"):
            del env[key]
    command = [
        os.environ.get("PYTHON", "python3"), "-m", "swebench.harness.run_evaluation",
        "--dataset_name", str(canonical_dataset),
        "--split", "test", "--predictions_path", str(predictions),
        "--run_id", run_id, "--report_dir", str(output),
        "--max_workers", str(max_workers),
        "--timeout", os.environ.get("EVALUATOR_TIMEOUT_SECONDS", "300"),
        # Every harness grades the same frozen task set on one disposable worker.
        # Keep per-instance images so later harnesses reuse the first harness's build.
        "--cache_level", "instance",
        "--instance_ids", *instance_ids,
    ]
    try:
        subprocess.run(
            command, check=False, env=env, cwd=output,
            timeout=process_timeout_seconds,
        )
    finally:
        # SWE-bench suppresses some cleanup failures. Verify exact-run absence
        # after every return path before another evaluator shard can launch.
        cleanup_evaluator_containers(run_id)


def load_official_report(report_dir: pathlib.Path) -> dict[str, set[str]]:
    for path in sorted(report_dir.rglob("*.json")):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(payload, dict):
            continue
        required = (
            "completed_ids", "resolved_ids", "unresolved_ids", "empty_patch_ids",
            "error_ids", "incomplete_ids",
        )
        if all(isinstance(payload.get(key), list) for key in required):
            return {key: set(payload[key]) for key in required}
    raise RuntimeError("official evaluator report did not contain outcome ID sets")


def load_resolved_ids(report_dir: pathlib.Path) -> set[str]:
    return load_official_report(report_dir)["resolved_ids"]


def selection_for_mode(frozen_ids: list[str], mode: str) -> list[str]:
    if len(frozen_ids) != 50 or len(set(frozen_ids)) != 50:
        raise ValueError("frozen official manifest must contain exactly 50 unique IDs")
    if mode == "smoke-5":
        return frozen_ids[:5]
    if mode == "official-50":
        return list(frozen_ids)
    raise ValueError(f"unsupported benchmark mode: {mode}")


def ordered_shards(instance_ids: list[str], shard_size: int) -> list[list[str]]:
    if shard_size < 1:
        raise ValueError("shard size must be positive")
    return [instance_ids[offset:offset + shard_size] for offset in range(0, len(instance_ids), shard_size)]


def validate_official_outcomes(outcomes: Mapping[str, set[str]], instance_ids: list[str]) -> None:
    expected = set(instance_ids)
    completed = outcomes["completed_ids"]
    resolved = outcomes["resolved_ids"]
    unresolved = outcomes["unresolved_ids"]
    empty_patch = outcomes["empty_patch_ids"]
    errors = outcomes["error_ids"]
    incomplete = outcomes["incomplete_ids"]
    successful = resolved | unresolved
    if resolved & unresolved or completed - errors != successful:
        raise ValueError("official resolved/unresolved sets do not partition successful completed IDs")
    terminal = (successful, empty_patch, errors, incomplete)
    if any(left & right for index, left in enumerate(terminal) for right in terminal[index + 1:]):
        raise ValueError("official terminal outcome sets overlap")
    if set().union(*terminal) != expected:
        raise ValueError("official terminal outcome sets do not cover the requested IDs")


def status_for_official_outcome(instance_id: str, outcomes: Mapping[str, set[str]]) -> str:
    if instance_id in outcomes["resolved_ids"] or instance_id in outcomes["unresolved_ids"]:
        return "evaluated"
    if instance_id in outcomes["empty_patch_ids"]:
        return "empty-patch"
    if instance_id in outcomes["error_ids"]:
        return "evaluation-error"
    return "evaluation-incomplete"


def apply_official_outcomes(
    records: list[dict[str, Any]], outcomes: Mapping[str, set[str]]
) -> None:
    """Apply grading only to slots whose agent process completed successfully."""
    for record in records:
        if record.get("status") != "agent-completed":
            continue
        instance_id = record["instance_id"]
        record["resolved"] = instance_id in outcomes["resolved_ids"]
        record["status"] = status_for_official_outcome(instance_id, outcomes)


def official_evaluation_unknowns(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return sorted(
        (record for record in records if record.get("status") not in {"evaluated", "empty-patch"}),
        key=lambda record: (record["instance_id"], record["harness"]),
    )


def require_complete_official_evaluations(records: list[dict[str, Any]]) -> None:
    unknown = official_evaluation_unknowns(records)
    if unknown:
        slots = ", ".join(
            f"{record['instance_id']}/{record['harness']}" for record in unknown
        )
        raise RuntimeError(
            f"official evaluation incomplete for {len(unknown)} slots: {slots}"
        )


def _clone(repo: str, commit: str, destination: pathlib.Path, mirror: pathlib.Path | None = None) -> None:
    if mirror is None:
        mirror = destination.parent / (destination.name + "-source.git")
        subprocess.run(
            ["git", "clone", "--quiet", "--mirror", f"https://github.com/{repo}.git", str(mirror)],
            check=True,
        )
    ref_suffix = hashlib.sha256(str(destination.resolve()).encode()).hexdigest()[:16]
    branch = f"carry-benchmark-base-{commit[:16]}-{ref_suffix}"
    ref = f"refs/heads/{branch}"
    subprocess.run(["git", "--git-dir", str(mirror), "update-ref", ref, commit], check=True)
    try:
        # --no-local forces upload-pack to send only objects reachable from the
        # temporary base ref instead of hard-linking the full local mirror.
        subprocess.run(
            [
                "git", "clone", "--quiet", "--no-local", "--no-tags", "--single-branch",
                "--branch", branch, str(mirror), str(destination),
            ],
            check=True,
        )
    finally:
        subprocess.run(
            ["git", "--git-dir", str(mirror), "update-ref", "-d", ref],
            check=True,
        )
    subprocess.run(["git", "-C", str(destination), "checkout", "--quiet", "--detach", commit], check=True)
    subprocess.run(
        ["git", "-C", str(destination), "branch", "--quiet", "--delete", "--force", branch],
        check=True,
    )
    subprocess.run(["git", "-C", str(destination), "remote", "remove", "origin"], check=True)
    subprocess.run(["git", "-C", str(destination), "reflog", "expire", "--expire=now", "--all"], check=True)
    subprocess.run(["git", "-C", str(destination), "gc", "--prune=now", "--quiet"], check=True)
    head = subprocess.run(
        ["git", "-C", str(destination), "rev-parse", "HEAD"],
        check=True, text=True, capture_output=True,
    ).stdout.strip()
    if head != commit:
        raise RuntimeError(f"task checkout HEAD {head} does not match base commit {commit}")
    git_dir = destination / ".git"
    forbidden_paths = (
        git_dir / "objects" / "info" / "alternates",
        git_dir / "info" / "grafts",
        git_dir / "shallow",
    )
    if any(path.exists() for path in forbidden_paths):
        raise RuntimeError("task checkout contains alternates, grafts, or a shallow boundary")
    refs = subprocess.run(
        ["git", "-C", str(destination), "for-each-ref", "--format=%(refname)"],
        check=True, text=True, capture_output=True,
    ).stdout.strip()
    if refs:
        raise RuntimeError("task checkout contains branch, tag, remote, or replace refs")
    remotes = subprocess.run(
        ["git", "-C", str(destination), "remote"],
        check=True, text=True, capture_output=True,
    ).stdout.strip()
    if remotes:
        raise RuntimeError("task checkout contains a configured remote")
    reflog = subprocess.run(
        ["git", "-C", str(destination), "reflog"],
        check=True, text=True, capture_output=True,
    ).stdout.strip()
    if reflog:
        raise RuntimeError("task checkout contains reflog entries")
    isolated_git_environment = {**os.environ, "GIT_NO_REPLACE_OBJECTS": "1"}
    ancestors = set(subprocess.run(
        ["git", "-C", str(destination), "rev-list", commit],
        check=True, text=True, capture_output=True, env=isolated_git_environment,
    ).stdout.splitlines())
    object_rows = subprocess.run(
        [
            "git", "-C", str(destination), "cat-file", "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype)",
        ],
        check=True, text=True, capture_output=True, env=isolated_git_environment,
    ).stdout.splitlines()
    stored_commits = {row.split()[0] for row in object_rows if row.endswith(" commit")}
    if stored_commits != ancestors:
        raise RuntimeError("task checkout object database is not exactly the base commit ancestry")
    unreachable = subprocess.run(
        [
            "git", "-C", str(destination), "fsck", "--connectivity-only",
            "--no-reflogs", "--unreachable", "--no-progress",
        ],
        check=True, text=True, capture_output=True,
    ).stdout.strip()
    if unreachable:
        raise RuntimeError("task checkout contains Git objects unreachable from the base commit")


def materialize(*, records: list[dict[str, Any]], selected_ids: list[str], root: pathlib.Path,
                clone: Any = None, harnesses: tuple[str, ...] = HARNESSES) -> list[dict[str, Any]]:
    by_id = {record.get("instance_id"): record for record in records}
    if (not selected_ids or len(selected_ids) > 50 or len(set(selected_ids)) != len(selected_ids)
            or any(instance_id not in by_id for instance_id in selected_ids)):
        raise ValueError("canonical dataset does not contain the requested unique selection")
    selected = [by_id[instance_id] for instance_id in selected_ids]
    root.mkdir(parents=True, exist_ok=True)
    clone_impl = clone
    if clone_impl is None:
        mirrors: dict[str, pathlib.Path] = {}

        def clone_one(repo: str, commit: str, destination: pathlib.Path) -> None:
            if repo not in mirrors:
                mirror = root / "repositories" / (repo.replace("/", "__") + ".git")
                mirror.parent.mkdir(parents=True, exist_ok=True)
                subprocess.run(
                    ["git", "clone", "--quiet", "--mirror", f"https://github.com/{repo}.git", str(mirror)],
                    check=True,
                )
                mirrors[repo] = mirror
            _clone(repo, commit, destination, mirrors[repo])
        clone_impl = clone_one
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
        (input_dir / "task.md").write_text(
            "Solve the issue below in the current repository. Work directly in the checkout, "
            "run relevant tests when practical, and do not commit your changes.\n\n"
            + record["problem_statement"] + "\n",
            encoding="utf-8",
        )
        for harness in harnesses:
            clone_impl(record["repo"], record["base_commit"], task_root / harness / "repo")
        tasks.append(public)
    return tasks


def build_images(*, source: pathlib.Path, run_id: str, config: Mapping[str, str],
                 execute: Any = subprocess.run,
                 harnesses: tuple[str, ...] = HARNESSES) -> dict[str, Any]:
    validated = validate_config(config)
    carry_base = config.get("CARRY_BASE_IMAGE", "")
    if not DIGEST_IMAGE.fullmatch(carry_base):
        raise ValueError("CARRY_BASE_IMAGE must use an immutable sha256 digest")
    harness_dir = source / "containers" / "swebench-harness"
    specifications = {
        "carry": {
            "dockerfile": source / "containers" / "swebench-harness" / "Dockerfile.carry",
            "context": source, "base": carry_base, "package_version": config.get("SOURCE_COMMIT", "current-source"),
            "args": [],
        },
        "codex": {
            "dockerfile": harness_dir / "Dockerfile.node", "context": harness_dir,
            "base": validated["BASE_IMAGE"], "package_version": validated["CODEX_VERSION"],
            "args": ["PACKAGE=@openai/codex", f"PACKAGE_VERSION={validated['CODEX_VERSION']}",
                     "AGENT_HARNESS=codex", f"AGENT_COMMAND={CODEX_COMMAND}"],
        },
        "pi": {
            "dockerfile": harness_dir / "Dockerfile.node", "context": harness_dir,
            "base": validated["BASE_IMAGE"], "package_version": validated["PI_VERSION"],
            "args": ["PACKAGE=@earendil-works/pi-coding-agent", f"PACKAGE_VERSION={validated['PI_VERSION']}",
                     "AGENT_HARNESS=pi", f"AGENT_COMMAND={PI_COMMAND}"],
        },
    }
    result = {}
    for harness in harnesses:
        spec = specifications[harness]
        tag = f"swebench-{run_id}-{harness}"
        command = ["docker", "build", "--pull", "--progress=plain", "--file", str(spec["dockerfile"]), "--tag", tag,
                   "--build-arg", f"BASE_IMAGE={spec['base']}"]
        for argument in spec["args"]:
            command.extend(("--build-arg", argument))
        command.append(str(spec["context"]))
        execute(command, check=True, text=True)
        inspected = execute(["docker", "image", "inspect", "--format", "{{.Id}}", tag], check=True, text=True, capture_output=True)
        result[harness] = {
            "tag": tag, "image_id": inspected.stdout.strip(), "base_resolved_digest": spec["base"],
            "package_version": spec["package_version"],
            "dockerfile_sha256": hashlib.sha256(spec["dockerfile"].read_bytes()).hexdigest(),
        }
    return result


def _validate_records(
    tasks: list[dict[str, Any]],
    records: list[dict[str, Any]],
    harnesses: tuple[str, ...] = HARNESSES,
) -> None:
    task_count = len(tasks)
    if task_count not in (5, 50):
        raise ValueError("benchmark must contain exactly 5 or 50 tasks")
    expected = {(task["instance_id"], harness) for task in tasks for harness in harnesses}
    actual = [(record.get("instance_id"), record.get("harness")) for record in records]
    expected_count = task_count * len(harnesses)
    if (len(expected) != expected_count or len(actual) != expected_count
            or set(actual) != expected or len(set(actual)) != expected_count):
        raise ValueError(f"expected exactly {expected_count} unique task/harness records")


def finalize(*, tasks: list[dict[str, Any]], records: list[dict[str, Any]], output: pathlib.Path,
             provenance: dict[str, Any], harnesses: tuple[str, ...] = HARNESSES) -> None:
    _validate_records(tasks, records, harnesses)
    output.mkdir(parents=True, exist_ok=True)
    normalized = []
    predictions = []
    for record in records:
        item = dict(record)
        item.setdefault("patch", "")
        item.setdefault("error", None)
        item.setdefault("resolved", False)
        item.setdefault("response_retries", 0)
        item.setdefault("elapsed_seconds", 0.0)
        item.setdefault("estimated_cost_usd", None)
        item.setdefault("usage", empty_usage())
        if (isinstance(item["response_retries"], bool)
                or not isinstance(item["response_retries"], int)
                or item["response_retries"] < 0):
            raise ValueError("response_retries must be a nonnegative integer")
        if (isinstance(item["elapsed_seconds"], bool)
                or not isinstance(item["elapsed_seconds"], (int, float))
                or item["elapsed_seconds"] < 0):
            raise ValueError("elapsed_seconds must be nonnegative")
        if (item["estimated_cost_usd"] is not None
                and (isinstance(item["estimated_cost_usd"], bool)
                     or not isinstance(item["estimated_cost_usd"], (int, float))
                     or item["estimated_cost_usd"] < 0)):
            raise ValueError("estimated_cost_usd must be null or nonnegative")
        if not isinstance(item["usage"], dict):
            raise ValueError("usage must be an object")
        item["usage"] = {key: _nonnegative_int(item["usage"].get(key)) for key in USAGE_KEYS}
        normalized.append(item)
        predictions.append({
            "instance_id": item["instance_id"],
            "model_name_or_path": item["harness"],
            "model_patch": item["patch"],
        })
    (output / "records.json").write_text(json.dumps(normalized, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output / "predictions.jsonl").write_text(
        "".join(json.dumps(item, sort_keys=True) + "\n" for item in predictions), encoding="utf-8"
    )
    completed = sum(item["status"] == "evaluated" for item in normalized)
    resolved = sum(bool(item["resolved"]) for item in normalized)
    task_count = len(tasks)
    denominator = task_count * len(harnesses)
    harness_reports = {}
    for harness in harnesses:
        harness_records = [item for item in normalized if item["harness"] == harness]
        costs = [item["estimated_cost_usd"] for item in harness_records
                 if item["estimated_cost_usd"] is not None]
        harness_reports[harness] = {
            "denominator": task_count,
            "completed": sum(item["status"] == "evaluated" for item in harness_records),
            "resolved": sum(bool(item["resolved"]) for item in harness_records),
            "response_retries": sum(item["response_retries"] for item in harness_records),
            "elapsed_seconds": round(sum(item["elapsed_seconds"] for item in harness_records), 3),
            "usage": {
                key: sum(item["usage"][key] for item in harness_records) for key in USAGE_KEYS
            },
            "estimated_cost_usd": round(sum(costs), 6) if costs else None,
            "costed_slots": len(costs),
            "statuses": dict(sorted(Counter(item["status"] for item in harness_records).items())),
        }
    report = {
        "denominator": denominator, "completed": completed, "resolved": resolved,
        "harnesses": harness_reports, "provenance": provenance,
    }
    (output / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    def cost_text(value: float | None) -> str:
        return "unavailable" if value is None else f"${value:.6f}"

    def harness_cost_text(values: dict[str, Any]) -> str:
        rendered = cost_text(values["estimated_cost_usd"])
        if values["estimated_cost_usd"] is not None and values["costed_slots"] < values["denominator"]:
            rendered += f" ({values['costed_slots']}/{values['denominator']} slots)"
        return rendered

    harness_lines = "\n".join(
        f"- {harness}: {values['resolved']}/{values['denominator']} resolved; "
        f"{values['completed']} completed; {values['denominator'] - values['completed']} failed/incomplete; "
        f"{values['response_retries']} response retries; "
        f"{values['elapsed_seconds']:.3f}s agent time; "
        f"{values['usage']['total_tokens']} tokens; {harness_cost_text(values)} estimated"
        for harness, values in harness_reports.items()
    )
    slot_lines = "\n".join(
        f"| {item['instance_id']} | {item['harness']} | {item['status']} | "
        f"{'yes' if item['resolved'] else 'no'} | {item['elapsed_seconds']:.3f} | "
        f"{item['usage']['total_tokens']} | {cost_text(item['estimated_cost_usd'])} |"
        for item in normalized
    )
    (output / "report.md").write_text(
        "# SWE-bench Verified baseline\n\n"
        f"- Denominator: {denominator}\n- Completed: {completed}\n- Resolved: {resolved}\n\n"
        + harness_lines
        + "\n\n## Agent runs\n\n"
        + "| Task | Agent | Status | Resolved | Agent seconds | Tokens | Estimated cost |\n"
        + "|---|---|---|---:|---:|---:|---:|\n"
        + slot_lines + "\n",
        encoding="utf-8",
    )


def execute_benchmark(*, source: pathlib.Path, work: pathlib.Path, output: pathlib.Path,
                      config: Mapping[str, str]) -> None:
    validated = validate_config(config)
    harnesses = selected_harnesses(config)
    pricing = pricing_for_model(validated["MODEL"])
    mode = config.get("BENCHMARK_MODE", "smoke-5")
    phase_limits = official_phase_limits(config) if mode == "official-50" else None
    frozen_ids = json.loads(
        (source / "benchmarks" / "swe-bench-verified-50.json").read_text(encoding="utf-8")
    )["instance_ids"]
    selection = selection_for_mode(frozen_ids, mode)
    agent_shard_size = 5 if mode == "smoke-5" else 10
    evaluator_shard_size = 5
    agent_shards = ordered_shards(selection, agent_shard_size)
    evaluator_shards = ordered_shards(selection, evaluator_shard_size)

    from datasets import load_dataset  # installed only on the disposable worker
    dataset = load_dataset(DATASET, split="test", revision=DATASET_REVISION)
    all_records = [dict(record) for record in dataset]
    by_id = {record.get("instance_id"): record for record in all_records}
    if any(instance_id not in by_id for instance_id in selection):
        raise ValueError("canonical dataset does not contain the frozen selection")
    selected_records = [by_id[instance_id] for instance_id in selection]
    work.mkdir(parents=True, exist_ok=True)
    (work / "canonical-dataset.json").write_text(
        json.dumps(selected_records, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    concurrency = int(config.get("AGENT_CONCURRENCY", "3"))
    if concurrency < 1 or concurrency > 5:
        raise ValueError("AGENT_CONCURRENCY must be between 1 and 5")
    agent_timeout = int(config.get("AGENT_TIMEOUT_SECONDS", "360"))
    evaluator_timeout = int(config.get("EVALUATOR_TIMEOUT_SECONDS", "300"))
    evaluator_concurrency = int(config.get("EVALUATOR_CONCURRENCY", "5"))
    if mode == "official-50" and (
            concurrency != 3 or agent_timeout != 360
            or evaluator_timeout != 300 or evaluator_concurrency != 5):
        raise ValueError("official mode requires fixed agent/evaluator timing and concurrency limits")

    provenance_payload = {
        "dataset": DATASET, "dataset_revision": DATASET_REVISION,
        "swebench_version": "4.1.0", "model": validated["MODEL"],
        "reasoning": validated["REASONING"], "images": {},
        "mode": mode, "harnesses": list(harnesses), "phase": "planned",
        "pricing_usd_per_million": pricing,
    }
    tasks = [{
        "instance_id": record["instance_id"], "repo": record["repo"],
        "base_commit": record["base_commit"], "problem_statement": record["problem_statement"],
    } for record in selected_records]
    records = [{
        "instance_id": task["instance_id"], "harness": harness,
        "status": "not-run", "patch": "", "error": "slot did not complete before checkpoint",
        "attempts": 0, "retries": 0, "response_retries": 0, "resolved": False,
        "model": validated["MODEL"], "reasoning": validated["REASONING"],
    } for task in tasks for harness in harnesses]
    records_by_slot = {(record["instance_id"], record["harness"]): record for record in records}
    # Persist the exact denominator before any model-bearing slot starts.
    finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload, harnesses=harnesses)

    provenance = build_images(
        source=source, run_id=config["RUN_ID"], config=config, harnesses=harnesses
    )
    execution_limits: dict[str, Any] = {
        "agent_timeout_seconds": agent_timeout,
        "agent_concurrency": concurrency,
        "agent_shard_size": agent_shard_size,
        "evaluator_timeout_seconds": evaluator_timeout,
        "evaluator_concurrency": evaluator_concurrency,
        "evaluator_shard_size": evaluator_shard_size,
    }
    if phase_limits is not None:
        execution_limits["phase_budgets"] = phase_limits
    provenance["execution_limits"] = execution_limits
    provenance_payload["images"] = provenance
    provenance_payload["phase"] = "agents"
    finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload, harnesses=harnesses)
    agent_deadline = (
        time.monotonic() + phase_limits["agent_seconds"]
        if phase_limits is not None else None
    )
    agent_budget_exhausted = False

    mirrors: dict[str, pathlib.Path] = {}

    def clone_one(repo: str, commit: str, destination: pathlib.Path) -> None:
        if repo not in mirrors:
            mirror = work / "repositories" / (repo.replace("/", "__") + ".git")
            mirror.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(
                ["git", "clone", "--quiet", "--mirror", f"https://github.com/{repo}.git", str(mirror)],
                check=True,
            )
            mirrors[repo] = mirror
        _clone(repo, commit, destination, mirrors[repo])

    for shard_index, shard_ids in enumerate(agent_shards):
        shard_root = work / "agent-shards" / f"{shard_index:02d}"
        shard_tasks = materialize(
            records=all_records, selected_ids=shard_ids, root=shard_root, clone=clone_one,
            harnesses=harnesses,
        )
        slots = []
        for task in shard_tasks:
            task_root = shard_root / "tasks" / task["instance_id"]
            for harness in harnesses:
                slot_output = output / "slots" / task["instance_id"] / harness
                slot_output.mkdir(parents=True, exist_ok=True)
                slots.append((task, harness, task_root, slot_output))

        def execute_slot(slot: tuple[Any, ...]) -> dict[str, Any]:
            task, harness, task_root, slot_output = slot
            slot_timeout = agent_timeout
            if agent_deadline is not None:
                remaining = math.ceil(agent_deadline - time.monotonic())
                if remaining <= 0:
                    return {
                        "instance_id": task["instance_id"], "harness": harness,
                        "status": "agent-budget-exhausted", "patch": "",
                        "error": "official agent phase budget exhausted before launch",
                        "attempts": 0, "retries": 0, "response_retries": 0,
                        "model": validated["MODEL"], "reasoning": validated["REASONING"],
                    }
                slot_timeout = min(slot_timeout, remaining)
            record = run_isolated_agent(
                instance_id=task["instance_id"], harness=harness,
                image=provenance[harness]["tag"], proxy_image=validated["BASE_IMAGE"],
                proxy_script=source / "scripts" / "openai_proxy.js",
                repo=task_root / harness / "repo",
                task_input=task_root / "input", output=slot_output,
                model=validated["MODEL"], reasoning=validated["REASONING"],
                timeout_seconds=slot_timeout, pricing=pricing,
            )
            record["model"] = validated["MODEL"]
            record["reasoning"] = validated["REASONING"]
            return record

        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
                for record in executor.map(execute_slot, slots):
                    records_by_slot[(record["instance_id"], record["harness"])].update(record)
                    agent_budget_exhausted |= (
                        record["status"] == "agent-budget-exhausted"
                        or bool(record.get("timed_out"))
                    )
            finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload, harnesses=harnesses)
        finally:
            # Agent workspaces are no longer needed after patch capture. Keeping only
            # mirrors and outputs bounds disk use before official evaluation starts.
            shutil.rmtree(shard_root, ignore_errors=True)
    shutil.rmtree(work / "agent-shards", ignore_errors=True)
    shutil.rmtree(work / "repositories", ignore_errors=True)

    # Model credentials cease to exist before the evaluator can launch Docker.
    os.environ.pop("OPENAI_API_KEY", None)
    secret_file = os.environ.pop("OPENAI_SECRET_FILE", "")
    if secret_file:
        pathlib.Path(secret_file).unlink(missing_ok=True)

    # Preserve the complete fixed-denominator checkpoint before slower grading.
    provenance_payload["phase"] = "grading"
    finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload, harnesses=harnesses)
    evaluation_deadline = (
        time.monotonic() + phase_limits["evaluation_seconds"]
        if phase_limits is not None else None
    )
    evaluation_budget_exhausted = False
    official_root = output / "official"
    for harness in harnesses:
        for shard_index, shard_ids in enumerate(evaluator_shards):
            report_dir = official_root / harness
            if len(evaluator_shards) > 1:
                report_dir = report_dir / f"shard-{shard_index:02d}"
            prediction_file = report_dir / "predictions.jsonl"
            prediction_file.parent.mkdir(parents=True, exist_ok=True)
            shard_records = [records_by_slot[(instance_id, harness)] for instance_id in shard_ids]
            prediction_file.write_text("".join(json.dumps({
                "instance_id": record["instance_id"], "model_name_or_path": harness,
                "model_patch": record["patch"],
            }, sort_keys=True) + "\n" for record in shard_records), encoding="utf-8")
            for record in shard_records:
                print("BENCHMARK_PROGRESS " + json.dumps({
                    "instance_id": record["instance_id"], "harness": harness, "state": "grading",
                }, sort_keys=True), flush=True)
            try:
                process_timeout = None
                if evaluation_deadline is not None:
                    remaining = math.ceil(evaluation_deadline - time.monotonic())
                    if remaining <= 0:
                        evaluation_budget_exhausted = True
                        raise TimeoutError("official evaluation phase budget exhausted before shard")
                    process_timeout = min(remaining, evaluator_timeout + 45)
                run_official_evaluation(
                    predictions=prediction_file, canonical_dataset=work / "canonical-dataset.json",
                    instance_ids=shard_ids,
                    run_id=f"{config['RUN_ID']}-{harness}-{shard_index:02d}", output=report_dir,
                    process_timeout_seconds=process_timeout,
                    max_workers=evaluator_concurrency,
                )
                outcomes = load_official_report(report_dir)
                validate_official_outcomes(outcomes, shard_ids)
                apply_official_outcomes(shard_records, outcomes)
            except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
                if isinstance(error, (subprocess.TimeoutExpired, ContainerCleanupError)):
                    evaluation_budget_exhausted = True
                for record in shard_records:
                    record["status"] = "evaluator-failed"
                    record["error"] = str(error)
            for record in shard_records:
                print("BENCHMARK_PROGRESS " + json.dumps({
                    "instance_id": record["instance_id"], "harness": harness,
                    "state": "graded", "status": record["status"],
                }, sort_keys=True), flush=True)
            finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload, harnesses=harnesses)
    evaluation_unknown = official_evaluation_unknowns(records)
    provenance_payload["phase"] = "incomplete" if evaluation_unknown else "complete"
    finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload, harnesses=harnesses)
    for record in records:
        slot = output / "slots" / record["instance_id"] / record["harness"]
        (slot / "metadata.json").write_text(
            json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    if agent_budget_exhausted or evaluation_budget_exhausted:
        exhausted = []
        if agent_budget_exhausted:
            exhausted.append("agent")
        if evaluation_budget_exhausted:
            exhausted.append("evaluation")
        raise RuntimeError(f"official {' and '.join(exhausted)} phase budget exhausted")
    require_complete_official_evaluations(records)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--validate-records", type=pathlib.Path)
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--source", type=pathlib.Path)
    parser.add_argument("--work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--harness", choices=HARNESSES, default="carry")
    args = parser.parse_args()
    if args.validate_records:
        payload = json.loads(args.validate_records.read_text(encoding="utf-8"))
        _validate_records(payload["tasks"], payload["records"])
    elif args.run:
        if not args.source or not args.work or not args.output:
            parser.error("--run requires --source, --work, and --output")
        config = dict(os.environ)
        config["BENCHMARK_HARNESS"] = args.harness
        execute_benchmark(source=args.source, work=args.work, output=args.output, config=config)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
