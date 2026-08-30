#!/usr/bin/env python3
"""Worker-side SWE-bench smoke orchestration and artifact validation."""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import importlib
import ipaddress
import json
import math
import os
import pathlib
import re
import shlex
import shutil
import subprocess
import time
from collections import Counter
from typing import Any, Mapping

HARNESSES = ("carry", "codex", "pi")


def selected_harnesses(values: Mapping[str, str]) -> tuple[str, ...]:
    harness = values.get("BENCHMARK_HARNESS", "carry")
    if harness == "all":
        return HARNESSES
    if harness not in HARNESSES:
        raise ValueError(f"BENCHMARK_HARNESS must be all or one of {', '.join(HARNESSES)}")
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


def load_proxy_round_input_tokens(proxy_container: str | None, *, execute: Any = subprocess.run) -> list[int]:
    """Read only provider-issued per-response inputs from isolated proxy logs."""
    if not proxy_container:
        return []
    try:
        result = execute(["docker", "logs", proxy_container], check=False, capture_output=True, text=True)
    except (OSError, subprocess.SubprocessError):
        return []
    values: list[int] = []
    for line in result.stdout.splitlines():
        prefix = "BENCHMARK_PROXY_USAGE "
        if not line.startswith(prefix):
            continue
        try:
            event = json.loads(line[len(prefix):])
        except json.JSONDecodeError:
            continue
        values.append(_nonnegative_int(event.get("input_tokens") if isinstance(event, dict) else None))
    return values


def max_observed_input_tokens(rounds: list[int]) -> int:
    return max(rounds, default=0)


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
TASK_IMAGE_CATALOG_VERSION = "swebench-4.1.0-prepared-v1"
VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
DIGEST_IMAGE = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
LOCAL_IMAGE_ID = re.compile(r"^sha256:[0-9a-f]{64}$")
REPO_DIGEST = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
CODEX_COMMAND = (
    "codex exec --dangerously-bypass-approvals-and-sandbox --model {model} "
    "--config model_reasoning_effort={reasoning} --json {prompt_text}"
)
PI_COMMAND = (
    "pi --mode json --provider openai-benchmark --model {model} "
    "--thinking {reasoning} --no-session {prompt_text}"
)


def prepared_image_recipe_sha256(source: pathlib.Path) -> str:
    """Hash every repository file copied into the reusable agent image."""
    relative_paths = (
        "containers/swebench-harness/Dockerfile.prepared",
        "containers/swebench-harness/prepared-entrypoint.sh",
        "containers/swebench-harness/apply-testbed-overlay.sh",
    )
    digest = hashlib.sha256()
    for relative in relative_paths:
        content = (source / relative).read_bytes()
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def task_image_cache_key(record: Mapping[str, Any], *,
                         prepared_dockerfile_sha256: str,
                         base_dockerfile_sha256: str) -> str:
    """Hash environment-affecting public inputs, never prompt/gold/evaluator data."""
    if not re.fullmatch(r"[0-9a-f]{64}", prepared_dockerfile_sha256):
        raise ValueError("prepared Dockerfile sha256 must be lowercase hexadecimal")
    if not re.fullmatch(r"[0-9a-f]{64}", base_dockerfile_sha256):
        raise ValueError("base Dockerfile sha256 must be lowercase hexadecimal")
    environment_record = {
        key: record.get(key)
        for key in ("instance_id", "repo", "version", "base_commit")
    }
    payload = {
        "catalog_version": TASK_IMAGE_CATALOG_VERSION,
        "dataset": DATASET,
        "dataset_revision": DATASET_REVISION,
        "swebench_version": "4.1.0",
        "platform": "linux/amd64",
        "prepared_dockerfile_sha256": prepared_dockerfile_sha256,
        "base_dockerfile_sha256": base_dockerfile_sha256,
        "record": environment_record,
    }
    canonical = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(canonical.encode()).hexdigest()


def task_catalog_payload(*, published: Mapping[str, Mapping[str, Any]],
                         repository: str, prepared_recipe_sha256: str,
                         base_recipe_sha256: str) -> dict[str, Any]:
    tasks = {
        instance_id: {
            "cache_key": item["cache_key"],
            "agent_digest": item["agent_image"]["resolved_digest"],
            "evaluator_digest": item["evaluator_image"]["resolved_digest"],
        }
        for instance_id, item in published.items()
    }
    return {
        "schema": "carry.swebench-task-catalog.v1",
        "catalog_version": TASK_IMAGE_CATALOG_VERSION,
        "dataset": DATASET,
        "dataset_revision": DATASET_REVISION,
        "swebench_version": "4.1.0",
        "platform": "linux/amd64",
        "repository": repository,
        "prepared_recipe_sha256": prepared_recipe_sha256,
        "base_recipe_sha256": base_recipe_sha256,
        "tasks": tasks,
    }


def validate_task_catalog(*, catalog: Mapping[str, Any], records: list[dict[str, Any]],
                          repository: str, prepared_recipe_sha256: str,
                          base_recipe_sha256: str) -> dict[str, Any]:
    expected_metadata = {
        "schema": "carry.swebench-task-catalog.v1",
        "catalog_version": TASK_IMAGE_CATALOG_VERSION,
        "dataset": DATASET,
        "dataset_revision": DATASET_REVISION,
        "swebench_version": "4.1.0",
        "platform": "linux/amd64",
        "repository": repository,
        "prepared_recipe_sha256": prepared_recipe_sha256,
        "base_recipe_sha256": base_recipe_sha256,
    }
    if any(catalog.get(key) != value for key, value in expected_metadata.items()):
        raise RuntimeError("task catalog metadata does not match reviewed benchmark inputs")
    tasks = catalog.get("tasks")
    expected_ids = {record["instance_id"] for record in records}
    if not isinstance(tasks, dict) or not expected_ids.issubset(tasks):
        raise RuntimeError("task catalog does not cover the fixed task selection")
    normalized: dict[str, Any] = dict(catalog)
    normalized_tasks: dict[str, dict[str, str]] = {}
    for record in records:
        instance_id = record["instance_id"]
        item = tasks[instance_id]
        if not isinstance(item, dict):
            raise RuntimeError(f"task catalog entry is invalid for {instance_id}")
        expected_key = task_image_cache_key(
            record, prepared_dockerfile_sha256=prepared_recipe_sha256,
            base_dockerfile_sha256=base_recipe_sha256,
        )
        if item.get("cache_key") != expected_key:
            raise RuntimeError(f"task catalog cache key mismatch for {instance_id}")
        agent = item.get("agent_digest", "")
        evaluator = item.get("evaluator_digest", "")
        if not DIGEST_IMAGE.fullmatch(agent) or not DIGEST_IMAGE.fullmatch(evaluator):
            raise RuntimeError(f"task catalog digest is invalid for {instance_id}")
        if not agent.startswith(repository + "@") or not evaluator.startswith(repository + "@"):
            raise RuntimeError(f"task catalog repository mismatch for {instance_id}")
        normalized_tasks[instance_id] = {
            "cache_key": expected_key,
            "agent_digest": agent,
            "evaluator_digest": evaluator,
        }
    normalized["tasks"] = normalized_tasks
    return normalized


def task_image_references(repository: str, cache_key: str) -> dict[str, str]:
    """Return immutable lookup tags for one evaluator/agent image pair."""
    final_component = repository.rsplit("/", 1)[-1]
    if (not repository or any(character.isspace() for character in repository)
            or "@" in repository or ":" in final_component):
        raise ValueError("TASK_IMAGE_REPOSITORY must be an untagged image repository")
    if not re.fullmatch(r"[0-9a-f]{64}", cache_key):
        raise ValueError("task image cache key must be lowercase sha256")
    return {
        "evaluator": f"{repository}:swebench-evaluator-{cache_key}",
        "agent": f"{repository}:swebench-ready-{cache_key}",
    }


def agent_concurrency_for_mode(values: Mapping[str, str], mode: str) -> int:
    """Return a bounded agent parallelism that fits the selected benchmark mode."""
    default = "5" if mode == "official-50" else "1" if mode in {"session-smoke-5", "session-20"} else "3"
    concurrency = int(values.get("AGENT_CONCURRENCY", default))
    if concurrency < 1 or concurrency > 5:
        raise ValueError("AGENT_CONCURRENCY must be between 1 and 5")
    if mode in {"session-smoke-5", "session-20"} and concurrency != 1:
        raise ValueError("retained-session modes require exactly one sequential native harness")
    return concurrency


