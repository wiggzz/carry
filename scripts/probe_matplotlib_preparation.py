#!/usr/bin/env python3
"""Default-off, credential-free actual-image diagnostic; never a task catalog."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scripts import swebench_smoke as smoke
from scripts import swebench_preparation_compat as compat
from scripts import probe_matplotlib_solver as solver

TASK = {
    "instance_id": "matplotlib__matplotlib-24627", "repo": "matplotlib/matplotlib",
    "version": "3.6", "base_commit": "9d22ab09d52d279b125d8770967569de070913b2",
    "environment_setup_commit": "73909bcb408886a22e2b84581d6b9e6d9907c813",
}
# Same CA donor as the production preparation worker; not an agent execution.
TRUSTED_CA_IMAGE = "node@sha256:afff6d8c97964a438d2e6a9c96509367e45d8bf93f790ad561a1eaea926303d9"


def install_build_limits(client, memory_bytes):
    """Official SDK legacy builds enforce daemon-side memory, not client rlimits."""
    original = client.api.build
    def build(**kwargs):
        kwargs["container_limits"] = {"memory": memory_bytes, "memswap": memory_bytes}
        observed = 0
        response = original(**kwargs)
        try:
            for chunk in response:
                observed += len(json.dumps(chunk).encode())
                if observed > solver.LOG_LIMIT:
                    raise RuntimeError("official Docker build exceeded log budget")
                yield chunk
        finally:
            close = getattr(response, "close", None)
            if close:
                close()
    client.api.build = build


def index_evidence(evidence):
    """Bound retained files, discard links, and hash every uploaded payload."""
    previous_path = evidence / "artifact-index.json"
    previous = json.loads(previous_path.read_text()) if previous_path.is_file() else {}
    originals = {item["path"]: item["original_size_bytes"] for item in previous.get("files", [])}
    entries = []
    truncated = previous.get("truncated", False)
    total = 0
    for path in sorted(evidence.rglob("*")):
        if path.is_symlink():
            path.unlink()
            continue
        if not path.is_file() or path.name == "artifact-index.json":
            continue
        size = path.stat().st_size
        budget = min(solver.LOG_LIMIT, max(0, 64 * 1024**2 - total))
        if size > budget:
            with path.open("r+b") as stream:
                stream.truncate(budget)
            truncated = True
        payload = path.read_bytes()
        total += len(payload)
        relative = str(path.relative_to(evidence))
        entries.append({"path": relative, "size_bytes": len(payload),
                        "sha256": hashlib.sha256(payload).hexdigest(),
                        "original_size_bytes": max(size, originals.get(relative, 0))})
    write_json(evidence / "artifact-index.json", {"files": entries, "total_bytes": total,
                                                "truncated": truncated})
    return truncated


def run_probe(evidence, work, *, execute=subprocess.run, worker_command=None, timeout_seconds=1800):
    evidence, work = Path(evidence).resolve(), Path(work).resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    report = {"status": "started", "environment_ready": False,
              "scope": "single-image diagnostic, not full-50 preparation",
              "github_sha": os.environ.get("GITHUB_SHA"),
              "github_run_id": os.environ.get("GITHUB_RUN_ID"),
              "platform": platform.platform(), "machine": platform.machine(),
              "stage_wall_limit_seconds": timeout_seconds, "readiness_limit_seconds": 180,
              "log_limit_bytes_per_file": solver.LOG_LIMIT, "artifact_limit_bytes": 64 * 1024**2,
              "max_workers": 1, "cleanup_verified": False}
    started = time.monotonic()
    owns_daemon = False
    write_json(evidence / "result.json", report)
    try:
        if not 0 < timeout_seconds <= 1800:
            raise ValueError("invalid image stage timeout")
        work.mkdir(parents=True, exist_ok=False)
        memory = solver.memory_snapshot()
        disk_free = shutil.disk_usage(work).free
        report.update(memory_before=memory, disk_free_bytes=disk_free)
        if (platform.system() != "Linux" or platform.machine() != "x86_64"
                or memory["MemTotal_bytes"] < 8 * 1024**3 or disk_free < 20 * 1024**3):
            raise ValueError("image probe requires Linux x86_64, 8 GiB RAM and 20 GiB free disk")
        memory_bytes = min(6 * 1024**3, memory["MemAvailable_bytes"] // 2)
        if memory_bytes < 3 * 1024**3:
            raise ValueError("insufficient available memory for isolated image probe")
        report["memory_limit_bytes"] = memory_bytes
        report["memory_limit_kind"] = "Docker memory=memswap plus child RLIMIT_AS/CPU"
        # This lane is an ephemeral standalone job. Refuse shared/live daemons;
        # after that gate only this job can have created cleanup targets.
        if execute(["docker", "ps", "-aq"], check=True, capture_output=True, text=True, timeout=30).stdout.strip():
            raise ValueError("image probe requires an empty ephemeral Docker daemon")
        owns_daemon = True
        info = json.loads(execute(["docker", "info", "--format", "{{json .}}"],
                                 check=True, capture_output=True, text=True, timeout=30).stdout)
        report["docker"] = {key: info.get(key) for key in
                            ("ServerVersion", "Architecture", "OSType", "KernelVersion", "MemTotal", "Driver",
                             "MemoryLimit", "SwapLimit")}
        if info.get("MemoryLimit") is not True or info.get("SwapLimit") is not True:
            raise ValueError("Docker daemon does not confirm memory/swap limit support")
        command = worker_command or [sys.executable, "-c",
            "import sys; sys.path.insert(0, sys.argv[1]); "
            "from scripts.probe_matplotlib_preparation import run_image_stage; "
            "run_image_stage(*sys.argv[2:])", str(Path(__file__).resolve().parent.parent),
            str(evidence), str(work), str(memory_bytes)]
        write_json(evidence / "result.json", report)
        report["process"] = solver.run_bounded(command, work, evidence, timeout_seconds, memory_bytes)
        process = report["process"]
        if process["returncode"] or process["timed_out"] or process["logs_truncated"]:
            raise RuntimeError("image stage failed, timed out, or exceeded log budget")
        image = json.loads((evidence / "image-result.json").read_text())
        if image.get("status") != "image-readiness-validated" or image.get("environment_ready") is not True:
            raise RuntimeError("image stage lacks validated readiness evidence")
        report.update(status="image-readiness-validated", environment_ready=True)
    except Exception as error:
        report.update(status="failed", environment_ready=False, error=f"{type(error).__name__}: {error}"[:2048])
    finally:
        if owns_daemon:
            try:
                ids = execute(["docker", "ps", "-aq"], check=True, capture_output=True, text=True, timeout=30).stdout.split()
                if ids:
                    execute(["docker", "rm", "-f", *ids], check=True, capture_output=True, text=True, timeout=60)
                remaining = execute(["docker", "ps", "-aq"], check=True, capture_output=True, text=True, timeout=30).stdout.strip()
                if remaining:
                    raise RuntimeError("job containers remain after cleanup")
                report["cleanup_verified"] = True
            except Exception as error:
                report.update(status="failed", environment_ready=False, cleanup_error=type(error).__name__)
        report["elapsed_seconds"] = time.monotonic() - started
        write_json(evidence / "result.json", report)
        if index_evidence(evidence):
            report.update(status="failed", environment_ready=False, error="evidence exceeded size budget")
            write_json(evidence / "result.json", report)
            index_evidence(evidence)
    return 0 if report["status"] == "image-readiness-validated" else 1


def run_image_stage(evidence, work, memory_bytes):
    """Child process: load actual pinned dataset/harness and exercise Docker."""
    from importlib.metadata import version
    import docker
    from datasets import load_dataset
    from swebench.harness import docker_build, dockerfiles
    from swebench.harness.constants import MAP_REPO_VERSION_TO_SPECS
    from swebench.harness.log_parsers import MAP_REPO_TO_PARSER
    from swebench.harness.test_spec.test_spec import make_test_spec

    evidence, work, memory_bytes = Path(evidence), Path(work), int(memory_bytes)
    evidence.mkdir(parents=True, exist_ok=True)
    source = Path(__file__).resolve().parent.parent
    if version("swebench") != "4.1.0":
        raise ValueError("actual image diagnostic requires swebench==4.1.0")
    records = load_dataset(smoke.DATASET, split="test", revision=smoke.DATASET_REVISION, token=False)
    matches = [dict(record) for record in records if record["instance_id"] == TASK["instance_id"]]
    if len(matches) != 1 or any(matches[0].get(key) != value for key, value in TASK.items()):
        raise ValueError("frozen Matplotlib dataset metadata mismatch")
    record = matches[0]
    base_hash = smoke.enforce_https_swebench_base_images(dockerfiles._DOCKERFILE_BASE, TRUSTED_CA_IMAGE)
    original = make_test_spec(record)
    specs, compatibility = compat.transform_test_specs([original], swebench_version=version("swebench"))
    spec = specs[0]
    # Store only public setup recipes; never serialize TestSpec's evaluator/gold.
    hashes = {}
    for name, value in (("base.Dockerfile", spec.base_dockerfile),
                        ("environment.Dockerfile", spec.env_dockerfile),
                        ("instance.Dockerfile", spec.instance_dockerfile),
                        ("setup_env.sh", spec.setup_env_script),
                        ("setup_repo.sh", spec.install_repo_script)):
        (evidence / name).write_text(value)
        hashes[name] = hashlib.sha256(value.encode()).hexdigest()
    environment = original.env_script_list[1].split("\n", 1)[1].removesuffix("EOF_59812759871").encode()
    if hashlib.sha256(environment).hexdigest() != solver.ENVIRONMENT_SHA256:
        raise ValueError("actual TestSpec environment identity mismatch")
    (evidence / "environment.yml").write_bytes(environment)
    effective = spec.env_script_list[1].split("\n", 1)[1].removesuffix("EOF_59812759871").encode()
    (evidence / "effective-environment.yml").write_bytes(effective)
    write_json(evidence / "inputs.json", {
        "task": TASK, "dataset": smoke.DATASET, "dataset_revision": smoke.DATASET_REVISION,
        "swebench_version": version("swebench"), "platform": spec.platform,
        "environment_sha256": hashlib.sha256(environment).hexdigest(),
        "effective_environment_sha256": hashlib.sha256(effective).hexdigest(),
        "compatibility_sha256": compatibility["compatibility_sha256"],
        "base_template_sha256": base_hash, "trusted_ca_image": TRUSTED_CA_IMAGE,
        "prepared_recipe_sha256": smoke.prepared_image_recipe_sha256(source),
        "recipe_file_sha256": hashes,
        "virtual_packages": "resolved inside actual base container; hosted dry-run plan NOT reused",
    })
    for kind, attr in (("base", "BASE_IMAGE_BUILD_DIR"), ("env", "ENV_IMAGE_BUILD_DIR"),
                       ("instances", "INSTANCE_IMAGE_BUILD_DIR")):
        setattr(docker_build, attr, evidence / "build-logs" / kind)
    client = docker.from_env(timeout=1800)
    install_build_limits(client, memory_bytes)
    try:
        exercise_image(record=record, original=original, client=client,
            build_instances=docker_build.build_instance_images,
            parser=MAP_REPO_TO_PARSER[record["repo"]],
            public_command=MAP_REPO_VERSION_TO_SPECS[record["repo"]][record["version"]]["test_cmd"],
            evidence=evidence, work=work, source=source, memory_bytes=memory_bytes)
        identities = {}
        for kind, key in (("base", spec.base_image_key), ("environment", spec.env_image_key),
                          ("instance", spec.instance_image_key)):
            image = client.images.get(key)
            identities[kind] = {"key": key, "id": image.id,
                **{field: image.attrs.get(field) for field in ("Architecture", "Os", "RepoDigests")}}
        write_json(evidence / "image-identities.json", identities)
        process = bounded_execute(memory_bytes)([
            "docker", "run", "--rm", "--network", "none", "--read-only", "--cap-drop=ALL",
            "--security-opt", "no-new-privileges", "--entrypoint", "/bin/bash",
            spec.instance_image_key, "-lc", "set -eu; getconf GNU_LIBC_VERSION; uname -m; "
            "source /opt/miniconda3/bin/activate; conda activate testbed; "
            "python -c \"import sys; print(sys.version); print(sys.prefix); "
            "assert sys.version_info[:2] == (3,11); assert sys.prefix == '/opt/miniconda3/envs/testbed'\"; "
            "conda --version; python -m pip --version",
        ], check=True, text=True, capture_output=True, timeout=60)
        (evidence / "image-platform.log").write_text(process.stdout + process.stderr)
    finally:
        client.close()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".partial")
    temporary.write_text(json.dumps(value, sort_keys=True, indent=2) + "\n")
    temporary.replace(path)


def bounded_execute(memory_bytes):
    """Bound Docker CLI build/run allocations as well as SDK builds."""
    def execute(command, **kwargs):
        if command[:2] in (["docker", "build"], ["docker", "run"]):
            command = command[:2] + ["--memory", str(memory_bytes), "--memory-swap", str(memory_bytes)] + command[2:]
        if command[:2] == ["docker", "build"]:
            # Legacy builder enforces --memory; BuildKit silently ignores it.
            command = [token for token in command if token != "--progress=plain"]
            kwargs["env"] = dict(os.environ, DOCKER_BUILDKIT="0")
        return subprocess.run(command, **kwargs)
    return execute


def exercise_image(*, record, original, client, build_instances, parser,
                   public_command, evidence, work, source, memory_bytes):
    """Same real TestSpec/build/prepared-layer/readiness helpers as production.

    No publisher, registry login/push, synthetic catalog, hidden tests or agent.
    Injectable seams exist only for offline fixture tests.
    """
    evidence.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    report = {"status": "started", "environment_ready": False,
              "scope": "single-image diagnostic, not full-50 preparation",
              "dataset": smoke.DATASET, "dataset_revision": smoke.DATASET_REVISION,
              "task": dict(TASK), "memory_limit_bytes": memory_bytes, "max_workers": 1}
    try:
        if any(record.get(key) != value for key, value in TASK.items()):
            raise ValueError("frozen Matplotlib dataset metadata mismatch")
        specs, compatibility = compat.transform_test_specs([original], swebench_version="4.1.0")
        spec = specs[0]
        write_json(evidence / "compatibility.json", compatibility)
        write_json(evidence / "recipe.json", {"env_script_list": spec.env_script_list,
                                              "repo_script_list": spec.repo_script_list})
        report["compatibility"] = compatibility
        report["stage"] = "official-image-build"
        write_json(evidence / "image-result.json", report)
        _, failed = build_instances(client, specs, force_rebuild=False, max_workers=1,
                                    tag="latest", env_image_tag="latest")
        if failed:
            raise RuntimeError("official image builder reported failure")
        # Official build lists omit tasks blocked by env failure: read exact image.
        image = client.images.get(spec.instance_image_key)
        report["official_image_id"] = image.id
        report["stage"] = "prepared-layer"
        write_json(evidence / "image-result.json", report)
        execute = bounded_execute(memory_bytes)
        prepared = smoke.build_prepared_task_image(
            source=source, run_id="matplotlib-probe", instance_id=record["instance_id"],
            task_image_id=image.id,
            cache_key=compatibility["tasks"][record["instance_id"]]["effective_recipe_sha256"],
            execute=execute,
        )
        report["prepared_image"] = prepared
        report["dependencies"] = smoke.capture_dependency_manifest(
            image=prepared["tag"], output=evidence / "readiness", execute=execute,
        )
        report["stage"] = "public-readiness"
        write_json(evidence / "image-result.json", report)
        smoke._clone(record["repo"], record["base_commit"], work / "repo")
        script, command = smoke.trusted_readiness_script(spec, public_test_command=public_command)
        (evidence / "readiness.sh").write_text(script)
        # Production helper and parser are unchanged; only diagnostic capacity is
        # added to its Docker invocation inside this disposable single-task child.
        make_command = smoke.readiness_docker_command
        def limited_command(**kwargs):
            command = make_command(**kwargs)
            return command[:2] + ["--memory", str(memory_bytes), "--memory-swap", str(memory_bytes)] + command[2:]
        smoke.readiness_docker_command = limited_command
        try:
            report["readiness"] = smoke.run_task_readiness(
                instance_id=record["instance_id"], image=prepared["tag"], repo=work / "repo",
                script=script, test_command=command, parser=parser, test_spec=spec,
                output=evidence / "readiness", timeout_seconds=180,
            )
        finally:
            smoke.readiness_docker_command = make_command
        report.update(status="image-readiness-validated", environment_ready=True)
        return report
    except Exception as error:
        report.update(status="failed", error=f"{type(error).__name__}: {error}"[:2048])
        raise
    finally:
        write_json(evidence / "image-result.json", report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        parser.error("real image probes are restricted to an opt-in GitHub-hosted job")
    return run_probe(args.evidence_dir, args.work_dir)


if __name__ == "__main__":
    raise SystemExit(main())
