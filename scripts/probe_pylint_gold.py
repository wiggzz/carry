#!/usr/bin/env python3
"""Unpublished single-Pylint gold diagnostic; not full-50 or five-task certification."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import subprocess
import time

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scripts import swebench_smoke as smoke
from scripts import swebench_preparation_compat as compat
from scripts import probe_matplotlib_preparation as preparation
from scripts import probe_pylint_evaluator as diagnostic

TASK = diagnostic.TASK
WALL_SECONDS = 1800
EVALUATOR_SECONDS = 270
WORKER_STAGES = {"build", "source", "dataset", "alias", "evaluate", "events", "grade", "complete"}
WORKER_REASONS = {"in_progress", "worker_exception", "validated", "official_gold_unresolved",
                  "grade_aggregate_invalid", "grade_report_coverage_invalid", "grade_test_patch_unproven",
                  "grade_test_output_invalid", "grade_evaluation_identity_invalid",
                  "grade_test_execution_invalid", "grade_target_coverage_invalid", "grade_report_replay_invalid"}
SAFE_EXCEPTIONS = (FileNotFoundError, TimeoutError, MemoryError, ValueError, TypeError,
                   KeyError, ImportError, OSError, RuntimeError, Exception, BaseException)


def exception_class(error):
    # Match trusted classes, not arbitrary subclass names or exception arguments.
    return next(cls.__name__ for cls in SAFE_EXCEPTIONS if isinstance(error, cls))


def safe_diagnostics(summary):
    allowed = {"worker_stage": WORKER_STAGES, "worker_reason": WORKER_REASONS,
               "worker_exception_type": {cls.__name__ for cls in SAFE_EXCEPTIONS}}
    return {key: summary[key] for key, values in allowed.items()
            if isinstance(summary.get(key), str) and summary[key] in values}



def setup_specs():
    """Only public metadata enters the real production setup transformation."""
    from importlib.metadata import version
    from swebench.harness.test_spec.test_spec import make_test_spec
    if version("swebench") != "4.1.0":
        raise ValueError("requires swebench==4.1.0")
    original = make_test_spec(dict(TASK, test_patch="", FAIL_TO_PASS=[], PASS_TO_PASS=[]))
    specs, report = compat.transform_test_specs([original], swebench_version="4.1.0")
    identity = report["tasks"][TASK["instance_id"]]
    if identity["original_recipe_sha256"] == identity["effective_recipe_sha256"]:
        raise ValueError("repaired recipe required")
    return original, specs[0], identity


def validate_grade(output, row, run_id, checkpoint=lambda stage, reason: None):
    """Replay unchanged upstream grading; return counts/hashes, never test content."""
    from swebench.harness.test_spec.test_spec import make_test_spec
    from swebench.harness.grading import get_eval_report, get_logs_eval
    from swebench.harness.constants import APPLY_PATCH_PASS, START_TEST_OUTPUT, END_TEST_OUTPUT
    task = TASK["instance_id"]
    checkpoint("grade", "grade_aggregate_invalid")
    aggregate = json.loads((output / ("gold." + run_id + ".json")).read_text())
    # Only a coherent completed, unresolved official aggregate earns this code;
    # missing/corrupt/incomplete/error reports remain validator/report failures.
    unresolved = {key: [task] for key in ("completed_ids", "submitted_ids", "unresolved_ids")}
    unresolved.update({key: [] for key in ("resolved_ids", "error_ids", "incomplete_ids", "empty_patch_ids")})
    unresolved.update({key: 1 for key in ("total_instances", "submitted_instances", "completed_instances", "unresolved_instances")})
    unresolved.update({key: 0 for key in ("resolved_instances", "error_instances", "empty_patch_instances")})
    if all(type(aggregate.get(key)) is type(value) and aggregate.get(key) == value
           for key, value in unresolved.items()):
        checkpoint("grade", "official_gold_unresolved")
        raise ValueError("official gold unresolved")
    for key in ("completed_ids", "resolved_ids", "submitted_ids"):
        if aggregate.get(key) != [task]:
            raise ValueError("gold aggregate coverage")
    for key in ("error_ids", "incomplete_ids", "empty_patch_ids", "unresolved_ids"):
        if aggregate.get(key) != []:
            raise ValueError("gold aggregate failure")
    for key, expected in (("total_instances", 1), ("submitted_instances", 1),
                          ("completed_instances", 1), ("resolved_instances", 1),
                          ("error_instances", 0), ("empty_patch_instances", 0), ("unresolved_instances", 0)):
        if type(aggregate.get(key)) is not int or aggregate[key] != expected:
            raise ValueError("gold aggregate count")
    checkpoint("grade", "grade_report_coverage_invalid")
    folder = output / "logs/run_evaluation" / run_id / "gold" / task
    if list((output / "logs/run_evaluation").glob("*/*/*/report.json")) != [folder / "report.json"]:
        raise ValueError("unexpected report coverage")
    checkpoint("grade", "grade_test_patch_unproven")
    spec = make_test_spec(row)
    from swebench.harness.test_spec.python import get_modified_files
    log = folder / "test_output.txt"
    content = log.read_text()
    test_files = get_modified_files(row["test_patch"])
    if not test_files or any("Applied patch " + name + " cleanly." not in content for name in test_files):
        raise ValueError("test patch application not proven")
    checkpoint("grade", "grade_test_output_invalid")
    if (content.count(START_TEST_OUTPUT) != 1 or content.count(END_TEST_OUTPUT) != 1
            or "Traceback (most recent call last):" in content):
        raise ValueError("missing test markers or raw exception")
    checkpoint("grade", "grade_evaluation_identity_invalid")
    if ((folder / "eval.sh").read_text() != spec.eval_script
            or not row["patch"] or (folder / "patch.diff").read_text() != row["patch"]
            or APPLY_PATCH_PASS not in (folder / "run_instance.log").read_text()):
        raise ValueError("official script or patch identity/application")
    checkpoint("grade", "grade_test_execution_invalid")
    statuses, found = get_logs_eval(spec, str(log))
    if not found or not statuses or any(s in {"ERROR", "FAILED"} for s in statuses.values()):
        raise ValueError("test execution error")
    checkpoint("grade", "grade_target_coverage_invalid")
    counts = {}
    for field in ("FAIL_TO_PASS", "PASS_TO_PASS"):
        expected = getattr(spec, field)
        if not expected or len(expected) != len(set(expected)) or any(statuses.get(t) != "PASSED" for t in expected):
            raise ValueError("incomplete target coverage")
        counts[field.lower() + "_count"] = len(expected)
    checkpoint("grade", "grade_report_replay_invalid")
    prediction = {"instance_id": task, "model_name_or_path": "gold", "model_patch": row["patch"]}
    replay = get_eval_report(spec, prediction, str(log), True)
    if (replay != json.loads((folder / "report.json").read_text())
            or replay[task].get("resolved") is not True
            or replay[task].get("patch_successfully_applied") is not True):
        raise ValueError("official report replay mismatch")
    return dict(counts, raw_sha256={name: hashlib.sha256((folder / name).read_bytes()).hexdigest()
                for name in ("test_output.txt", "eval.sh", "patch.diff", "report.json", "run_instance.log")})



def verify_source(image, run_id):
    """Public-only source lineage probe before any gold data is loaded."""
    command = ["docker", "run", "--rm", "--name", run_id + "-source", "--network", "none",
               "--memory", str(diagnostic.MEMORY_BYTES), "--memory-swap", str(diagnostic.MEMORY_BYTES),
               "--workdir", "/tmp", "--mount", "type=bind,src=" + str(Path(diagnostic.__file__).resolve()) + ",dst=/diagnostic/probe.py,readonly",
               "--entrypoint", diagnostic.PYTHON, image, "/diagnostic/probe.py", "--inside", "snapshot"]
    result = subprocess.run(command, check=True, capture_output=True, text=True, timeout=60)
    snapshot = diagnostic.parse_payload(result)
    diagnostic.validate_source_identity(snapshot)
    return hashlib.sha256(json.dumps(snapshot["source_identity"], sort_keys=True).encode()).hexdigest()


def build_pair(work, run_id, client, checkpoint=lambda stage: None):
    """Production transform, upstream builder and sanitized layer, without publish."""
    from swebench.harness import docker_build, dockerfiles
    source = Path(__file__).resolve().parent.parent
    original, spec, identity = setup_specs()
    base_hash = smoke.enforce_https_swebench_base_images(dockerfiles._DOCKERFILE_BASE, preparation.TRUSTED_CA_IMAGE)
    cache_key = smoke.task_image_cache_key(TASK,
        prepared_dockerfile_sha256=smoke.prepared_image_recipe_sha256(source), base_dockerfile_sha256=base_hash)
    for kind, attr in (("base", "BASE_IMAGE_BUILD_DIR"), ("env", "ENV_IMAGE_BUILD_DIR"), ("instances", "INSTANCE_IMAGE_BUILD_DIR")):
        setattr(docker_build, attr, work / "build-logs" / kind)
    preparation.install_build_limits(client, diagnostic.MEMORY_BYTES)
    _, failed = docker_build.build_instance_images(client, [spec], force_rebuild=False,
        max_workers=1, tag="latest", env_image_tag="latest")
    if failed:
        raise ValueError("official build failed")
    image = client.images.get(spec.instance_image_key)
    if not smoke.LOCAL_IMAGE_ID.fullmatch(image.id):
        raise ValueError("invalid local evaluator identity")
    prepared = smoke.build_prepared_task_image(source=source, run_id=run_id,
        instance_id=TASK["instance_id"], task_image_id=image.id, cache_key=cache_key,
        execute=preparation.bounded_execute(diagnostic.MEMORY_BYTES))
    labels = client.images.get(prepared["image_id"]).attrs.get("Config", {}).get("Labels", {})
    if (labels.get("org.carry.swebench.evaluator-image-id") != image.id
            or labels.get("org.carry.swebench.task-cache-key") != cache_key):
        raise ValueError("prepared agent/evaluator pair identity mismatch")
    checkpoint("source")
    source_hash = verify_source(image.id, run_id)
    return dict(identity, cache_key=cache_key, evaluator_image_id=image.id,
                agent_image_id=prepared["image_id"], source_identity_sha256=source_hash,
                prepared_recipe_sha256=smoke.prepared_image_recipe_sha256(source),
                compatibility_sha256=compat.preparation_compatibility_sha256(), base_recipe_sha256=base_hash)


def verify_events(start, name, alias):
    """Reject upstream fallback pulls/builds or a missing/ambiguous alias consumer."""
    result = subprocess.run(["docker", "events", "--since", str(start), "--until", str(time.time()),
                             "--format", "{{json .}}"], check=True, capture_output=True, text=True, timeout=30)
    events = [json.loads(line) for line in result.stdout.splitlines()]
    if any(e.get("Type") == "image" for e in events):
        raise ValueError("image activity during evaluation; possible fallback")
    creates = [e.get("Actor", {}).get("Attributes", {}) for e in events
               if e.get("Type") == "container" and e.get("Action") == "create"]
    if len(creates) != 1 or creates[0].get("name") != name or creates[0].get("image") != alias:
        raise ValueError("official alias not consumed exactly once")


def evaluate(row, work, run_id, client, evaluator_id, checkpoint=lambda stage, reason="in_progress": None):
    from swebench.harness.test_spec.test_spec import make_test_spec
    checkpoint("alias")
    if any(row.get(key) != value for key, value in TASK.items()):
        raise ValueError("pinned public metadata mismatch")
    output = (work / "official").resolve()
    output.mkdir(parents=True, exist_ok=False)  # upstream must never reuse report.json
    dataset = output / "canonical.json"
    preparation.write_json(dataset, [row])
    spec = make_test_spec(row, namespace=smoke.OFFICIAL_IMAGE_NAMESPACE,
                          instance_image_tag=smoke.OFFICIAL_IMAGE_TAG)
    alias = spec.instance_image_key
    if client.images.get(alias).id != evaluator_id:
        raise ValueError("official evaluator alias mismatch")
    environment = {key: os.environ[key] for key in ("HOME", "PATH", "LANG", "TMPDIR", "DOCKER_CONFIG") if key in os.environ}
    previous = {key: os.environ.get(key) for key in ("PYTHON", "EVALUATOR_TIMEOUT_SECONDS")}
    os.environ.update(PYTHON=sys.executable, EVALUATOR_TIMEOUT_SECONDS=str(EVALUATOR_SECONDS))
    start = time.time()
    checkpoint("evaluate")
    try:
        smoke.run_official_evaluation(predictions=Path("gold"), canonical_dataset=dataset,
            instance_ids=[TASK["instance_id"]], run_id=run_id, output=output,
            environment=environment, process_timeout_seconds=EVALUATOR_SECONDS, max_workers=1)
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
    checkpoint("alias")
    if client.images.get(alias).id != evaluator_id:
        raise ValueError("official evaluator alias changed")
    checkpoint("events")
    verify_events(start, spec.get_instance_container_name(run_id), alias)
    checkpoint("grade")
    return validate_grade(output, row, run_id, checkpoint)



def worker(work, run_id):
    import signal
    summary = {}
    client = None
    def checkpoint(stage, reason="in_progress"):
        if stage not in WORKER_STAGES or reason not in WORKER_REASONS:
            raise ValueError("invalid worker checkpoint")
        summary.update(worker_stage=stage, worker_reason=reason)
        preparation.write_json(work / "worker-summary.json", summary)
    checkpoint("build")
    try:
        import docker
        from datasets import load_dataset
        from swebench.harness.test_spec.test_spec import make_test_spec
        client = docker.from_env(timeout=60)
        signal.alarm(1200)  # Build/download ceiling; parent owns cleanup on SIGALRM.
        identity = build_pair(work, run_id, client, checkpoint)
        signal.alarm(0)
        summary["identity"] = identity
        checkpoint("dataset")
        # Gold/test material is fetched only AFTER all image/context construction.
        records = load_dataset(smoke.DATASET, split="test", revision=smoke.DATASET_REVISION, token=False)
        rows = [dict(row) for row in records if row["instance_id"] == TASK["instance_id"]]
        if len(rows) != 1 or any(rows[0].get(key) != value for key, value in TASK.items()):
            raise ValueError("pinned dataset selection mismatch")
        checkpoint("alias")
        spec = make_test_spec(rows[0], namespace=smoke.OFFICIAL_IMAGE_NAMESPACE,
                              instance_image_tag=smoke.OFFICIAL_IMAGE_TAG)
        client.images.get(identity["evaluator_image_id"]).tag(spec.instance_image_key)
        grade = evaluate(rows[0], work, run_id, client, identity["evaluator_image_id"], checkpoint)
        client.close()
        client = None
        summary.update(grade=grade, gold_validated=True, worker_stage="complete", worker_reason="validated")
        preparation.write_json(work / "worker-summary.json", summary)
    except BaseException as error:
        summary.update(gold_validated=False, worker_exception_type=exception_class(error))
        if summary["worker_reason"] == "in_progress":
            summary["worker_reason"] = "worker_exception"
        preparation.write_json(work / "worker-summary.json", summary)
        raise  # Traceback/arguments stay in the disposable private transport.
    finally:
        signal.alarm(0)
        if client is not None:
            client.close()


def run_probe(evidence, work, *, execute=None):
    import shutil
    import uuid
    evidence, work = Path(evidence).resolve(), Path(work).resolve()
    source = Path(__file__).resolve().parent.parent
    if (work == source or source in work.parents or work == evidence
            or evidence in work.parents or work in evidence.parents):
        raise ValueError("private work must be outside source and upload directories")
    evidence.mkdir(parents=True, exist_ok=False)
    evidence.chmod(0o755)  # Only safe summaries: the unprivileged uploader must traverse.
    run_id = "pylint-gold-" + uuid.uuid4().hex
    run_work = work / run_id
    report = {"status": "failed", "gold_validated": False, "environment_ready": False,
        "score_validated": False, "cleanup_verified": False, "run_id": run_id,
        "task": TASK["instance_id"], "dataset": smoke.DATASET, "dataset_revision": smoke.DATASET_REVISION,
        "github_sha": os.environ.get("GITHUB_SHA"), "github_run_id": os.environ.get("GITHUB_RUN_ID"),
        "scope": "unpublished one-task gold diagnostic; no readiness/catalog/five-task certification",
        "model_execution": False, "registry_publication": False,
        "limits": {"wall_seconds": WALL_SECONDS, "build_seconds": 1200,
            "evaluator_seconds": EVALUATOR_SECONDS, "max_workers": 1,
            "build_memory_bytes": diagnostic.MEMORY_BYTES, "worker_address_space_bytes": diagnostic.MEMORY_BYTES,
            "worker_cpu_seconds": 300, "log_bytes_per_stream": diagnostic.LOG_LIMIT,
            "disk_growth_budget_bytes": 12 * 1024**3, "disk_reserve_bytes": 8 * 1024**3,
            "disk_watchdog_interval_seconds": 0.1,
            "official_container_limits": "unchanged upstream: no diagnostic memory/PID/capability/network flags; ephemeral credential-free host"}}
    preparation.write_json(evidence / "result.json", report)
    (evidence / "result.json").chmod(0o644)
    created = owned = False
    run = execute
    try:
        work.mkdir(parents=True, mode=0o700, exist_ok=False)
        created = True
        run_work.mkdir(mode=0o700)
        diagnostic.preflight(work)
        run = run or diagnostic.Transport(run_work / "private-transport", run_work, timeout=WALL_SECONDS)
        if run(["docker", "ps", "-aq"], label="initial").stdout.strip():
            raise ValueError("requires empty exclusive ephemeral Docker daemon")
        owned = True
        fmt = '{"MemoryLimit":{{json .MemoryLimit}},"SwapLimit":{{json .SwapLimit}},"DockerRootDir":{{json .DockerRootDir}}}'
        info = json.loads(run(["docker", "info", "--format", fmt], label="daemon").stdout)
        if info.get("MemoryLimit") is not True or info.get("SwapLimit") is not True:
            raise ValueError("Docker build memory/swap limits unavailable")
        if isinstance(run, diagnostic.Transport):
            if shutil.disk_usage(info["DockerRootDir"]).free < 20 * 1024**3:
                raise ValueError("insufficient daemon disk")
            run.watch_disk(info["DockerRootDir"])
        run([sys.executable, str(Path(__file__).resolve()), "--worker", "--work-dir", str(run_work),
             "--run-id", run_id], label="worker", seconds=WALL_SECONDS)
        summary = json.loads((run_work / "worker-summary.json").read_text())
        if summary.get("gold_validated") is not True:
            raise ValueError("missing validated gold evidence")
        report.update({key: summary[key] for key in ("identity", "grade", "gold_validated") if key in summary},
                      status="single-task-gold-validated")
    except Exception as error:
        report["error_type"] = type(error).__name__  # Never leak exception args/raw gold.
    finally:
        if owned:
            try:
                # Initially empty, exclusive disposable daemon: these IDs belong
                # only to this run, including daemon-side interrupted build steps.
                ids = run(["docker", "ps", "-aq"], label="cleanup-list", seconds=30, cleanup=True).stdout.split()
                if ids:
                    run(["docker", "rm", "-f", *ids], label="remove", seconds=60, cleanup=True)
                if run(["docker", "ps", "-aq"], label="remaining", seconds=30, cleanup=True).stdout.strip():
                    raise RuntimeError("run containers remain")
                report["cleanup_verified"] = True
            except Exception as error:
                report["cleanup_error_type"] = type(error).__name__
        if created:
            try:
                summary_path = run_work / "worker-summary.json"
                if summary_path.is_file():
                    summary = json.loads(summary_path.read_text())
                    report.update(safe_diagnostics(summary))
                    if isinstance(summary.get("identity"), dict):
                        report["identity"] = summary["identity"]
                processes = list((run_work / "private-transport").glob("*-worker/process.json"))
                if len(processes) == 1:
                    process = json.loads(processes[0].read_text())
                    report["worker_returncode"] = process["returncode"]
                    report["worker_stop_reason"] = process["failure"]
            except (OSError, ValueError, KeyError):
                report["private_summary_invalid"] = True
            try:
                shutil.rmtree(work)  # Raw gold never crosses the upload boundary.
            except OSError as error:
                report["private_cleanup_error_type"] = type(error).__name__
        if not report["cleanup_verified"] or work.exists():
            report.update(status="failed", gold_validated=False)
        preparation.write_json(evidence / "result.json", report)
        (evidence / "result.json").chmod(0o644)
    return 0 if report["gold_validated"] else 1


def main():
    import signal
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--run-id", help=argparse.SUPPRESS)
    args = parser.parse_args()
    os.umask(0o077)
    if args.worker:
        if Path.cwd() != args.work_dir or not args.run_id:
            parser.error("worker requires its private supervisor directory")
        worker(args.work_dir, args.run_id)
        return 0
    if (os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
            or os.geteuid() != 0 or not args.evidence_dir):
        parser.error("requires opt-in root GitHub-hosted job and evidence directory")
    def interrupted(signum, frame):
        raise RuntimeError("supervisor interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    return run_probe(args.evidence_dir, args.work_dir)


if __name__ == "__main__":
    raise SystemExit(main())