def official_phase_limits(values: Mapping[str, str] | None = None) -> dict[str, int]:
    source = values if values is not None else os.environ
    limits = {
        "worker_seconds": int(source.get("OFFICIAL_WORKER_SECONDS", "18900")),
        "preparation_seconds": int(source.get("OFFICIAL_PREPARATION_PHASE_SECONDS", "3000")),
        "agent_seconds": int(source.get("OFFICIAL_AGENT_PHASE_SECONDS", "4500")),
        "evaluation_seconds": int(source.get("OFFICIAL_EVALUATION_PHASE_SECONDS", "10200")),
        "setup_reserve_seconds": int(source.get("OFFICIAL_SETUP_RESERVE_SECONDS", "1200")),
    }
    if any(value <= 0 for value in limits.values()):
        raise ValueError("official phase limits must be positive")
    if (limits["preparation_seconds"] + limits["agent_seconds"]
            + limits["evaluation_seconds"] + limits["setup_reserve_seconds"]
            != limits["worker_seconds"]):
        raise ValueError("official phase limits must exactly partition the worker budget")
    return limits


def validate_config(values: Mapping[str, str]) -> dict[str, str]:
    required = ("BASE_IMAGE", "CODEX_VERSION", "PI_VERSION", "MODEL", "REASONING")
    config = {key: values.get(key, "") for key in required}
    config["CARRY_COMPACTION_POLICY"] = values.get("CARRY_COMPACTION_POLICY", "economic")
    if config["CARRY_COMPACTION_POLICY"] not in {"economic", "disabled"}:
        raise ValueError("CARRY_COMPACTION_POLICY must be economic or disabled")
    context_pressure_threshold = values.get("CARRY_CONTEXT_PRESSURE_REMINDER_AT_TOKENS", "").strip()
    if context_pressure_threshold and (
        not context_pressure_threshold.isascii()
        or not context_pressure_threshold.isdecimal()
        or int(context_pressure_threshold) <= 0
    ):
        raise ValueError("CARRY_CONTEXT_PRESSURE_REMINDER_AT_TOKENS must be a positive integer or empty")
    config["CARRY_CONTEXT_PRESSURE_REMINDER_AT_TOKENS"] = context_pressure_threshold
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


def agent_docker_command(*, image: str, harness: str, repo: pathlib.Path,
                         harness_bundle: pathlib.Path, task_input: pathlib.Path,
                         output: pathlib.Path, model: str, reasoning: str,
                         container_name: str, agent_timeout_seconds: int,
                         network: str, proxy_ip: str, api_base: str,
                         resume_session: pathlib.Path | None = None,
                         codex_session: pathlib.Path | None = None,
                         codex_thread: str | None = None,
                         pi_session_dir: pathlib.Path | None = None) -> list[str]:
    if harness not in HARNESSES:
        raise ValueError("unknown harness")
    if resume_session is not None and harness != "carry":
        raise ValueError("only Carry supports a resumed session context")
    if (codex_session is not None or codex_thread is not None) and harness != "codex":
        raise ValueError("only Codex supports a durable native session")
    if codex_thread is not None and codex_session is None:
        raise ValueError("a Codex thread requires its durable session directory")
    if pi_session_dir is not None and harness != "pi":
        raise ValueError("only Pi supports a native session directory")
    command = [
        "docker", "run", "--rm", "--name", container_name, "--stop-timeout", "10",
        "--network", network, "--dns", "127.0.0.1",
        "--add-host", f"openai-proxy:{proxy_ip}",
        "--read-only", "--cap-drop=ALL",
        "--security-opt", "no-new-privileges", "--env", "OPENAI_API_KEY",
        "--env", f"OPENAI_BASE_URL={api_base}",
        "--env", f"AGENT_TIMEOUT_SECONDS={agent_timeout_seconds}",
        "--env", "CARRY_COMPACTION_POLICY",
        "--env", "CARRY_CONTEXT_PRESSURE_REMINDER_AT_TOKENS",
        "--env", "BENCHMARK_WORKSPACE=/testbed",
        "--env", "HOME=/agent-home", "--env", "XDG_CONFIG_HOME=/agent-home/.config",
        "--tmpfs", "/agent-home:rw,nosuid,nodev,size=256m",
        "--tmpfs", "/tmp:rw,nosuid,nodev,size=512m",
        "--mount", f"type=bind,src={repo.resolve()},dst=/testbed",
        "--mount", f"type=bind,src={harness_bundle.resolve()},dst=/opt/swebench-harness,readonly",
        "--mount", f"type=bind,src={task_input.resolve()},dst=/benchmark/input,readonly",
        "--mount", f"type=bind,src={output.resolve()},dst=/benchmark/output",
    ]
    if resume_session is not None:
        command.extend([
            "--mount",
            f"type=bind,src={resume_session.resolve()},dst=/benchmark/session,readonly",
        ])
    if codex_session is not None:
        command.extend([
            "--mount",
            f"type=bind,src={codex_session.resolve()},dst=/benchmark/codex-session",
        ])
    if pi_session_dir is not None:
        command.extend([
            "--mount",
            f"type=bind,src={pi_session_dir.resolve()},dst=/benchmark/pi-session",
        ])
    command.extend([
        "--workdir", "/testbed", image, "run", "--harness", harness,
        "--model", model,
        "--reasoning", reasoning, "--prompt", "/benchmark/input/task.md",
        "--output", "/benchmark/output",
    ])
    if resume_session is not None:
        command.extend(["--resume-session", "/benchmark/session"])
    if codex_session is not None:
        command.extend(["--codex-session", "/benchmark/codex-session"])
    if codex_thread is not None:
        command.extend(["--codex-thread", codex_thread])
    if pi_session_dir is not None:
        command.extend(["--pi-session-dir", "/benchmark/pi-session"])
    return command


def readiness_docker_command(*, image: str, container_name: str, repo: pathlib.Path,
                             script: str) -> list[str]:
    """Run a trusted public-test preflight without network or evaluator mounts."""
    return [
        "docker", "run", "--rm", "--name", container_name,
        "--network", "none", "--read-only", "--cap-drop=ALL",
        "--security-opt", "no-new-privileges",
        "--env", "HOME=/tmp/home",
        "--tmpfs", "/tmp:rw,nosuid,nodev,size=1g",
        "--mount", f"type=bind,src={repo.resolve()},dst=/testbed",
        "--workdir", "/testbed", "--entrypoint", "/bin/bash", image,
        "-lc", script,
    ]


def validate_readiness_result(*, returncode: int, timed_out: bool,
                              parsed_tests: Mapping[str, str]) -> dict[str, Any]:
    """Accept buggy baseline failures only after the official parser saw tests run."""
    if not parsed_tests:
        raise RuntimeError("readiness command did not execute any parseable public tests")
    return {
        "status": "ready",
        "baseline_exit_code": returncode,
        "timed_out_after_tests_started": timed_out,
        "parsed_test_count": len(parsed_tests),
        "parsed_statuses": dict(sorted(Counter(parsed_tests.values()).items())),
    }


def build_prepared_task_image(*, source: pathlib.Path, run_id: str, instance_id: str,
                              task_image_id: str, cache_key: str,
                              execute: Any = subprocess.run) -> dict[str, str]:
    """Create one harness-neutral, sanitized image from an official task image."""
    if not LOCAL_IMAGE_ID.fullmatch(task_image_id):
        raise ValueError("prepared task images require an immutable local sha256 parent ID")
    if not re.fullmatch(r"[0-9a-f]{64}", cache_key):
        raise ValueError("prepared task images require a sha256 cache key")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", instance_id):
        raise ValueError("invalid instance ID for prepared image tag")
    tag = f"swebench-{run_id}-prepared-{instance_id.lower()}"
    parent_tag = f"{tag}-parent-task-image"
    execute(["docker", "image", "tag", task_image_id, parent_tag], check=True)
    dockerfile = source / "containers" / "swebench-harness" / "Dockerfile.prepared"
    command = [
        "docker", "build", "--progress=plain", "--file", str(dockerfile), "--tag", tag,
        "--build-arg", f"TASK_IMAGE={parent_tag}",
        "--build-arg", f"TASK_IMAGE_ID={task_image_id}",
        "--build-arg", f"TASK_CACHE_KEY={cache_key}",
        str(source),
    ]
    execute(command, check=True, text=True)
    parent_inspected = execute(
        ["docker", "image", "inspect", "--format", "{{.Id}}", parent_tag],
        check=True, text=True, capture_output=True,
    ).stdout.strip()
    if parent_inspected != task_image_id:
        raise RuntimeError("prepared task parent image changed during build")
    inspected = execute(
        ["docker", "image", "inspect", "--format", "{{.Id}}", tag],
        check=True, text=True, capture_output=True,
    ).stdout.strip()
    if not LOCAL_IMAGE_ID.fullmatch(inspected):
        raise RuntimeError("prepared task image did not resolve to a sha256 image ID")
    return {
        "tag": tag,
        "image_id": inspected,
        "task_image_id": task_image_id,
        "cache_key": cache_key,
        "dockerfile_sha256": hashlib.sha256(dockerfile.read_bytes()).hexdigest(),
    }


