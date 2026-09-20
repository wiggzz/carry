#!/usr/bin/env python3
"""Offline process/API fixtures only; never actual image-build evidence."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

from scripts import swebench_smoke as smoke
from scripts.test_swebench_preparation_compat import matplotlib_spec

from scripts import probe_matplotlib_preparation as probe


class ImageProbeTests(unittest.TestCase):
    def test_official_build_receives_repaired_spec_then_public_readiness(self):
        self.assertIsNotNone(probe, "actual image diagnostic is missing")
        original = matplotlib_spec()
        original.instance_image_key = "fixture-instance"
        original.base_image_key = "fixture-base"
        original.env_image_key = "fixture-env"
        original.platform = "linux/amd64"
        original.eval_script_list = [
            "source /opt/miniconda3/bin/activate", "conda activate testbed", "cd /testbed",
            "git config --global --add safe.directory /testbed", "git apply SECRET-GOLD",
            ": '>>>>> Start Test Output'", "pytest SECRET-TEST", ": '>>>>> End Test Output'",
        ]
        before = copy.deepcopy(original)
        image = SimpleNamespace(id="sha256:" + "a" * 64)
        built = []
        def build(client, specs, **kwargs):
            built.extend(specs)
            self.assertEqual(kwargs, dict(force_rebuild=False, max_workers=1,
                                          tag="latest", env_image_tag="latest"))
            self.assertEqual(specs[0].eval_script_list, before.eval_script_list)
            self.assertNotEqual(specs[0].env_script_list, before.env_script_list)
            return [specs[0]], []
        def run(command, **kwargs):
            self.assertIn("--network", command)
            self.assertEqual(command[command.index("--network") + 1], "none")
            self.assertIn("--memory", command)
            self.assertNotIn("SECRET", str(command))
            return subprocess.CompletedProcess(command, 1, stdout="FAILED public_test\n", stderr="")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with mock.patch.object(smoke, "build_prepared_task_image", return_value={
                "tag": "fixture-prepared", "image_id": image.id}), \
                 mock.patch.object(smoke, "capture_dependency_manifest", return_value={"package_count": 3}), \
                 mock.patch.object(smoke, "_clone") as clone, \
                 mock.patch.object(smoke.subprocess, "run", side_effect=run):
                report = probe.exercise_image(
                    record=dict(probe.TASK), original=original,
                    client=SimpleNamespace(images=SimpleNamespace(get=lambda key: image)),
                    build_instances=build, parser=lambda log, spec: {"public_test": "FAILED"},
                    public_command="pytest -rA", evidence=root / "evidence",
                    work=root / "work", source=root, memory_bytes=1024**3,
                )
                clone.assert_called_once_with(probe.TASK["repo"], probe.TASK["base_commit"], root / "work/repo")
            self.assertEqual(report["status"], "image-readiness-validated")
            self.assertEqual(report["readiness"]["parsed_test_count"], 1)
            self.assertEqual(report["readiness"]["baseline_exit_code"], 1)
            self.assertEqual(len(built), 1)
            self.assertEqual(original, before)
            evidence = "\n".join(path.read_text() for path in (root / "evidence").rglob("*") if path.is_file())
            self.assertNotIn("SECRET", evidence)
            self.assertFalse((root / "evidence/catalog.json").exists())


    def test_sdk_build_capacity_and_log_overflow_fail_closed(self):
        self.assertTrue(hasattr(probe, "install_build_limits"), "SDK capacity boundary is missing")
        calls = []
        def build(**kwargs):
            calls.append(kwargs)
            yield {"stream": "ok"}
            yield {"stream": "x" * (probe.solver.LOG_LIMIT + 1)}
        client = SimpleNamespace(api=SimpleNamespace(build=build))
        probe.install_build_limits(client, 512 * 1024**2)
        stream = client.api.build(platform="linux/amd64", tag="fixture", forcerm=True)
        self.assertEqual(next(stream), {"stream": "ok"})
        with self.assertRaisesRegex(RuntimeError, "log budget"):
            next(stream)
        self.assertEqual(calls[0]["container_limits"],
                         {"memory": 512 * 1024**2, "memswap": 512 * 1024**2})
        self.assertTrue(calls[0]["forcerm"])

    def test_missing_daemon_memory_support_blocks_image_execution(self):
        import sys
        for flags in ({}, {"MemoryLimit": False, "SwapLimit": True},
                      {"MemoryLimit": True, "SwapLimit": False}):
            with self.subTest(flags=flags), tempfile.TemporaryDirectory() as directory, \
                 mock.patch.object(probe.solver, "memory_snapshot", return_value={
                     "MemTotal_bytes": 16 * 1024**3, "MemAvailable_bytes": 12 * 1024**3}), \
                 mock.patch.object(probe.shutil, "disk_usage", return_value=SimpleNamespace(free=40 * 1024**3)):
                root = Path(directory)
                def docker(command, **kwargs):
                    return subprocess.CompletedProcess(command, 0,
                        "" if command[1:3] == ["ps", "-aq"] else json.dumps(flags), "")
                status = probe.run_probe(root / "evidence", root / "work", execute=docker,
                    worker_command=[sys.executable, "-c", "print('must-not-start')"], timeout_seconds=3)
                report = json.loads((root / "evidence/result.json").read_text())
                self.assertEqual(status, 1)
                self.assertNotIn("process", report, "unbounded Docker stage must not start")
                self.assertIn("memory/swap", report["error"])

    def test_supervisor_retains_failure_and_kills_only_job_containers(self):
        self.assertTrue(hasattr(probe, "run_probe"), "bounded image supervisor is missing")
        import sys
        calls = []
        inventories = iter(["", "a" * 64 + "\n", ""])
        def docker(command, **kwargs):
            calls.append(command)
            output = next(inventories) if command[1:3] == ["ps", "-aq"] else '{"MemoryLimit": true, "SwapLimit": true}'
            return subprocess.CompletedProcess(command, 0, output, "")
        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.object(probe.solver, "memory_snapshot", return_value={
                 "MemTotal_bytes": 16 * 1024**3, "MemAvailable_bytes": 12 * 1024**3}), \
             mock.patch.object(probe.shutil, "disk_usage", return_value=SimpleNamespace(free=40 * 1024**3)):
            root = Path(directory)
            status = probe.run_probe(root / "evidence", root / "work", execute=docker,
                worker_command=[sys.executable, "-c", "import sys; print('synthetic-build-failure'); sys.exit(47)"],
                timeout_seconds=3)
            report = json.loads((root / "evidence/result.json").read_text())
            self.assertEqual(status, 1)
            self.assertFalse(report["environment_ready"])
            self.assertEqual(report["process"]["returncode"], 47)
            self.assertIn("synthetic-build-failure", (root / "evidence/stdout.log").read_text())
            self.assertTrue(report["cleanup_verified"])
            self.assertIn(["docker", "rm", "-f", "a" * 64], calls)
            self.assertTrue((root / "evidence/artifact-index.json").is_file())


    def test_installed_harness_stage_uses_pinned_metadata_and_production_base(self):
        self.assertTrue(hasattr(probe, "run_image_stage"), "real harness stage is missing")
        try:
            import datasets
            import docker
            from swebench.harness import docker_build, dockerfiles
            from swebench.harness.test_spec import python as recipes
        except ImportError:
            self.skipTest("optional pinned SWE-bench harness is not installed")
        record = dict(probe.TASK, test_patch="", FAIL_TO_PASS=[], PASS_TO_PASS=[])
        data = (Path(__file__).parent / "fixtures/matplotlib-24627-environment.yml").read_text()
        image = SimpleNamespace(id="sha256:" + "a" * 64, attrs={"Architecture": "amd64", "Os": "linux"})
        client = SimpleNamespace(api=SimpleNamespace(build=lambda **kw: iter(())),
                                 images=SimpleNamespace(get=lambda key: image), close=lambda: None)
        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.object(datasets, "load_dataset", return_value=[record]) as load, \
             mock.patch.object(docker, "from_env", return_value=client), \
             mock.patch.object(recipes, "get_environment_yml", return_value=data[:-1]), \
             mock.patch.dict(dockerfiles._DOCKERFILE_BASE, dict(dockerfiles._DOCKERFILE_BASE)), \
             mock.patch.object(probe, "exercise_image", return_value={"status": "image-readiness-validated"}) as exercise, \
             mock.patch.object(probe, "bounded_execute", return_value=lambda *a, **kw: subprocess.CompletedProcess(a, 0, "glibc fixture\n", "")):
            root = Path(directory)
            probe.run_image_stage(str(root / "evidence"), str(root / "work"), str(1024**3))
            load.assert_called_once_with(smoke.DATASET, split="test", revision=smoke.DATASET_REVISION, token=False)
            self.assertEqual(exercise.call_args.kwargs["record"], record)
            self.assertIs(exercise.call_args.kwargs["build_instances"], docker_build.build_instance_images)
            self.assertEqual(exercise.call_args.kwargs["original"].repo_script_list,
                             matplotlib_spec().repo_script_list)
            inputs = json.loads((root / "evidence/inputs.json").read_text())
            self.assertEqual(inputs["task"], probe.TASK)
            self.assertEqual(inputs["swebench_version"], "4.1.0")
            self.assertEqual(inputs["environment_sha256"], probe.solver.ENVIRONMENT_SHA256)
            self.assertTrue((root / "evidence/base.Dockerfile").is_file())
            self.assertTrue((root / "evidence/image-identities.json").is_file())


    def test_missing_official_image_fails_before_readiness_even_with_empty_failure_list(self):
        original = matplotlib_spec()
        original.instance_image_key = "missing-fixture-image"
        client = SimpleNamespace(images=SimpleNamespace(get=mock.Mock(side_effect=RuntimeError("missing image"))))
        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.object(smoke, "build_prepared_task_image") as prepared:
            root = Path(directory)
            with self.assertRaisesRegex(RuntimeError, "missing image"):
                probe.exercise_image(record=dict(probe.TASK), original=original, client=client,
                    build_instances=lambda *a, **kw: ([], []), parser=lambda *a: {},
                    public_command="pytest -rA", evidence=root / "evidence", work=root / "work",
                    source=root, memory_bytes=1024**3)
            prepared.assert_not_called()
            report = json.loads((root / "evidence/image-result.json").read_text())
            self.assertEqual(report["status"], "failed")
            self.assertFalse(report["environment_ready"])
            self.assertEqual(report["stage"], "official-image-build")

    def test_supervisor_timeout_scrubs_credentials_and_rejects_fake_success_exit(self):
        import os
        import sys
        def docker(command, **kwargs):
            return subprocess.CompletedProcess(command, 0, "" if command[1:3] == ["ps", "-aq"] else
                                               '{"MemoryLimit": true, "SwapLimit": true}', "")
        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.object(probe.solver, "memory_snapshot", return_value={
                 "MemTotal_bytes": 16 * 1024**3, "MemAvailable_bytes": 12 * 1024**3}), \
             mock.patch.object(probe.shutil, "disk_usage", return_value=SimpleNamespace(free=40 * 1024**3)), \
             mock.patch.dict(os.environ, {"OPENAI_API_KEY": "synthetic-secret", "AWS_ACCESS_KEY_ID": "synthetic-secret"}):
            root = Path(directory)
            for name, code in (("timeout", "import time; time.sleep(60)"),
                               ("empty", "import os; print(sorted(os.environ)); assert 'OPENAI_API_KEY' not in os.environ; assert 'AWS_ACCESS_KEY_ID' not in os.environ")):
                with self.subTest(name=name):
                    status = probe.run_probe(root / name, root / (name + "-work"), execute=docker,
                        worker_command=[sys.executable, "-c", code], timeout_seconds=0.2)
                    self.assertEqual(status, 1)
                    report = json.loads((root / name / "result.json").read_text())
                    self.assertFalse(report["environment_ready"])
                    self.assertEqual(report["process"]["timed_out"], name == "timeout")
                    self.assertNotIn("synthetic-secret", (root / name / "stdout.log").read_text())

    def test_evidence_index_bounds_files_without_following_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evidence = root / "evidence"
            evidence.mkdir()
            (root / "private").write_text("outside-secret")
            (evidence / "link").symlink_to(root / "private")
            (evidence / "large.log").write_bytes(b"x" * (probe.solver.LOG_LIMIT + 1))
            self.assertTrue(probe.index_evidence(evidence))
            index = json.loads((evidence / "artifact-index.json").read_text())
            self.assertTrue(index["truncated"])
            self.assertEqual(index["total_bytes"], probe.solver.LOG_LIMIT)
            self.assertFalse((evidence / "link").exists())
            self.assertEqual((root / "private").read_text(), "outside-secret")
            self.assertTrue(probe.index_evidence(evidence), "reindex must preserve truncation evidence")
            refreshed = json.loads((evidence / "artifact-index.json").read_text())
            self.assertEqual(refreshed["files"][0]["original_size_bytes"], probe.solver.LOG_LIMIT + 1)

    def test_cli_refuses_local_real_build_before_creating_evidence(self):
        import os
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = dict(os.environ, GITHUB_ACTIONS="false", RUNNER_ENVIRONMENT="self-hosted")
            result = subprocess.run([sys.executable, str(Path(probe.__file__)),
                "--evidence-dir", str(root / "evidence"), "--work-dir", str(root / "work")],
                env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertFalse((root / "evidence").exists())

    def test_hosted_cli_rejects_mismatched_checkout_owner_before_build(self):
        import os
        import sys
        with mock.patch.dict(os.environ, GITHUB_ACTIONS="true", RUNNER_ENVIRONMENT="github-hosted"), \
             mock.patch.object(probe.os, "geteuid", return_value=1001), \
             mock.patch.object(sys, "argv", ["probe", "--evidence-dir", "unused", "--work-dir", "unused-work"]), \
             mock.patch.object(probe, "run_probe", return_value=0) as stage:
            with self.assertRaises(SystemExit) as failure:
                probe.main()
            self.assertEqual(failure.exception.code, 2)
            stage.assert_not_called()

    def test_workflow_outcome_writer_handles_restricted_image_evidence(self):
        import os
        try:
            import yaml
        except ImportError:
            self.skipTest("PyYAML unavailable; pinned-harness CI executes workflow fixture")
        workflow = yaml.load((Path(__file__).parent.parent / ".github/workflows/ci.yml").read_text(), Loader=yaml.BaseLoader)
        step = next(s for s in workflow["jobs"]["preparation-solver-probe"]["steps"]
                    if s.get("name") == "Record job outcome even if checkout or tests failed")
        for stage in ("solve", "image"):
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                evidence = root / "matplotlib-solver-evidence"
                evidence.mkdir()
                (evidence / "result.json").write_text('{}')
                # Emulate the denied non-owner write without requiring local root.
                evidence.chmod(0o555 if stage == "image" else 0o755)
                sudo = root / "sudo"
                sudo.write_text('#!/bin/bash\nset -eu\n'
                    '[[ "$1" == --non-interactive ]]\nshift\n'
                    '[[ "$1" == --preserve-env=RUNNER_TEMP,SOLVER_STEP_OUTCOME,PREPARATION_PROBE_STAGE,GITHUB_SHA ]]\n'
                    'shift\nchmod u+w "$RUNNER_TEMP/matplotlib-solver-evidence"\nexec "$@"\n')
                sudo.chmod(0o700)
                try:
                    result = subprocess.run(["bash", "-eu", "-c", step["run"]], capture_output=True, text=True,
                        env=dict(os.environ, PATH=str(root)+os.pathsep+os.environ["PATH"], RUNNER_TEMP=str(root),
                                 SOLVER_STEP_OUTCOME="failure", PREPARATION_PROBE_STAGE=stage, GITHUB_SHA="a"*40))
                    self.assertEqual(result.returncode, 0, result.stderr)
                    saved = json.loads((evidence / "job-outcome.json").read_text())
                    self.assertEqual(saved, {"solver_step_outcome": "failure", "preparation_probe_stage": stage,
                                            "github_sha": "a"*40, "result_present": True})
                finally:
                    evidence.chmod(0o755)

    def test_workflow_stage_dispatch_executes_only_selected_diagnostic(self):
        try:
            import yaml
        except ImportError:
            self.skipTest("PyYAML unavailable; pinned-harness CI executes workflow fixture")
        import os
        workflow = yaml.load((Path(__file__).parent.parent / ".github/workflows/ci.yml").read_text(), Loader=yaml.BaseLoader)
        inputs = workflow["on"]["workflow_dispatch"]["inputs"]
        self.assertIn("preparation_probe_stage", inputs, "image stage choice is missing")
        self.assertEqual(inputs["preparation_probe_stage"]["default"], "solve")
        self.assertEqual(inputs["preparation_solver_probe"]["default"], "false")
        steps = workflow["jobs"]["preparation-solver-probe"]["steps"]
        command = next(step["run"] for step in steps if step.get("id") == "solver")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "swebench-image-probe/bin").mkdir(parents=True)
            for path in (root / "python3", root / "swebench-image-probe/bin/python"):
                path.write_text('#!/bin/bash\nset -eu\n'
                                'if [[ "$0" == */swebench-image-probe/bin/python ]]; then '
                                '[[ "${PROBE_TEST_SUDO:-0}" == 1 ]] || exit 97; fi\n'
                                'printf "%s\\n" "$*" >> "$CALLS"\n')
                path.chmod(0o700)
            sudo = root / "sudo"
            sudo.write_text('#!/bin/bash\nset -eu\n'
                            '[[ "$1" == --non-interactive ]]\nshift\n'
                            '[[ "$1" == --preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,GITHUB_SHA,GITHUB_RUN_ID ]]\n'
                            'shift\nexport PROBE_TEST_SUDO=1\nexec "$@"\n')
            sudo.chmod(0o700)
            for stage, script in (("solve", "probe_matplotlib_solver.py"), ("image", "probe_matplotlib_preparation.py"), ("bad", None)):
                calls = root / (stage + "-calls")
                result = subprocess.run(["bash", "-eu", "-c", command], capture_output=True, text=True,
                    env=dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"],
                             RUNNER_TEMP=str(root), PREPARATION_PROBE_STAGE=stage, CALLS=str(calls)))
                if script:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    actual = calls.read_text().splitlines()
                    self.assertEqual(len(actual), 1)
                    self.assertEqual(actual[0].split()[0], "scripts/" + script)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(calls.exists())


if __name__ == "__main__":
    unittest.main()
