#!/usr/bin/env python3
"""Opt-in preparation diagnostic, never a production recipe or readiness gate."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import resource
import selectors
import signal
import subprocess
import time
import urllib.request


SOLVER_URL = ("https://github.com/mamba-org/micromamba-releases/releases/"
              "download/2.3.3-0/micromamba-linux-64")
SOLVER_SHA256 = "9496f94a8b78c536573c93d946ec9bba74bd9ff79ee55aaa4b546e30db8f511b"
ENVIRONMENT = Path(__file__).parent / "fixtures/matplotlib-24627-environment.yml"
ENVIRONMENT_SHA256 = "02d571274b320e3083ce6b599e6110ab25112b4b7fce607c8c214d141d658b86"
# Exact heredoc bytes from the failed public dependency setup, including its
# final blank line. No task prompt, test patch, gold patch, or dataset is copied.
ENVIRONMENT_SOURCE = {
    "repository": "wiggzz/carry", "run_id": 35512750813,
    "artifact_id": 10607580591,
    "artifact": "swebench-prepare-long-50-carry-35512750813-1-attempt-1",
    "path": "preparation/build-logs/env/sweb.env.py.x86_64.d15840ebf2fd1cd2dd754d__latest/setup_env.sh",
    "setup_env_sha256": "9161f8f53d9c5561795d677f45e84e8ab225f3e716487d75b6d2787b227cc7db",
    "extraction": "literal EOF_59812759871 heredoc; no edits",
    "original_commands": ["CONDA_SOLVER=classic conda env create --file environment.yml",
                          "conda activate testbed && CONDA_SOLVER=classic conda install python=3.11 -y"],
}
LOG_LIMIT = 4 * 1024**2


def memory_snapshot():
    values = {}
    for line in Path("/proc/meminfo").read_text().splitlines():
        key, value = line.split(":", 1)
        if key in ("MemTotal", "MemAvailable"):
            values[key + "_bytes"] = int(value.split()[0]) * 1024
    return values


def parse_requirements(data):
    """Only the checksum-pinned fixture's simple YAML/spec subset is supported."""
    if hashlib.sha256(data).hexdigest() != ENVIRONMENT_SHA256:
        raise ValueError("environment SHA-256 mismatch")
    conda, pip = [], []
    for line in data.decode().splitlines():
        if line.startswith("      - "):
            pip.append(line[8:])
        elif line.startswith("  - ") and line not in ("  - conda-forge", "  - pip:"):
            conda.append(line[4:])
    return conda, pip


def validate_plan(plan, requirements, pip_requirements):
    if plan.get("success") is not True or plan.get("dry_run") is not True:
        raise ValueError("solver did not report a successful dry-run")
    records = {record["name"]: record["version"] for record in plan["actions"]["LINK"]}
    python = records.get("python", "")
    if not re.fullmatch(r"3\.11\.\d+", python):
        raise ValueError("plan does not contain Python 3.11")
    verified = []
    for spec in requirements:
        match = re.fullmatch(r"([\w-]+)(?:\[execute\])?((?:(?:>=|!=)[\d.]+,?)*)", spec)
        if not match:
            raise ValueError("unsupported declared spec: " + spec)
        name, constraints = match.groups()
        version = records.get(name, "")
        if not re.fullmatch(r"\d+(?:\.\d+)*", version):
            raise ValueError("missing or unsupported planned version: " + name)
        actual = tuple(int(part) for part in version.split("."))
        for op, value in re.findall(r"(>=|!=)([\d.]+)", constraints):
            required = tuple(int(part) for part in value.split("."))
            length = max(len(actual), len(required))
            left, right = actual + (0,) * (length - len(actual)), required + (0,) * (length - len(required))
            if (op == ">=" and left < right) or (op == "!=" and left == right):
                raise ValueError("planned version violates " + spec)
        verified.append(name)
    return {"python": python, "conda_dependencies": verified,
            "declared_conda_specs": requirements, "pip_not_validated": pip_requirements}


def run_bounded(command, work, evidence, timeout_seconds, memory_bytes):
    """Kill the entire process group on deadline; cap retained logs, not repodata."""
    def limits():
        resource.setrlimit(resource.RLIMIT_AS, (memory_bytes, memory_bytes))
        cpu = math.ceil(timeout_seconds)
        resource.setrlimit(resource.RLIMIT_CPU, (cpu, cpu))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))

    started = time.monotonic()
    environment = {"HOME": str(work / "home"), "PATH": "/usr/bin:/bin",
                   "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"}
    Path(environment["HOME"]).mkdir()
    process = subprocess.Popen(command, cwd=work, env=environment,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True, preexec_fn=limits)
    selector = selectors.DefaultSelector()
    outputs = {}
    sizes = {"stdout": 0, "stderr": 0}
    truncated = False
    timed_out = False
    usage = None
    try:
        for stream, name in ((process.stdout, "stdout"), (process.stderr, "stderr")):
            outputs[name] = (evidence / (name + ".log")).open("wb")
            selector.register(stream, selectors.EVENT_READ, name)
        while selector.get_map() or process.returncode is None:
            if time.monotonic() - started >= timeout_seconds:
                timed_out = True
                break
            for key, _ in selector.select(0.05):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                name = key.data
                room = max(0, LOG_LIMIT - sizes[name])
                outputs[name].write(chunk[:room])
                sizes[name] += len(chunk)
                truncated |= len(chunk) > room
            if process.returncode is None:
                pid, status, child_usage = os.wait4(process.pid, os.WNOHANG)
                if pid:
                    process.returncode = os.waitstatus_to_exitcode(status)
                    usage = child_usage
    finally:
        # Also kill descendants holding pipes after the direct child has exited.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        if process.returncode is None:
            _, status, usage = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
        selector.close()
        for out in outputs.values():
            out.close()
        process.stdout.close()
        process.stderr.close()
    return {"returncode": process.returncode, "timed_out": timed_out,
            "logs_truncated": truncated, "log_bytes_observed": sizes,
            "elapsed_seconds": time.monotonic() - started,
            "max_rss_kib": usage.ru_maxrss,
            "user_cpu_seconds": usage.ru_utime, "system_cpu_seconds": usage.ru_stime}