def streamable_public_test_command(command: str) -> str:
    """Make public runners report bounded test outcomes during readiness."""
    tokens = shlex.split(command)
    pytest_command = (
        bool(tokens) and pathlib.PurePath(tokens[0]).name in {"pytest", "py.test"}
    ) or (
        len(tokens) >= 3 and pathlib.PurePath(tokens[0]).name.startswith("python")
        and tokens[1:3] == ["-m", "pytest"]
    )
    if pytest_command:
        if not any(token in {"-v", "-vv", "--verbose"} or token.startswith("--verbose=")
                   for token in tokens):
            tokens.append("-vv")
        if not any(token == "-x" or token.startswith("--maxfail") for token in tokens):
            tokens.append("--maxfail=1")
        return shlex.join(tokens)
    if any(token.lstrip("./") == "bin/test" for token in tokens):
        if not any(token == "--timeout" or token.startswith("--timeout=") for token in tokens):
            tokens.extend(("--timeout", "15"))
        if not any(token == "--split" or token.startswith("--split=") for token in tokens):
            tokens.extend(("--split", "1/500"))
        return shlex.join(tokens)
    return command


def trusted_readiness_script(test_spec: Any, *, public_test_command: str | None = None) -> tuple[str, str]:
    """Create a gold-free baseline command from the official public test path."""
    commands = list(test_spec.eval_script_list)
    try:
        start = commands.index(": '>>>>> Start Test Output'")
    except ValueError as error:
        raise RuntimeError("test spec has no official test-output marker") from error
    if start + 1 >= len(commands):
        raise RuntimeError("test spec has no public test command")
    test_command = streamable_public_test_command(
        public_test_command or commands[start + 1]
    )
    if not test_command.strip():
        raise RuntimeError("public readiness test command is empty")
    apply_indexes = [
        index for index, command in enumerate(commands[:start])
        if command.lstrip().startswith("git apply ")
    ]
    if len(apply_indexes) != 1:
        raise RuntimeError("test spec must contain exactly one hidden test-patch command")
    config_indexes = [
        index for index, command in enumerate(commands[:apply_indexes[0]])
        if command.strip().startswith("git config --global --add safe.directory ")
    ]
    if len(config_indexes) != 1:
        raise RuntimeError("test spec must delimit repository eval commands")
    # The official instance image already executed its repository installation
    # script. Re-running the later evaluator install could hide a broken prepared
    # layer and would require writes to the immutable environment.
    preamble = commands[:config_indexes[0]]
    lines = [
        "set -euo pipefail", "/usr/local/bin/apply-testbed-overlay", *preamble, "set +e",
    ]
    lines.extend((
        "printf '%s\\n' '>>>>> Start Test Output'",
        test_command,
        "readiness_status=$?",
        "printf '%s\\n' '>>>>> End Test Output'",
        "exit \"$readiness_status\"",
    ))
    script = "\n".join(lines) + "\n"
    if "git apply" in script:
        raise RuntimeError("hidden test patch leaked into readiness script")
    return script, test_command


def run_task_readiness(*, instance_id: str, image: str, repo: pathlib.Path,
                       script: str, test_command: str, parser: Any, test_spec: Any,
                       output: pathlib.Path, timeout_seconds: int) -> dict[str, Any]:
    """Run and parse a disposable baseline public-test check before model spend."""
    if timeout_seconds < 1:
        raise ValueError("readiness timeout must be positive")
    output.mkdir(parents=True, exist_ok=True)
    identity = hashlib.sha256(f"{instance_id}\0{image}\0{repo.resolve()}".encode()).hexdigest()[:16]
    container_name = f"carry-readiness-{identity}"
    command = readiness_docker_command(
        image=image, container_name=container_name, repo=repo, script=script,
    )
    timed_out = False
    try:
        process = subprocess.run(
            command, check=False, capture_output=True, text=True,
            timeout=timeout_seconds,
        )
        returncode = process.returncode
        captured = (process.stdout or "") + (process.stderr or "")
    except subprocess.TimeoutExpired as error:
        timed_out = True
        returncode = 124
        stdout = error.stdout.decode(errors="replace") if isinstance(error.stdout, bytes) else (error.stdout or "")
        stderr = error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else (error.stderr or "")
        captured = stdout + stderr
        force_remove_container(container_name, exact_name=True)
    (output / "test-output.txt").write_text(captured, encoding="utf-8")
    base_metadata = {
        "instance_id": instance_id,
        "baseline_exit_code": returncode,
        "timed_out_after_tests_started": timed_out,
        "test_command": test_command,
        "test_command_sha256": hashlib.sha256(test_command.encode()).hexdigest(),
        "prepared_image": image,
    }
    try:
        parsed_tests = parser(captured, test_spec)
        result = validate_readiness_result(
            returncode=returncode, timed_out=timed_out, parsed_tests=parsed_tests,
        )
    except Exception as error:
        failure = {**base_metadata, "status": "not-ready", "error": str(error)}
        (output / "metadata.json").write_text(
            json.dumps(failure, indent=2, sort_keys=True) + "\n", encoding="utf-8",
        )
        raise
    result.update(base_metadata)
    (output / "metadata.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8",
    )
    return result


def capture_dependency_manifest(*, image: str, output: pathlib.Path,
                                execute: Any = subprocess.run) -> dict[str, Any]:
    """Capture the exact installed conda package set from a prepared task image."""
    command = [
        "docker", "run", "--rm", "--network", "none", "--read-only",
        "--cap-drop=ALL", "--security-opt", "no-new-privileges",
        "--entrypoint", "/bin/bash", image, "-lc",
        "source /opt/miniconda3/bin/activate && conda activate testbed "
        "&& conda list --json",
    ]
    process = execute(command, check=True, capture_output=True, text=True, timeout=60)
    overlay = execute(
        [
            "docker", "run", "--rm", "--network", "none", "--read-only",
            "--cap-drop=ALL", "--security-opt", "no-new-privileges",
            "--entrypoint", "sha256sum", image,
            "/opt/swebench-prepared/testbed-overlay.tar",
        ],
        check=True, capture_output=True, text=True, timeout=60,
    )
    overlay_match = re.fullmatch(
        r"([0-9a-f]{64})\s+/opt/swebench-prepared/testbed-overlay\.tar\s*",
        overlay.stdout,
    )
    if overlay_match is None:
        raise RuntimeError("prepared build overlay did not produce a sha256 digest")
    packages = json.loads(process.stdout)
    if not isinstance(packages, list) or any(not isinstance(item, dict) for item in packages):
        raise RuntimeError("prepared dependency manifest is not a JSON package list")
    canonical = json.dumps(packages, sort_keys=True, separators=(",", ":"))
    payload = {
        "image": image,
        "package_count": len(packages),
        "sha256": hashlib.sha256(canonical.encode()).hexdigest(),
        "build_overlay_sha256": overlay_match.group(1),
        "packages": packages,
    }
    output.mkdir(parents=True, exist_ok=True)
    (output / "dependencies.json").write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8",
    )
    return {
        key: payload[key]
        for key in ("package_count", "sha256", "build_overlay_sha256")
    }


def enforce_https_swebench_base_images(templates: dict[str, str],
                                       trusted_ca_image: str) -> str:
    """Keep trusted dependency installation on the worker's HTTPS-only egress."""
    if not DIGEST_IMAGE.fullmatch(trusted_ca_image):
        raise ValueError("trusted CA image must use an immutable sha256 digest")
    template = templates.get("py", "")
    source_from = "FROM --platform={platform} ubuntu:{ubuntu_version}"
    ca_stage = f"FROM --platform={{platform}} {trusted_ca_image} AS trusted_certs"
    if ca_stage not in template:
        if template.count(source_from) != 1:
            raise RuntimeError("unexpected SWE-bench Python base image source")
        template = template.replace(source_from, f"{ca_stage}\n{source_from}", 1)
    ca_copy = "COPY --from=trusted_certs /etc/ssl/certs /etc/ssl/certs"
    if ca_copy not in template:
        env_marker = "ENV TZ=Etc/UTC"
        if template.count(env_marker) != 1:
            raise RuntimeError("unexpected SWE-bench Python base environment")
        template = template.replace(env_marker, f"{env_marker}\n{ca_copy}", 1)
    marker = "RUN sed -i 's|http://|https://|g' /etc/apt/sources.list && apt update"
    if marker not in template:
        needle = "RUN apt update"
        if template.count(needle) != 1:
            raise RuntimeError("unexpected SWE-bench Python base Dockerfile")
        template = template.replace(needle, marker, 1)
    templates["py"] = template
    return hashlib.sha256(template.encode()).hexdigest()


def publish_task_catalog_image(*, catalog: Mapping[str, Any], repository: str,
                               output: pathlib.Path,
                               execute: Any = subprocess.run) -> str:
    """Publish the frozen catalog as a tiny OCI image and return its digest reference."""
    canonical = json.dumps(catalog, indent=2, sort_keys=True) + "\n"
    catalog_hash = hashlib.sha256(canonical.encode()).hexdigest()
    context = output / "catalog-image"
    context.mkdir(parents=True, exist_ok=False)
    (context / "catalog.json").write_text(canonical, encoding="utf-8")
    (context / "Dockerfile").write_text(
        "FROM scratch\nCOPY catalog.json /catalog.json\nCMD [\"/catalog.json\"]\n",
        encoding="utf-8",
    )
    tag = f"{repository}:catalog-{catalog_hash}"
    execute(
        ["docker", "build", "--file", str(context / "Dockerfile"), "--tag", tag, str(context)],
        check=True, text=True,
    )
    execute(["docker", "push", tag], check=True, text=True)
    inspected = _inspect_catalog_image(tag, execute=execute)
    return inspected["resolved_digest"]