def run_probe(evidence, work, *, solver_url=SOLVER_URL, solver_sha256=SOLVER_SHA256,
              timeout_seconds=600, memory_bytes=None):
    evidence, work = Path(evidence).resolve(), Path(work).resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    started = time.monotonic()
    report = {"status": "started", "environment_ready": False,
              "scope": "Conda dry-run only; pip installation/build/readiness not tested",
              "experimental_changes": ["micromamba 2.3.3 instead of classic Conda",
                                       "apply eventual python=3.11 constraint upfront"],
              "started_utc": datetime.now(timezone.utc).isoformat(),
              "source": ENVIRONMENT_SOURCE, "environment_sha256": ENVIRONMENT_SHA256,
              "solver_url": solver_url, "solver_sha256": solver_sha256,
              "platform": platform.platform(), "machine": platform.machine(),
              "github_sha": os.environ.get("GITHUB_SHA"),
              "github_run_id": os.environ.get("GITHUB_RUN_ID")}
    result = evidence / "result.json"
    result.write_text(json.dumps(report, indent=2) + "\n")
    for name in ("stdout.log", "stderr.log"):
        (evidence / name).touch()
    try:
        work.mkdir(parents=True, exist_ok=False)
        before = memory_snapshot()
        budget = min(6 * 1024**3, before["MemAvailable_bytes"] // 2)
        memory_bytes = budget if memory_bytes is None else min(memory_bytes, budget)
        if not 0 < timeout_seconds <= 600 or memory_bytes < 128 * 1024**2:
            raise ValueError("invalid time/memory budget")
        if platform.system() != "Linux" or platform.machine() != "x86_64":
            raise ValueError("probe requires Linux x86_64")
        report.update(memory_before=before, memory_limit_bytes=memory_bytes,
                      memory_limit_kind="RLIMIT_AS (virtual address space)",
                      timeout_seconds=timeout_seconds, log_limit_bytes_per_stream=LOG_LIMIT)
        data = ENVIRONMENT.read_bytes()
        (evidence / "environment.yml").write_bytes(data)
        requirements, pip_requirements = parse_requirements(data)
        report["environment_size_bytes"] = len(data)
        solver = work / "micromamba"
        command = [str(solver), "create", "--no-rc", "--no-env",
                   "--root-prefix", str(work / "mamba"), "--prefix", str(work / "env"),
                   "--file", str(evidence / "environment.yml"),
                   "-c", "conda-forge", "-c", "defaults",
                   "--channel-priority", "flexible", "--platform", "linux-64",
                   "python=3.11", "--dry-run", "--json", "--yes"]
        report["command"] = command
        report["channels"] = ["conda-forge", "defaults"]
        result.write_text(json.dumps(report, indent=2) + "\n")
        download_verified(solver_url, solver_sha256, solver)
        solver.chmod(0o500)
        report["solver_download_verified"] = True
        execution = run_bounded(command, work, evidence, timeout_seconds, memory_bytes)
        report["process"] = execution
        if execution["timed_out"] or execution["returncode"] != 0 or execution["logs_truncated"]:
            raise ValueError("solver timed out, failed, or exceeded log budget")
        plan = json.loads((evidence / "stdout.log").read_text())
        report["validation"] = validate_plan(plan, requirements, pip_requirements)
        report["status"] = "conda-dry-run-validated"
        return 0
    except Exception as error:
        report.update(status="failed", error=f"{type(error).__name__}: {error}"[:4096])
        return 1
    finally:
        report["elapsed_seconds"] = time.monotonic() - started
        report["finished_utc"] = datetime.now(timezone.utc).isoformat()
        report["memory_after"] = memory_snapshot()
        result.write_text(json.dumps(report, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        parser.error("real solver probes are restricted to an opt-in GitHub-hosted job")
    return run_probe(args.evidence_dir, args.work_dir)


def download_verified(url, expected_sha256, target):
    """Bound a public download and publish it only after checksum verification."""
    target = Path(target)
    partial = target.with_suffix(".part")
    digest = hashlib.sha256()
    deadline = time.monotonic() + 120
    size = 0
    try:
        with urllib.request.urlopen(url, timeout=10) as response, partial.open("wb") as out:
            while True:
                # One underlying read: a dribbling peer cannot keep a buffered
                # read(65536) busy indefinitely without a wall-deadline check.
                chunk = response.read1(65536)
                if not chunk:
                    break
                size += len(chunk)
                if size > 64 * 1024**2 or time.monotonic() > deadline:
                    raise ValueError("download exceeded size/time budget")
                digest.update(chunk)
                out.write(chunk)
        if digest.hexdigest() != expected_sha256:
            raise ValueError("download SHA-256 mismatch")
        partial.replace(target)
    finally:
        partial.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main())