def load_task_catalog_image(*, reference: str, repository: str, output: pathlib.Path,
                            execute: Any = subprocess.run) -> dict[str, Any]:
    """Pull an immutable OCI catalog and extract its JSON without running it."""
    if not DIGEST_IMAGE.fullmatch(reference) or not reference.startswith(repository + "@"):
        raise ValueError("TASK_IMAGE_CATALOG must pin the configured repository by digest")
    execute(["docker", "pull", reference], check=True, text=True)
    name = "carry-task-catalog-" + reference.rsplit(":", 1)[-1][:16]
    execute(["docker", "create", "--name", name, reference], check=True, capture_output=True, text=True)
    output.mkdir(parents=True, exist_ok=True)
    destination = output / "catalog.json"
    try:
        execute(["docker", "cp", f"{name}:/catalog.json", str(destination)], check=True)
    finally:
        execute(["docker", "rm", "--force", name], check=True)
    try:
        payload = json.loads(destination.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError("task catalog image contains invalid JSON") from error
    if not isinstance(payload, dict):
        raise RuntimeError("task catalog image root must be an object")
    return payload


def _inspect_catalog_image(reference: str, *, execute: Any) -> dict[str, Any]:
    inspected = execute(
        ["docker", "image", "inspect", "--format", "{{json .}}", reference],
        check=True, capture_output=True, text=True,
    )
    try:
        payload = json.loads(inspected.stdout)
    except (TypeError, json.JSONDecodeError) as error:
        raise RuntimeError(f"catalog image {reference} produced invalid inspect data") from error
    image_id = payload.get("Id")
    if not isinstance(image_id, str) or not LOCAL_IMAGE_ID.fullmatch(image_id):
        raise RuntimeError(f"catalog image {reference} has no immutable local image ID")
    repository = reference.split("@", 1)[0] if "@" in reference else reference.rsplit(":", 1)[0]
    repo_digests = payload.get("RepoDigests") or []
    resolved = next(
        (value for value in repo_digests
         if isinstance(value, str) and value.startswith(repository + "@")
         and REPO_DIGEST.fullmatch(value)),
        None,
    )
    if resolved is None:
        raise RuntimeError(f"catalog image {reference} has no resolved repository digest")
    if "@" in reference and resolved != reference:
        raise RuntimeError(f"catalog image digest does not match requested reference: {reference}")
    labels = ((payload.get("Config") or {}).get("Labels") or {})
    if not isinstance(labels, dict):
        raise RuntimeError(f"catalog image {reference} has invalid labels")
    return {"tag": reference, "image_id": image_id, "resolved_digest": resolved, "labels": labels}


def resolve_task_environments(*, records: list[dict[str, Any]], source: pathlib.Path,
                              repository: str, output: pathlib.Path,
                              base_dockerfile_sha256: str | None = None,
                              dockerfile_templates: dict[str, str] | None = None,
                              trusted_ca_image: str | None = None,
                              max_workers: int = 5,
                              catalog_reference: str | None = None,
                              catalog: Mapping[str, Any] | None = None,
                              get_specs: Any = None,
                              execute: Any = subprocess.run) -> dict[str, dict[str, Any]]:
    """Pull and validate prepared catalog pairs without building any task image."""
    if not records or len({record["instance_id"] for record in records}) != len(records):
        raise ValueError("resolution requires unique task records")
    if base_dockerfile_sha256 is None:
        if dockerfile_templates is None:
            dockerfile_templates = importlib.import_module(
                "swebench.harness.dockerfiles"
            )._DOCKERFILE_BASE
        if trusted_ca_image is None:
            raise RuntimeError("resolver build recipe inputs were not initialized")
        base_dockerfile_sha256 = enforce_https_swebench_base_images(
            dockerfile_templates, trusted_ca_image,
        )
    if get_specs is None:
        get_specs = importlib.import_module(
            "swebench.harness.test_spec.test_spec"
        ).get_test_specs_from_dataset
    specs = list(get_specs(records))
    by_spec = {spec.instance_id: spec for spec in specs}
    expected = {record["instance_id"] for record in records}
    if set(by_spec) != expected:
        raise RuntimeError("catalog test specs do not match the fixed task denominator")
    dockerfile_hash = prepared_image_recipe_sha256(source)
    if catalog is None:
        if catalog_reference is None:
            raise RuntimeError("benchmark requires an immutable task catalog digest")
        catalog = load_task_catalog_image(
            reference=catalog_reference, repository=repository,
            output=output / "catalog", execute=execute,
        )
    catalog = validate_task_catalog(
        catalog=catalog, records=records, repository=repository,
        prepared_recipe_sha256=dockerfile_hash,
        base_recipe_sha256=base_dockerfile_sha256,
    )
    if max_workers < 1:
        raise ValueError("catalog pull concurrency must be positive")

    def resolve_one(record: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        instance_id = record["instance_id"]
        cache_key = task_image_cache_key(
            record, prepared_dockerfile_sha256=dockerfile_hash,
            base_dockerfile_sha256=base_dockerfile_sha256,
        )
        catalog_item = catalog["tasks"][instance_id]
        references = {
            "evaluator": catalog_item["evaluator_digest"],
            "agent": catalog_item["agent_digest"],
        }
        try:
            execute(["docker", "pull", references["evaluator"]], check=True, text=True)
            execute(["docker", "pull", references["agent"]], check=True, text=True)
            evaluator = _inspect_catalog_image(references["evaluator"], execute=execute)
            agent = _inspect_catalog_image(references["agent"], execute=execute)
        except (OSError, subprocess.CalledProcessError) as error:
            raise RuntimeError(f"required catalog images unavailable for {instance_id}") from error
        labels = agent.pop("labels")
        evaluator.pop("labels")
        if labels.get("org.carry.swebench.task-cache-key") != cache_key:
            raise RuntimeError(f"prepared image cache key mismatch for {instance_id}")
        if labels.get("org.carry.swebench.evaluator-image-id") != evaluator["image_id"]:
            raise RuntimeError(f"prepared image evaluator image identity mismatch for {instance_id}")
        official_key = by_spec[instance_id].instance_image_key
        execute(
            ["docker", "image", "tag", references["evaluator"], official_key], check=True,
        )
        retagged = execute(
            ["docker", "image", "inspect", "--format", "{{.Id}}", official_key],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
        if retagged != evaluator["image_id"]:
            raise RuntimeError(f"official evaluator retag mismatch for {instance_id}")
        return instance_id, {
            "cache_key": cache_key,
            "source_task_image": official_key,
            "evaluator_image": evaluator,
            "agent_image": agent,
            "dockerfile_sha256": dockerfile_hash,
            "base_dockerfile_sha256": base_dockerfile_sha256,
        }

    completed: dict[str, dict[str, Any]] = {}
    executor = concurrent.futures.ThreadPoolExecutor(max_workers=max_workers)
    futures = [executor.submit(resolve_one, record) for record in records]
    try:
        for instance_id, item in fail_fast_completion_order(futures):
            completed[instance_id] = item
    finally:
        executor.shutdown(wait=True, cancel_futures=True)
    resolved = {record["instance_id"]: completed[record["instance_id"]] for record in records}
    output.mkdir(parents=True, exist_ok=True)
    (output / "preparation.json").write_text(
        json.dumps(resolved, indent=2, sort_keys=True) + "\n", encoding="utf-8",
    )
    return resolved


def _catalog_pair(*, references: Mapping[str, str], cache_key: str,
                  execute: Any) -> tuple[dict[str, Any], dict[str, Any]]:
    for reference in (references["evaluator"], references["agent"]):
        execute(["docker", "pull", reference], check=True, text=True)
    evaluator = _inspect_catalog_image(references["evaluator"], execute=execute)
    agent = _inspect_catalog_image(references["agent"], execute=execute)
    labels = agent.pop("labels")
    evaluator.pop("labels")
    if labels.get("org.carry.swebench.task-cache-key") != cache_key:
        raise RuntimeError("prepared catalog image cache key mismatch")
    if labels.get("org.carry.swebench.evaluator-image-id") != evaluator["image_id"]:
        raise RuntimeError("prepared catalog image evaluator image identity mismatch")
    return evaluator, agent


def _remote_tag_exists(reference: str, *, execute: Any) -> bool:
    result = execute(
        ["docker", "manifest", "inspect", reference], check=False,
        capture_output=True, text=True,
    )
    return result.returncode == 0


def fail_fast_completion_order(futures: list[Any]) -> Any:
    """Yield future results as completed and cancel outstanding work on failure."""
    try:
        for future in concurrent.futures.as_completed(futures):
            yield future.result()
    except Exception:
        for future in futures:
            future.cancel()
        raise


def publish_task_environments(*, records: list[dict[str, Any]], source: pathlib.Path,
                              run_id: str, repository: str, work: pathlib.Path,
                              output: pathlib.Path, clone: Any,
                              timeout_seconds: int = 180, max_workers: int = 5,
                              client: Any = None, build_instances: Any = None,
                              get_specs: Any = None, parsers: Mapping[str, Any] | None = None,
                              repo_specs: Mapping[str, Any] | None = None,
                              dockerfile_templates: dict[str, str] | None = None,
                              trusted_ca_image: str | None = None,
                              base_dockerfile_sha256: str | None = None,
                              remote_exists: Any = None,
                              execute: Any = subprocess.run) -> dict[str, dict[str, Any]]:
    """Publish readiness-approved evaluator/agent pairs, building only cache misses."""
    if not records or len({record["instance_id"] for record in records}) != len(records):
        raise ValueError("publication requires unique task records")
    output.mkdir(parents=True, exist_ok=True)
    if base_dockerfile_sha256 is None:
        if dockerfile_templates is None:
            dockerfile_templates = importlib.import_module(
                "swebench.harness.dockerfiles"
            )._DOCKERFILE_BASE
        if trusted_ca_image is None:
            raise RuntimeError("publisher build recipe inputs were not initialized")
        base_dockerfile_sha256 = enforce_https_swebench_base_images(
            dockerfile_templates, trusted_ca_image,
        )
    if get_specs is None:
        get_specs = importlib.import_module(
            "swebench.harness.test_spec.test_spec"
        ).get_test_specs_from_dataset
    specs = list(get_specs(records))
    by_spec = {spec.instance_id: spec for spec in specs}
    expected = {record["instance_id"] for record in records}
    if set(by_spec) != expected:
        raise RuntimeError("publisher test specs do not match the fixed task denominator")
    dockerfile_hash = prepared_image_recipe_sha256(source)
    exists = remote_exists or (lambda reference: _remote_tag_exists(reference, execute=execute))
    catalog = {
        record["instance_id"]: {
            "cache_key": task_image_cache_key(
                record, prepared_dockerfile_sha256=dockerfile_hash,
                base_dockerfile_sha256=base_dockerfile_sha256,
            )
        }
        for record in records
    }
    for item in catalog.values():
        item["references"] = task_image_references(repository, item["cache_key"])

    published: dict[str, dict[str, Any]] = {}
    misses: list[dict[str, Any]] = []
    for record in records:
        instance_id = record["instance_id"]
        item = catalog[instance_id]
        references = item["references"]
        if not exists(references["agent"]):
            misses.append(record)
            continue
        if not exists(references["evaluator"]):
            raise RuntimeError(f"ready catalog image has no evaluator pair for {instance_id}")
        try:
            evaluator, agent = _catalog_pair(
                references=references, cache_key=item["cache_key"], execute=execute,
            )
        except (OSError, subprocess.CalledProcessError) as error:
            raise RuntimeError(f"cached catalog pair unavailable for {instance_id}") from error
        published[instance_id] = {
            "status": "cached", "cache_key": item["cache_key"],
            "source_task_image": by_spec[instance_id].instance_image_key,
            "evaluator_image": evaluator, "agent_image": agent,
            "dockerfile_sha256": dockerfile_hash,
        }

    if not misses:
        (output / "preparation.json").write_text(
            json.dumps(published, indent=2, sort_keys=True) + "\n", encoding="utf-8",
        )
        return published
    if client is None:
        client = importlib.import_module("docker").from_env()
    if build_instances is None:
        docker_build = importlib.import_module("swebench.harness.docker_build")
        build_log_root = (output / "build-logs").resolve()
        setattr(docker_build, "BASE_IMAGE_BUILD_DIR", build_log_root / "base")
        setattr(docker_build, "ENV_IMAGE_BUILD_DIR", build_log_root / "env")
        setattr(docker_build, "INSTANCE_IMAGE_BUILD_DIR", build_log_root / "instances")
        build_instances = docker_build.build_instance_images
    if parsers is None:
        parsers = importlib.import_module("swebench.harness.log_parsers").MAP_REPO_TO_PARSER
    if repo_specs is None:
        repo_specs = importlib.import_module(
            "swebench.harness.constants"
        ).MAP_REPO_VERSION_TO_SPECS
    _, failed = build_instances(
        client, misses, force_rebuild=False, max_workers=max_workers,
        tag="latest", env_image_tag="latest",
    )
    if failed:
        raise RuntimeError(f"dependency preparation failed for {len(failed)} task images")

    readiness_root = work / "readiness"
    staged: list[dict[str, Any]] = []
    for record in misses:
        instance_id = record["instance_id"]
        spec = by_spec[instance_id]
        source_image = client.images.get(spec.instance_image_key)
        item = catalog[instance_id]
        prepared = build_prepared_task_image(
            source=source, run_id=run_id, instance_id=instance_id,
            task_image_id=source_image.id, cache_key=item["cache_key"],
        )
        dependency = capture_dependency_manifest(
            image=prepared["tag"], output=output / instance_id,
        )
        task_root = readiness_root / instance_id
        clone(record["repo"], record["base_commit"], task_root / "repo")
        staged.append({
            "record": record, "spec": spec, "source_image": source_image,
            "prepared": prepared, "dependency": dependency, "task_root": task_root,
            "catalog": item,
        })

    def check_readiness(item: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
        record = item["record"]
        try:
            try:
                public_command = repo_specs[record["repo"]][record["version"]]["test_cmd"]
            except (KeyError, TypeError) as error:
                raise RuntimeError(
                    f"no ordinary public test command for {record['repo']} {record.get('version')}"
                ) from error
            script, test_command = trusted_readiness_script(
                item["spec"], public_test_command=public_command,
            )
            parser = parsers.get(record["repo"])
            if parser is None:
                raise RuntimeError(f"no official log parser for {record['repo']}")
            readiness = run_task_readiness(
                instance_id=record["instance_id"], image=item["prepared"]["tag"],
                repo=item["task_root"] / "repo", script=script,
                test_command=test_command, parser=parser, test_spec=record,
                output=output / record["instance_id"], timeout_seconds=timeout_seconds,
            )
            return item, readiness
        finally:
            shutil.rmtree(item["task_root"], ignore_errors=True)

    executor = concurrent.futures.ThreadPoolExecutor(max_workers=max_workers)
    futures = [executor.submit(check_readiness, item) for item in staged]
    try:
        for item, readiness in fail_fast_completion_order(futures):
            record = item["record"]
            instance_id = record["instance_id"]
            references = item["catalog"]["references"]
            execute(
                ["docker", "image", "tag", item["source_image"].id, references["evaluator"]],
                check=True,
            )
            execute(["docker", "push", references["evaluator"]], check=True, text=True)
            execute(
                ["docker", "image", "tag", item["prepared"]["tag"], references["agent"]],
                check=True,
            )
            # The readiness-approved tag is the immutable completion marker and is pushed last.
            execute(["docker", "push", references["agent"]], check=True, text=True)
            evaluator, agent = _catalog_pair(
                references=references, cache_key=item["catalog"]["cache_key"],
                execute=execute,
            )
            published[instance_id] = {
                "status": "published", "cache_key": item["catalog"]["cache_key"],
                "source_task_image": item["spec"].instance_image_key,
                "evaluator_image": evaluator, "agent_image": agent,
                "dockerfile_sha256": dockerfile_hash,
                "base_dockerfile_sha256": base_dockerfile_sha256,
                "dependency_manifest": item["dependency"], "readiness": readiness,
            }
    except Exception:
        for future in futures:
            future.cancel()
        raise
    finally:
        executor.shutdown(wait=True, cancel_futures=True)
        shutil.rmtree(readiness_root, ignore_errors=True)
    (output / "preparation.json").write_text(
        json.dumps(published, indent=2, sort_keys=True) + "\n", encoding="utf-8",
    )
    return published





def codex_thread_id(trace_path: pathlib.Path) -> str:
    """Extract one native Codex thread UUID from a JSONL execution trace."""
    thread_ids = set()
    for line in trace_path.read_text(encoding="utf-8").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") != "thread.started":
            continue
        thread_id = event.get("thread_id", event.get("threadId"))
        if isinstance(thread_id, str) and re.fullmatch(
            r"[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}",
            thread_id.lower(),
        ):
            thread_ids.add(thread_id.lower())
    if len(thread_ids) != 1:
        raise RuntimeError("Codex trace must contain exactly one native thread.started UUID")
    return thread_ids.pop()


def run_isolated_agent(*, instance_id: str, harness: str, image: str,
                       harness_bundle: pathlib.Path, proxy_image: str,
                       proxy_script: pathlib.Path, repo: pathlib.Path,
                       task_input: pathlib.Path, output: pathlib.Path, model: str, reasoning: str,
                       timeout_seconds: int | None = None,
                       pricing: Mapping[str, float] | None = None,
                       resume_session: pathlib.Path | None = None,
                       codex_session: pathlib.Path | None = None,
                       codex_thread: str | None = None,
                       pi_session_dir: pathlib.Path | None = None) -> dict[str, Any]:
    identity = f"{instance_id}\0{harness}\0{output.resolve()}"
    network = start_agent_network(
        identity=identity, proxy_image=proxy_image, proxy_script=proxy_script,
    )
    try:
        return run_agent(
            instance_id=instance_id, harness=harness, image=image,
            harness_bundle=harness_bundle,
            repo=repo, task_input=task_input, output=output,
            model=model, reasoning=reasoning, timeout_seconds=timeout_seconds,
            pricing=pricing, network=network["internal"], proxy_ip=network["proxy_ip"],
            proxy_container=network["proxy"], api_base=network["api_base"], resume_session=resume_session,
            codex_session=codex_session, codex_thread=codex_thread,
            pi_session_dir=pi_session_dir,
        )
    finally:
        cleanup_agent_network(network)


def run_agent(*, instance_id: str, harness: str, image: str, repo: pathlib.Path,
              harness_bundle: pathlib.Path, task_input: pathlib.Path,
              output: pathlib.Path, model: str, reasoning: str,
              network: str, proxy_ip: str, proxy_container: str | None = None, api_base: str = "",
              timeout_seconds: int | None = None,
              pricing: Mapping[str, float] | None = None,
              resume_session: pathlib.Path | None = None,
              codex_session: pathlib.Path | None = None,
              codex_thread: str | None = None,
              pi_session_dir: pathlib.Path | None = None) -> dict[str, Any]:
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
        image=image, harness=harness, repo=repo, harness_bundle=harness_bundle,
        task_input=task_input, output=output,
        model=model, reasoning=reasoning, container_name=container_name,
        agent_timeout_seconds=in_container_timeout, network=network,
        proxy_ip=proxy_ip, api_base=api_base, resume_session=resume_session,
        codex_session=codex_session, codex_thread=codex_thread,
        pi_session_dir=pi_session_dir,
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
    record["round_input_tokens"] = load_proxy_round_input_tokens(proxy_container)
    record["max_round_input_tokens"] = max_observed_input_tokens(record["round_input_tokens"])
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


def selection_for_mode(frozen_ids: list[str], mode: str,
                       smoke_ids: list[str] | None = None) -> list[str]:
    if len(frozen_ids) != 50 or len(set(frozen_ids)) != 50:
        raise ValueError("frozen official manifest must contain exactly 50 unique IDs")
    if mode in {"smoke-5", "session-smoke-5"}:
        if (smoke_ids is None or len(smoke_ids) != 5 or len(set(smoke_ids)) != 5
                or not set(smoke_ids).issubset(frozen_ids)):
            raise ValueError("smoke manifest must contain five unique frozen task IDs")
        return list(smoke_ids)
    if mode == "session-20":
        return list(frozen_ids[:20])
    if mode == "official-50":
        return list(frozen_ids)
    raise ValueError(f"unsupported benchmark mode: {mode}")


def validate_session_mode(mode: str, harnesses: tuple[str, ...]) -> None:
    if mode in {"session-smoke-5", "session-20"} and harnesses not in {("carry",), ("codex",), ("pi",)}:
        raise ValueError("retained-session modes require exactly one native harness")


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


def export_harness_bundles(harness_images: Mapping[str, Mapping[str, str]],
                           output: pathlib.Path,
                           execute: Any = subprocess.run) -> dict[str, pathlib.Path]:
    """Export each built harness once for read-only per-slot mounting."""
    if not harness_images or any(harness not in HARNESSES for harness in harness_images):
        raise ValueError("harness bundle export received unknown or empty image set")
    output.mkdir(parents=True, exist_ok=True)
    bundles: dict[str, pathlib.Path] = {}
    for harness, image in harness_images.items():
        image_id = image.get("image_id", "")
        if not LOCAL_IMAGE_ID.fullmatch(image_id):
            raise ValueError("harness bundle export requires immutable local image IDs")
        bundle = output / harness
        if bundle.exists():
            raise RuntimeError(f"harness bundle destination already exists: {bundle}")
        bundle.mkdir()
        container_name = f"carry-harness-export-{harness}-{image_id[7:19]}"
        execute(
            ["docker", "create", "--name", container_name, image_id],
            check=True, capture_output=True, text=True,
        )
        try:
            execute(
                ["docker", "cp", f"{container_name}:/opt/swebench-harness/.", str(bundle)],
                check=True,
            )
        finally:
            execute(["docker", "rm", "--force", container_name], check=True)
        if not (bundle / "bin" / "adapter").is_file():
            raise RuntimeError(f"exported {harness} harness bundle has no adapter")
        bundles[harness] = bundle.resolve()
    return bundles


def _validate_records(
    tasks: list[dict[str, Any]],
    records: list[dict[str, Any]],
    harnesses: tuple[str, ...] = HARNESSES,
) -> None:
    task_count = len(tasks)
    if task_count not in (5, 20, 50):
        raise ValueError("benchmark must contain exactly 5, 20, or 50 tasks")
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
        item.setdefault("round_input_tokens", [])
        item.setdefault("max_round_input_tokens", 0)
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
        if not isinstance(item["round_input_tokens"], list):
            raise ValueError("round_input_tokens must be a list")
        item["round_input_tokens"] = [_nonnegative_int(value) for value in item["round_input_tokens"]]
        item["max_round_input_tokens"] = max_observed_input_tokens(item["round_input_tokens"])
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
            "max_round_input_tokens": max(
                (item["max_round_input_tokens"] for item in harness_records), default=0,
            ),
            "observed_input_token_decreases": sum(
                later < earlier for item in harness_records
                for earlier, later in zip(item["round_input_tokens"], item["round_input_tokens"][1:])
            ),
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


def execute_preparation(*, source: pathlib.Path, work: pathlib.Path, output: pathlib.Path,
                        config: Mapping[str, str]) -> None:
    """Build/readiness-check only missing frozen task images and publish them."""
    if os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENAI_SECRET_FILE"):
        raise RuntimeError("task-image preparation must not receive model credentials")
    validated = validate_config(config)
    repository = config.get("TASK_IMAGE_REPOSITORY", "")
    task_image_references(repository, "0" * 64)
    if config.get("BENCHMARK_MODE") != "prepare-50":
        raise ValueError("image publisher requires BENCHMARK_MODE=prepare-50")
    frozen_ids = json.loads(
        (source / "benchmarks" / "swe-bench-verified-50.json").read_text(encoding="utf-8")
    )["instance_ids"]
    selection_for_mode(frozen_ids, "official-50")

    from datasets import load_dataset  # installed only on the disposable worker
    dataset = load_dataset(DATASET, split="test", revision=DATASET_REVISION)
    by_id = {record.get("instance_id"): dict(record) for record in dataset}
    if any(instance_id not in by_id for instance_id in frozen_ids):
        raise ValueError("canonical dataset does not contain the frozen preparation set")
    selected_records = [by_id[instance_id] for instance_id in frozen_ids]
    work.mkdir(parents=True, exist_ok=True)
    output.mkdir(parents=True, exist_ok=True)
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

    started = time.monotonic()
    dockerfile_templates = importlib.import_module(
        "swebench.harness.dockerfiles"
    )._DOCKERFILE_BASE
    base_recipe_sha256 = enforce_https_swebench_base_images(
        dockerfile_templates, validated["BASE_IMAGE"],
    )
    prepared_recipe_sha256 = prepared_image_recipe_sha256(source)
    try:
        published = publish_task_environments(
            records=selected_records, source=source, run_id=config["RUN_ID"],
            repository=repository, work=work, output=output / "preparation",
            clone=clone_one, timeout_seconds=int(config.get("READINESS_TIMEOUT_SECONDS", "180")),
            max_workers=int(config.get("READINESS_CONCURRENCY", "5")),
            trusted_ca_image=validated["BASE_IMAGE"],
            base_dockerfile_sha256=base_recipe_sha256,
        )
    finally:
        shutil.rmtree(work / "repositories", ignore_errors=True)
        shutil.rmtree(work / "readiness", ignore_errors=True)
    if set(published) != set(frozen_ids):
        raise RuntimeError("publisher did not return the exact frozen task denominator")
    catalog = task_catalog_payload(
        published=published, repository=repository,
        prepared_recipe_sha256=prepared_recipe_sha256,
        base_recipe_sha256=base_recipe_sha256,
    )
    catalog = validate_task_catalog(
        catalog=catalog, records=selected_records, repository=repository,
        prepared_recipe_sha256=prepared_recipe_sha256,
        base_recipe_sha256=base_recipe_sha256,
    )
    catalog_reference = publish_task_catalog_image(
        catalog=catalog, repository=repository, output=output,
    )
    report = {
        "schema": "carry.swebench-task-catalog.v1",
        "phase": "complete",
        "dataset": DATASET,
        "dataset_revision": DATASET_REVISION,
        "repository": repository,
        "catalog_reference": catalog_reference,
        "denominator": len(frozen_ids),
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "status_counts": dict(sorted(Counter(item["status"] for item in published.values()).items())),
        "tasks": {
            instance_id: {
                "cache_key": item["cache_key"],
                "status": item["status"],
                "agent_digest": item["agent_image"]["resolved_digest"],
                "evaluator_digest": item["evaluator_image"]["resolved_digest"],
            }
            for instance_id, item in published.items()
        },
    }
    (output / "preparation-report.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8",
    )


def execute_benchmark(*, source: pathlib.Path, work: pathlib.Path, output: pathlib.Path,
                      config: Mapping[str, str]) -> None:
    validated = validate_config(config)
    harnesses = selected_harnesses(config)
    repository = config.get("TASK_IMAGE_REPOSITORY", "")
    task_image_references(repository, "0" * 64)
    catalog_reference = config.get("TASK_IMAGE_CATALOG", "")
    if not DIGEST_IMAGE.fullmatch(catalog_reference) or not catalog_reference.startswith(repository + "@"):
        raise ValueError("TASK_IMAGE_CATALOG must pin the configured repository by digest")
    pricing = pricing_for_model(validated["MODEL"])
    mode = config.get("BENCHMARK_MODE", "smoke-5")
    validate_session_mode(mode, harnesses)
    phase_limits = official_phase_limits(config) if mode == "official-50" else None
    frozen_ids = json.loads(
        (source / "benchmarks" / "swe-bench-verified-50.json").read_text(encoding="utf-8")
    )["instance_ids"]
    smoke_ids = None
    if mode in {"smoke-5", "session-smoke-5"}:
        smoke_ids = json.loads(
            (source / "benchmarks" / "swe-bench-verified-smoke-5.json").read_text(encoding="utf-8")
        )["instance_ids"]
    selection = selection_for_mode(frozen_ids, mode, smoke_ids)
    agent_shard_size = 5 if mode in {"smoke-5", "session-smoke-5"} else 10
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
    session_source: pathlib.Path | None = None
    codex_session: pathlib.Path | None = None
    codex_thread: str | None = None
    pi_session_dir: pathlib.Path | None = None
    pi_session_file: pathlib.Path | None = None
    if mode in {"session-smoke-5", "session-20"}:
        session_root = work / "session"
        session_root.mkdir(parents=True, exist_ok=True)
        if harnesses == ("codex",):
            codex_session = session_root / "codex"
            codex_session.mkdir()
        elif harnesses == ("pi",):
            pi_session_dir = session_root / "pi"
            pi_session_dir.mkdir()
            pi_session_file = pi_session_dir / "session.jsonl"
            if pi_session_file.exists():
                raise RuntimeError("Pi session storage must be empty before the benchmark starts")
    (work / "canonical-dataset.json").write_text(
        json.dumps(selected_records, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    concurrency = agent_concurrency_for_mode(config, mode)
    agent_timeout = int(config.get("AGENT_TIMEOUT_SECONDS", "360"))
    readiness_timeout = int(config.get("READINESS_TIMEOUT_SECONDS", "180"))
    readiness_concurrency = int(config.get("READINESS_CONCURRENCY", "5"))
    evaluator_timeout = int(config.get("EVALUATOR_TIMEOUT_SECONDS", "270"))
    evaluator_concurrency = int(config.get("EVALUATOR_CONCURRENCY", "5"))
    if mode == "official-50" and (
            concurrency != 5 or agent_timeout != 360
            or readiness_timeout != 180 or readiness_concurrency != 5
            or evaluator_timeout != 270 or evaluator_concurrency != 5):
        raise ValueError("official mode requires fixed preparation, agent, and evaluator limits")

    provenance_payload = {
        "dataset": DATASET, "dataset_revision": DATASET_REVISION,
        "swebench_version": "4.1.0", "model": validated["MODEL"],
        "reasoning": validated["REASONING"],
        "carry_compaction_policy": validated["CARRY_COMPACTION_POLICY"],
        "carry_context_pressure_reminder_at_tokens": validated[
            "CARRY_CONTEXT_PRESSURE_REMINDER_AT_TOKENS"
        ] or None,
        "images": {},
        "mode": mode, "harnesses": list(harnesses), "phase": "planned",
        "pricing_usd_per_million": pricing,
    }
    if mode in {"session-smoke-5", "session-20"}:
        provenance_payload.update({
            "retained_context": True,
            "session_id": f"{config['RUN_ID']}:{harnesses[0]}",
            "task_order": selection,
            "task_workspace_isolation": True,
            "evaluator_isolation": True,
        })
        if harnesses == ("pi",):
            provenance_payload.update({
                "session_file": "session.jsonl",
                "session_storage": "worker-local",
            })
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

    # Build each reviewed harness once, export it as a read-only bundle, then
    # pull every selected task/evaluator image before the first model call.
    provenance = build_images(
        source=source, run_id=config["RUN_ID"], config=config, harnesses=HARNESSES
    )
    harness_bundles = export_harness_bundles(
        provenance, work / "harness-bundles",
    )
    execution_limits: dict[str, Any] = {
        "catalog_pull_timeout_seconds": readiness_timeout,
        "catalog_pull_concurrency": readiness_concurrency,
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
    provenance_payload["phase"] = "resolving-task-images"
    finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload, harnesses=harnesses)
    preparation_started = time.monotonic()
    try:
        prepared = resolve_task_environments(
            records=selected_records, source=source,
            repository=repository,
            output=output / "preparation",
            trusted_ca_image=validated["BASE_IMAGE"],
            max_workers=readiness_concurrency,
            catalog_reference=catalog_reference,
        )
        preparation_elapsed = time.monotonic() - preparation_started
        if phase_limits is not None and preparation_elapsed > phase_limits["preparation_seconds"]:
            raise TimeoutError("official task-image resolution budget exhausted before model launch")
    except Exception:
        provenance_payload["phase"] = "preparation-failed"
        finalize(tasks=tasks, records=records, output=output,
                 provenance=provenance_payload, harnesses=harnesses)
        shutil.rmtree(work / "repositories", ignore_errors=True)
        os.environ.pop("OPENAI_API_KEY", None)
        secret_file = os.environ.pop("OPENAI_SECRET_FILE", "")
        if secret_file:
            pathlib.Path(secret_file).unlink(missing_ok=True)
        raise
    provenance_payload["preparation_elapsed_seconds"] = round(preparation_elapsed, 3)
    provenance_payload["task_catalog_reference"] = catalog_reference
    provenance_payload["prepared_tasks"] = {
        instance_id: {
            "cache_key": item["cache_key"],
            "source_task_image": item["source_task_image"],
            "agent_image": item["agent_image"],
            "evaluator_image": item["evaluator_image"],
            "dockerfile_sha256": item["dockerfile_sha256"],
        }
        for instance_id, item in prepared.items()
    }
    provenance_payload["phase"] = "agents"
    finalize(tasks=tasks, records=records, output=output, provenance=provenance_payload, harnesses=harnesses)
    agent_deadline = (
        time.monotonic() + phase_limits["agent_seconds"]
        if phase_limits is not None else None
    )
    agent_budget_exhausted = False

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
            nonlocal session_source, codex_thread
            task, harness, task_root, slot_output = slot
            session_position = selection.index(task["instance_id"]) + 1 if mode in {"session-smoke-5", "session-20"} else None
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
            if session_position is not None:
                if session_position > 1 and (
                    (harness == "carry" and session_source is None)
                    or (harness == "codex" and codex_thread is None)
                    or (harness == "pi" and (pi_session_file is None or not pi_session_file.is_file()))
                ):
                    return {
                        "instance_id": task["instance_id"], "harness": harness,
                        "status": "agent-session-context-missing", "patch": "",
                        "error": "retained native source session is unavailable before this task",
                        "attempts": 0, "retries": 0, "response_retries": 0,
                        "model": validated["MODEL"], "reasoning": validated["REASONING"],
                        "session_position": session_position,
                    }
                prompt_path = task_root / "input" / "task.md"
                if prompt_path.is_file():
                    task_prompt = prompt_path.read_text(encoding="utf-8")
                    prompt_path.write_text(
                        "# New independent benchmark task\n\n"
                        "This task has a fresh repository and workspace. Earlier conversation "
                        "context is retained only as historical context: do not reuse prior task "
                        "paths, patches, commands, or conclusions. Work only in the current "
                        "`/testbed` workspace and solve the task below.\n\n"
                        + task_prompt,
                        encoding="utf-8",
                    )
            source_state = None
            if session_position is not None and session_source is not None:
                source_state = session_source / "context-state.json"
                if not source_state.is_file():
                    raise RuntimeError("retained Carry source session has no context-state.json")
            source_pi_session_sha256 = None
            if (session_position is not None and harness == "pi"
                    and pi_session_file is not None and pi_session_file.is_file()):
                source_pi_session_sha256 = hashlib.sha256(pi_session_file.read_bytes()).hexdigest()
            record = run_isolated_agent(
                instance_id=task["instance_id"], harness=harness,
                image=prepared[task["instance_id"]]["agent_image"]["tag"],
                harness_bundle=harness_bundles[harness],
                proxy_image=validated["BASE_IMAGE"],
                proxy_script=source / "scripts" / "openai_proxy.js",
                repo=task_root / harness / "repo",
                task_input=task_root / "input", output=slot_output,
                model=validated["MODEL"], reasoning=validated["REASONING"],
                timeout_seconds=slot_timeout, pricing=pricing,
                resume_session=session_source if harness == "carry" else None,
                codex_session=codex_session if harness == "codex" else None,
                codex_thread=codex_thread if harness == "codex" else None,
                pi_session_dir=pi_session_dir if session_position is not None and harness == "pi" else None,
            )
            record["model"] = validated["MODEL"]
            record["reasoning"] = validated["REASONING"]
            if session_position is not None:
                record["session_position"] = session_position
                if harness == "carry":
                    if source_state is not None:
                        record["source_session_state_sha256"] = hashlib.sha256(source_state.read_bytes()).hexdigest()
                    if record["status"] == "agent-completed":
                        state = slot_output / "context-state.json"
                        if not state.is_file():
                            raise RuntimeError("completed Carry slot has no context-state.json")
                        record["session_state_sha256"] = hashlib.sha256(state.read_bytes()).hexdigest()
                        session_source = slot_output
                elif harness == "codex" and record["status"] == "agent-completed":
                    if codex_session is None:
                        raise RuntimeError("Codex session directory was not initialized")
                    next_thread = codex_thread_id(slot_output / "trace.log")
                    if codex_thread is not None and next_thread != codex_thread:
                        raise RuntimeError("resumed Codex task changed its native thread UUID")
                    session_files = sorted(codex_session.rglob("*.jsonl"))
                    if not session_files:
                        raise RuntimeError("completed Codex slot persisted no native session JSONL")
                    state_hash = hashlib.sha256()
                    for path in session_files:
                        state_hash.update(path.relative_to(codex_session).as_posix().encode() + b"\0")
                        state_hash.update(path.read_bytes())
                    record["session_state_sha256"] = state_hash.hexdigest()
                    record["native_session_id_sha256"] = hashlib.sha256(next_thread.encode()).hexdigest()
                    codex_thread = next_thread
                elif harness == "pi":
                    record["session_id"] = f"{config['RUN_ID']}:pi"
                    record["session_file"] = "session.jsonl"
                    if source_pi_session_sha256 is not None:
                        record["source_session_file_sha256"] = source_pi_session_sha256
                    if record["status"] == "agent-completed":
                        if pi_session_file is None or not pi_session_file.is_file():
                            raise RuntimeError("completed Pi slot has no session.jsonl")
                        record["session_file_sha256"] = hashlib.sha256(pi_session_file.read_bytes()).hexdigest()
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
    parser.add_argument("--prepare-images", action="store_true")
    parser.add_argument("--source", type=pathlib.Path)
    parser.add_argument("--work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--harness", choices=(*HARNESSES, "all"), default="carry")
    args = parser.parse_args()
    if args.validate_records:
        payload = json.loads(args.validate_records.read_text(encoding="utf-8"))
        _validate_records(payload["tasks"], payload["records"])
    elif args.run or args.prepare_images:
        if not args.source or not args.work or not args.output:
            parser.error("execution requires --source, --work, and --output")
        config = dict(os.environ)
        config["BENCHMARK_HARNESS"] = args.harness
        if args.prepare_images:
            execute_preparation(
                source=args.source, work=args.work, output=args.output, config=config,
            )
        else:
            execute_benchmark(
                source=args.source, work=args.work, output=args.output, config=config,
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
