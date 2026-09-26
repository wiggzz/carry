#!/usr/bin/env python3
"""Behavior tests for the executable protected-worker benchmark."""
import concurrent.futures
import contextlib
import copy
import hashlib
import importlib.util
import itertools
import json
import os
import pathlib
import shutil
import shlex
import subprocess
import sys
import tempfile
import types
import unittest
import yaml
import zlib
from unittest import mock

from scripts import swebench_preparation_compat as compat


SCRIPT = pathlib.Path(__file__).with_name("swebench_smoke.py")


class RecipeSpec(types.SimpleNamespace):
    """Offline TestSpec fixture with upstream 4.1.0's computed image keys."""
    def __init__(self, **kwargs):
        super().__init__(namespace=None, instance_image_tag="latest",
                         env_script_list=["conda activate testbed", f"echo {kwargs.get('instance_id', '')}"],
                         repo_script_list=["python -m pip install -e ."], **kwargs)

    @property
    def instance_image_key(self):
        key = f"sweb.eval.x86_64.{self.instance_id.lower()}:{self.instance_image_tag}"
        return f"{self.namespace}/{key}".replace("__", "_1776_") if self.namespace else key

    @property
    def env_image_key(self):
        digest = hashlib.sha256(str(self.env_script_list).encode()).hexdigest()[:22]
        return f"sweb.env.py.x86_64.{digest}:latest"


class SmokeWorkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("swebench_smoke", SCRIPT)
        cls.worker = importlib.util.module_from_spec(spec)
        assert spec.loader
        spec.loader.exec_module(cls.worker)

    def test_config_requires_pinned_images_and_versions(self):
        valid = {
            "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CODEX_VERSION": "1.2.3", "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna", "REASONING": "medium",
            "TASK_IMAGE_REPOSITORY": "registry.example/tasks",
            "TASK_IMAGE_CATALOG": "registry.example/tasks@sha256:" + "f" * 64,
        }
        config = self.worker.validate_config(valid)
        self.assertEqual(config["PI_VERSION"], "0.84.2")
        self.assertEqual(config["CARRY_COMPACTION_POLICY"], "economic")
        self.assertEqual(config["CARRY_LEASE_REVIEW_POLICY"], "baseline")
        self.assertEqual(self.worker.validate_config(dict(valid, CARRY_LEASE_REVIEW_POLICY="batch-ordinary"))["CARRY_LEASE_REVIEW_POLICY"], "batch-ordinary")
        self.assertEqual(config["CARRY_COMPACTION_PAYOFF_REQUESTS"], "1")
        self.assertEqual(config["CARRY_COMPACTION_MIN_PAYBACK_PERCENT"], "25")
        self.assertEqual(config["CARRY_COMPACTION_ROLLOUT_STOP_PROBABILITY_PERCENT"], "10")
        self.assertEqual(config["CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS"], "0")
        self.assertEqual(config["CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS"], "0")
        configured_budget = self.worker.validate_config(
            dict(valid, CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS="32768", CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS="24576")
        )
        self.assertEqual(configured_budget["CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS"], "32768")
        self.assertEqual(configured_budget["CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS"], "24576")
        with self.assertRaises(ValueError):
            self.worker.validate_config(dict(valid, CARRY_COMPACTION_ROLLOUT_SAMPLES="16"))
        self.assertNotIn("CARRY_COMPACTION_ROLLOUT_SAMPLES", config)
        for key, value in (("BASE_IMAGE", "node:22"), ("CODEX_VERSION", "latest"),
                           ("CARRY_COMPACTION_POLICY", "adaptive"),
                           ("CARRY_LEASE_REVIEW_POLICY", "unexpected"),
                           ("CARRY_COMPACTION_MIN_PAYBACK_PERCENT", "101"),
                           ("CARRY_COMPACTION_ROLLOUT_SAMPLES", "65")):
            bad = dict(valid)
            bad[key] = value
            with self.assertRaises(ValueError):
                self.worker.validate_config(bad)
        with self.assertRaises(ValueError):
            self.worker.validate_config(
                dict(valid, CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS="0", CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS="1")
            )

    def test_workflow_defaults_neutral_watermarks_to_zero(self):
        workflow = pathlib.Path(__file__).parents[1] / ".github" / "workflows" / "run-swebench.yml"
        contents = yaml.safe_load(workflow.read_text(encoding="utf-8"))
        inputs = contents[True]["workflow_dispatch"]["inputs"]
        self.assertEqual(inputs["carry_compaction_neutral_high_watermark_tokens"]["default"], "0")
        self.assertEqual(inputs["carry_compaction_neutral_low_watermark_tokens"]["default"], "0")
        self.assertEqual(inputs["carry_compaction_min_payback_percent"]["default"], "25")
        self.assertEqual(inputs["carry_lease_review_policy"]["default"], "baseline")
        self.assertEqual(inputs["carry_lease_review_policy"]["options"], ["baseline", "batch-ordinary"])
        self.assertNotIn("carry_compaction_rollout_samples", inputs)
        self.assertEqual(contents["jobs"]["bootstrap-worker"]["env"]["CARRY_LEASE_REVIEW_POLICY"],
                         "${{ inputs.carry_lease_review_policy }}")

    def test_workflow_benchmark_model_input_controls_protected_worker(self):
        workflow = pathlib.Path(__file__).parents[1] / ".github" / "workflows" / "run-swebench.yml"
        contents = yaml.safe_load(workflow.read_text(encoding="utf-8"))
        dispatched = contents[True]["workflow_dispatch"]["inputs"]["model"]
        self.assertEqual(dispatched["default"], "gpt-6-luna")
        self.assertTrue(dispatched["required"])
        worker_model = contents["jobs"]["bootstrap-worker"]["env"]["MODEL"]
        self.assertEqual(worker_model, "${{ inputs.model }}")
        reasoning = contents[True]["workflow_dispatch"]["inputs"]["reasoning"]
        self.assertEqual(reasoning["default"], "medium")
        self.assertEqual(contents["jobs"]["bootstrap-worker"]["env"]["REASONING"],
                         "${{ inputs.reasoning }}")

    def test_workflow_stages_lease_review_treatment_for_the_worker(self):
        workflow = pathlib.Path(__file__).parents[1] / ".github" / "workflows" / "run-swebench.yml"
        contents = yaml.safe_load(workflow.read_text(encoding="utf-8"))
        stage = next(step for step in contents["jobs"]["bootstrap-worker"]["steps"]
                     if step.get("name") == "Stage run-scoped inputs and output capability")
        script = stage["run"]
        fragment = script[script.index('SOURCE_URL="$SOURCE_URL" KEY_URL='):]
        fragment = fragment[:fragment.index("\nPY\n") + len("\nPY\n")]
        inputs = (
            "SOURCE_URL", "KEY_URL", "DOCKER_AUTH_URL", "REGISTRY_AUTH_URL", "RESULT_URL", "CONTROL_URL",
            "SOURCE_SHA256", "SOURCE_COMMIT", "BENCHMARK_MODE", "BENCHMARK_HARNESS", "BENCHMARK_ATTEMPT",
            "BENCHMARK_ATTEMPTS", "CARRY_COMPACTION_POLICY", "CARRY_KEEP_LEASE_TURNS",
            "CARRY_COMPACTION_PAYOFF_REQUESTS", "CARRY_COMPACTION_MIN_PAYBACK_PERCENT",
            "CARRY_COMPACTION_ROLLOUT_STOP_PROBABILITY_PERCENT",
            "CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS", "CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS",
            "BOOTSTRAP_WAIT_SECONDS", "RUN_ID", "MODEL", "REASONING", "TASK_IMAGE_REPOSITORY",
            "TASK_IMAGE_CATALOG",
        )
        env = dict(os.environ, **dict.fromkeys(inputs, "fixture"))
        env.pop("CARRY_LEASE_REVIEW_POLICY", None)
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "worker-bootstrap.env"
            env["BOOTSTRAP_CONFIG_FILE"] = str(path)
            result = subprocess.run(["bash", "-euo", "pipefail", "-c",
                                     "CARRY_LEASE_REVIEW_POLICY=batch-ordinary\n" + fragment],
                                    env=env, text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            sourced = subprocess.run(["bash", "-euo", "pipefail", "-c",
                                      'source "$1"; printf "%s" "$CARRY_LEASE_REVIEW_POLICY"', "_", str(path)],
                                     text=True, capture_output=True, check=False)
            self.assertEqual(sourced.returncode, 0, sourced.stderr)
            self.assertEqual(sourced.stdout, "batch-ordinary")

    def test_proxy_round_usage_records_maximum_and_non_monotonic_inputs(self):
        log = "noise\nBENCHMARK_PROXY_USAGE {\"input_tokens\": 120}\nBENCHMARK_PROXY_USAGE {\"input_tokens\": 90}\nBENCHMARK_PROXY_USAGE {\"input_tokens\": 180}\n"
        execute = mock.Mock(return_value=types.SimpleNamespace(stdout=log))
        rounds = self.worker.load_proxy_round_input_tokens("isolated-proxy", execute=execute)
        self.assertEqual(rounds, [120, 90, 180])
        self.assertEqual(self.worker.max_observed_input_tokens(rounds), 180)

    def test_codex_thread_id_accepts_the_native_uuidv7_emitted_by_codex(self):
        with tempfile.TemporaryDirectory() as directory:
            trace = pathlib.Path(directory) / "trace.log"
            trace.write_text(
                '{"type":"thread.started","thread_id":"01a0488e-3e27-7ff0-9d61-b286f5ec1213"}\n',
                encoding="utf-8",
            )
            self.assertEqual(
                self.worker.codex_thread_id(trace),
                "01a0488e-3e27-7ff0-9d61-b286f5ec1213",
            )

    def test_selected_harness_defaults_to_carry_and_rejects_unknown(self):
        self.assertEqual(self.worker.selected_harnesses({}), ("carry",))
        for harness in ("carry", "codex", "pi"):
            with self.subTest(harness=harness):
                self.assertEqual(
                    self.worker.selected_harnesses({"BENCHMARK_HARNESS": harness}),
                    (harness,),
                )
        self.assertEqual(
            self.worker.selected_harnesses({"BENCHMARK_HARNESS": "all"}),
            ("carry", "codex", "pi"),
        )
        with self.assertRaisesRegex(ValueError, "BENCHMARK_HARNESS"):
            self.worker.selected_harnesses({"BENCHMARK_HARNESS": "unknown"})

    def test_run_cli_forwards_the_selected_harness(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            argv = [
                "swebench_smoke.py",
                "--run",
                "--source",
                str(root / "source"),
                "--work",
                str(root / "work"),
                "--output",
                str(root / "output"),
                "--harness",
                "pi",
            ]
            with mock.patch.object(sys, "argv", argv), mock.patch.object(
                self.worker, "execute_benchmark"
            ) as execute:
                self.assertEqual(self.worker.main(), 0)
            self.assertEqual(execute.call_args.kwargs["config"]["BENCHMARK_HARNESS"], "pi")

    def test_prepare_cli_uses_publisher_without_model_runner(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            argv = [
                "swebench_smoke.py", "--prepare-images",
                "--source", str(root / "source"),
                "--work", str(root / "work"),
                "--output", str(root / "output"),
            ]
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(self.worker, "execute_preparation") as prepare, \
                    mock.patch.object(self.worker, "execute_benchmark") as benchmark:
                self.assertEqual(self.worker.main(), 0)
            prepare.assert_called_once()
            benchmark.assert_not_called()

    def assert_agent_mounts(self, command, root, extra=()):
        mounts = [command[index + 1] for index, arg in enumerate(command) if arg == "--mount"]
        expected = [
            f"type=bind,src={(root / 'repo').resolve()},dst=/testbed",
            f"type=bind,src={(root / 'harness').resolve()},dst=/opt/swebench-harness,readonly",
            f"type=bind,src={(root / 'input').resolve()},dst=/benchmark/input,readonly",
            f"type=bind,src={(root / 'output').resolve()},dst=/benchmark/output",
            *extra,
        ]
        self.assertCountEqual(mounts, expected)
        sources = {pathlib.Path(field.removeprefix("src=")).resolve()
                   for mount in mounts for field in mount.split(",") if field.startswith("src=")}
        self.assertTrue(sources.isdisjoint({pathlib.Path.home().resolve(),
                                           pathlib.Path("/var/run/docker.sock").resolve(),
                                           pathlib.Path("/run/docker.sock").resolve()}))
        self.assertNotIn("-v", command)
        self.assertNotIn("--volume", command)

    def test_agent_command_mounts_only_workspace_prompt_and_output_and_key_by_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output", "harness"):
                (root / name).mkdir()
            command = self.worker.agent_docker_command(
                image="smoke-codex:run", harness="codex", repo=root / "repo",
                harness_bundle=root / "harness",
                task_input=root / "input", output=root / "output", model="gpt-5.6-luna",
                reasoning="medium", container_name="carry-agent-codex-test",
                agent_timeout_seconds=315, network="carry-agent-internal-test",
                proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
            )
            rendered = "\n".join(command)
            self.assertIn("--env\nOPENAI_API_KEY", rendered)
            self.assertIn("--network\ncarry-agent-internal-test", rendered)
            self.assertIn("--dns\n127.0.0.1", rendered)
            self.assertIn("--add-host\nopenai-proxy:172.28.0.2", rendered)
            self.assertIn("OPENAI_BASE_URL=http://openai-proxy:8080/v1", rendered)
            self.assertNotIn("/var/run/docker.sock", rendered)
            self.assert_agent_mounts(command, root)
            self.assertEqual(rendered.count("type=bind"), 4)
            self.assertIn(f"src={(root / 'harness').resolve()},dst=/opt/swebench-harness,readonly", rendered)
            self.assertEqual(rendered.count("dst=/opt/swebench-harness,readonly"), 1)
            self.assertIn("dst=/testbed", rendered)
            self.assertIn("BENCHMARK_WORKSPACE=/testbed", rendered)
            self.assertIn("dst=/benchmark/input,readonly", rendered)
            self.assertIn("dst=/benchmark/output", rendered)
            self.assertIn("--harness\ncodex", rendered)
            self.assertIn("HOME=/agent-home", rendered)
            self.assertIn("AGENT_TIMEOUT_SECONDS=315", rendered)
            self.assertIn("--env\nCARRY_COMPACTION_POLICY", rendered)
            self.assertIn("--env\nCARRY_LEASE_REVIEW_POLICY", rendered)
            self.assertIn("carry-agent-codex-test", rendered)
            self.assertIn("/agent-home:rw", rendered)
            self.assertIn("/tmp:rw", rendered)

    def test_runner_supplies_canonical_agent_template_for_each_harness(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output", "harness"):
                (root / name).mkdir()
            for harness, template in self.worker.AGENT_COMMANDS.items():
                command = self.worker.agent_docker_command(
                    image=f"prepared-{harness}:immutable", harness=harness, repo=root / "repo",
                    harness_bundle=root / "harness", task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium", container_name=f"carry-agent-{harness}-template-test",
                    agent_timeout_seconds=315, network="carry-agent-internal-test",
                    proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                )
                env_values = [command[index + 1] for index, value in enumerate(command[:-1]) if value == "--env"]
                self.assertIn(f"AGENT_COMMAND={template}", env_values)

    def test_empty_keep_lease_is_not_forwarded_to_the_native_carry_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output", "harness"):
                (root / name).mkdir()
            with mock.patch.dict(os.environ, {"CARRY_KEEP_LEASE_TURNS": ""}):
                command = self.worker.agent_docker_command(
                    image="prepared-carry:immutable", harness="carry", repo=root / "repo",
                    harness_bundle=root / "harness", task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium", container_name="carry-agent-empty-lease-test",
                    agent_timeout_seconds=315, network="carry-agent-internal-test",
                    proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                )
        env_values = [command[index + 1] for index, value in enumerate(command[:-1]) if value == "--env"]
        self.assertNotIn("CARRY_KEEP_LEASE_TURNS", env_values)

    def test_session_carry_command_mounts_a_read_only_source_session_as_its_fifth_bind(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output", "harness", "session"):
                (root / name).mkdir()
            session = root / "session"
            command = self.worker.agent_docker_command(
                image="smoke-carry:run", harness="carry", repo=root / "repo",
                harness_bundle=root / "harness", task_input=root / "input", output=root / "output",
                model="gpt-5.6-luna", reasoning="medium", container_name="carry-agent-session-test",
                agent_timeout_seconds=315, network="carry-agent-internal-test",
                proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                resume_session=session,
            )
        rendered = "\n".join(command)
        self.assertEqual(rendered.count("type=bind"), 5)
        self.assertIn(f"src={session.resolve()},dst=/benchmark/session,readonly", rendered)
        self.assertIn("--resume-session\n/benchmark/session", rendered)
        self.assertNotIn("/var/run/docker.sock", rendered)
        self.assert_agent_mounts(command, root, (
            f"type=bind,src={session.resolve()},dst=/benchmark/session,readonly",
        ))
        with self.assertRaisesRegex(ValueError, "Carry"):
            self.worker.agent_docker_command(
                image="smoke-codex:run", harness="codex", repo=root / "repo",
                harness_bundle=root / "harness", task_input=root / "input", output=root / "output",
                model="gpt-5.6-luna", reasoning="medium", container_name="codex-session-test",
                agent_timeout_seconds=315, network="carry-agent-internal-test",
                proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                resume_session=session,
            )

    def test_pi_session_command_mounts_a_writable_worker_local_session_and_uses_native_flags(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output", "harness", "pi-session"):
                (root / name).mkdir()
            session_dir = root / "pi-session"
            command = self.worker.agent_docker_command(
                image="smoke-pi:run", harness="pi", repo=root / "repo",
                harness_bundle=root / "harness", task_input=root / "input", output=root / "output",
                model="gpt-5.6-luna", reasoning="medium", container_name="carry-agent-pi-session-test",
                agent_timeout_seconds=315, network="carry-agent-internal-test",
                proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                pi_session_dir=session_dir,
            )
        rendered = "\n".join(command)
        self.assertEqual(rendered.count("type=bind"), 5)
        self.assertIn(f"src={session_dir.resolve()},dst=/benchmark/pi-session", rendered)
        self.assertNotIn(f"src={session_dir.resolve()},dst=/benchmark/pi-session,readonly", rendered)
        self.assertIn("--pi-session-dir\n/benchmark/pi-session", rendered)
        self.assertNotIn("/var/run/docker.sock", rendered)
        self.assert_agent_mounts(command, root, (
            f"type=bind,src={session_dir.resolve()},dst=/benchmark/pi-session",
        ))

    def test_readiness_command_has_no_network_secret_or_evaluator_mounts(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = pathlib.Path(directory) / "repo"
            repo.mkdir()
            command = self.worker.readiness_docker_command(
                image="sha256:" + "a" * 64,
                container_name="carry-readiness-task",
                repo=repo,
                script="pytest -rA tests/test_public.py",
            )
        rendered = "\n".join(command)
        self.assertIn("--network\nnone", rendered)
        self.assertIn("--cap-drop=ALL", command)
        self.assertIn("no-new-privileges", command)
        self.assertIn("dst=/testbed", rendered)
        self.assertNotIn("OPENAI", rendered)
        self.assertNotIn("test_patch", rendered)
        self.assertNotIn("canonical-dataset", rendered)
        self.assertEqual(rendered.count("type=bind"), 1)

    def test_readiness_accepts_failing_baseline_after_tests_execute(self):
        result = self.worker.validate_readiness_result(
            returncode=1,
            timed_out=False,
            parsed_tests={"tests/test_public.py::test_bug": "FAILED", "optional": "SKIPPED"},
        )
        self.assertEqual(result["status"], "ready")
        self.assertEqual(result["parsed_test_count"], 2)
        self.assertEqual(result["executed_test_count"], 1)
        self.assertEqual(result["baseline_exit_code"], 1)

    def test_readiness_rejects_runner_that_never_executes_a_test(self):
        with self.assertRaisesRegex(RuntimeError, "did not execute any parseable public tests"):
            self.worker.validate_readiness_result(
                returncode=0,
                timed_out=False,
                parsed_tests={},
            )

    def test_readiness_rejects_skip_and_collection_error_only_results(self):
        for parsed in (
            {"test_optional.py": "SKIPPED"},
            {"test_import.py": "ERROR"},
            {"test_optional.py": "SKIPPED", "test_import.py": "ERROR"},
            {"test_unknown.py": "UNKNOWN"},
        ):
            with self.subTest(parsed=parsed), self.assertRaisesRegex(
                RuntimeError, "did not execute any parseable public tests"
            ):
                self.worker.validate_readiness_result(
                    returncode=1, timed_out=False, parsed_tests=parsed,
                )

    def test_matplotlib_collection_error_fails_readiness_with_official_parser(self):
        try:
            from swebench.harness.log_parsers import MAP_REPO_TO_PARSER
        except ImportError:
            self.skipTest("pinned SWE-bench harness unavailable")
        captured = (
            "collecting ... collected 1035 items / 1 error / 1 skipped\n"
            "SKIPPED [1] lib/matplotlib/tests/test_backend_macosx.py:10: These are mac only tests\n"
            "ERROR lib/matplotlib/tests/test_backend_nbagg.py - TypeError: unexpected keyword 'extra_items'\n"
            "!!!!!!!!!!!!!!!! stopping after 1 failures !!!!!!!!!!!!!!!!\n"
        )
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo = root / "repo"
            repo.mkdir()
            output = root / "evidence"
            with mock.patch.object(self.worker.subprocess, "run", return_value=mock.Mock(
                returncode=1, stdout=captured, stderr=""
            )), self.assertRaisesRegex(RuntimeError, "PASSED or FAILED required"):
                self.worker.run_task_readiness(
                    instance_id="matplotlib__matplotlib-24627", image="probe", repo=repo,
                    script="pytest -rA -vv --maxfail=1", test_command="pytest -rA -vv --maxfail=1",
                    parser=MAP_REPO_TO_PARSER["matplotlib/matplotlib"], test_spec=None,
                    output=output, timeout_seconds=180,
                )
            self.assertEqual(json.loads((output / "metadata.json").read_text())["status"], "not-ready")
            self.assertEqual((output / "test-output.txt").read_text(), captured)

    def test_readiness_strips_ansi_before_official_parser_but_preserves_raw_evidence(self):
        captured = "tests/test_public.py::test_ok \x1b[32mPASSED\x1b[0m [  6%]\n"
        parsed_inputs = []

        def parser(output, _spec):
            parsed_inputs.append(output)
            if output.strip() == "tests/test_public.py::test_ok PASSED":
                return {"tests/test_public.py::test_ok": "PASSED"}
            return {}

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo = root / "repo"
            repo.mkdir()
            output = root / "evidence"
            with mock.patch.object(self.worker.subprocess, "run", return_value=mock.Mock(
                returncode=124, stdout=captured, stderr=""
            )):
                result = self.worker.run_task_readiness(
                    instance_id="scikit-learn__scikit-learn-25102", image="probe", repo=repo,
                    script="pytest -rA -vv --maxfail=1", test_command="pytest -rA -vv --maxfail=1",
                    parser=parser, test_spec=None, output=output, timeout_seconds=180,
                )
            self.assertEqual(result["status"], "ready")
            self.assertEqual(result["executed_test_count"], 1)
            self.assertEqual(parsed_inputs, ["tests/test_public.py::test_ok PASSED\n"])
            self.assertEqual((output / "test-output.txt").read_text(), captured)

    def test_task_catalog_references_are_deterministic_and_content_addressed(self):
        record = {
            "instance_id": "owner__repo-1", "repo": "owner/repo",
            "version": "1.0", "base_commit": "a" * 40,
            "problem_statement": "Fix it", "test_patch": "hidden",
        }
        reordered = dict(reversed(list(record.items())))
        key = self.worker.task_image_cache_key(
            record, prepared_dockerfile_sha256="b" * 64,
            base_dockerfile_sha256="d" * 64,
        )
        self.assertEqual(
            key,
            self.worker.task_image_cache_key(
                reordered, prepared_dockerfile_sha256="b" * 64,
                base_dockerfile_sha256="d" * 64,
            ),
        )
        self.assertRegex(key, r"^[0-9a-f]{64}$")
        self.assertNotEqual(
            key,
            self.worker.task_image_cache_key(
                record, prepared_dockerfile_sha256="c" * 64,
                base_dockerfile_sha256="d" * 64,
            ),
        )
        self.assertNotEqual(
            key,
            self.worker.task_image_cache_key(
                record, prepared_dockerfile_sha256="b" * 64,
                base_dockerfile_sha256="e" * 64,
            ),
        )
        evaluator_only = dict(
            record,
            problem_statement="different prompt",
            patch="hidden gold patch",
            test_patch="different hidden evaluator patch",
        )
        self.assertEqual(
            key,
            self.worker.task_image_cache_key(
                evaluator_only, prepared_dockerfile_sha256="b" * 64,
                base_dockerfile_sha256="d" * 64,
            ),
        )
        self.assertEqual(
            self.worker.task_image_references("public.ecr.aws/example/tasks", key),
            {
                "evaluator": f"public.ecr.aws/example/tasks:swebench-evaluator-{key}",
                "agent": f"public.ecr.aws/example/tasks:swebench-ready-{key}",
            },
        )
        with self.assertRaisesRegex(ValueError, "TASK_IMAGE_REPOSITORY"):
            self.worker.task_image_references("", key)

    def test_compatibility_policy_change_invalidates_cache_and_frozen_catalog(self):
        record = dict(instance_id="owner__repo-1", repo="owner/repo", version="1.0", base_commit="a" * 40)
        repository = "registry.example/tasks"
        with tempfile.TemporaryDirectory() as directory:
            source = pathlib.Path(directory)
            for relative in (
                "containers/swebench-harness/Dockerfile.prepared",
                "containers/swebench-harness/prepared-entrypoint.sh",
                "containers/swebench-harness/apply-testbed-overlay.sh",
                "scripts/swebench_preparation_compat.py",
            ):
                destination = source / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(SCRIPT.parents[1] / relative, destination)
            before = self.worker.prepared_image_recipe_sha256(source)
            old_key = self.worker.task_image_cache_key(
                record, prepared_dockerfile_sha256=before, base_dockerfile_sha256="e" * 64,
            )
            catalog = self.worker.task_catalog_payload(
                published={record["instance_id"]: {
                    "cache_key": old_key, "preparation_compatibility": {},
                    "agent_image": {"resolved_digest": repository + "@sha256:" + "a" * 64},
                    "evaluator_image": {"resolved_digest": repository + "@sha256:" + "b" * 64},
                }}, repository=repository, prepared_recipe_sha256=before, base_recipe_sha256="e" * 64,
            )
            policy = source / "scripts/swebench_preparation_compat.py"
            policy.write_bytes(policy.read_bytes() + b"\n# policy revision\n")
            after = self.worker.prepared_image_recipe_sha256(source)
            self.assertNotEqual(before, after)
            new_key = self.worker.task_image_cache_key(
                record, prepared_dockerfile_sha256=after, base_dockerfile_sha256="e" * 64,
            )
            self.assertNotEqual(old_key, new_key)
            self.assertNotEqual(self.worker.task_image_references(repository, old_key),
                                self.worker.task_image_references(repository, new_key))
            with self.assertRaisesRegex(RuntimeError, "metadata"):
                self.worker.validate_task_catalog(
                    catalog=catalog, records=[record], repository=repository,
                    prepared_recipe_sha256=after, base_recipe_sha256="e" * 64,
                )

    def test_readiness_policy_change_invalidates_cache_and_frozen_catalog(self):
        source = SCRIPT.parents[1]
        record = dict(instance_id="owner__repo-1", repo="owner/repo", version="1.0", base_commit="a" * 40)
        repository = "registry.example/tasks"
        with mock.patch.object(self.worker, "READINESS_EXECUTED_STATUSES", ("PASSED", "FAILED", "ERROR", "SKIPPED")):
            before = self.worker.prepared_image_recipe_sha256(source)
        after = self.worker.prepared_image_recipe_sha256(source)
        self.assertNotEqual(before, after)
        old_key = self.worker.task_image_cache_key(record, prepared_dockerfile_sha256=before,
                                                   base_dockerfile_sha256="e" * 64)
        new_key = self.worker.task_image_cache_key(record, prepared_dockerfile_sha256=after,
                                                   base_dockerfile_sha256="e" * 64)
        self.assertNotEqual(old_key, new_key)
        catalog = self.worker.task_catalog_payload(
            published={record["instance_id"]: {
                "cache_key": old_key, "preparation_compatibility": {},
                "agent_image": {"resolved_digest": repository + "@sha256:" + "a" * 64},
                "evaluator_image": {"resolved_digest": repository + "@sha256:" + "b" * 64},
            }}, repository=repository, prepared_recipe_sha256=before, base_recipe_sha256="e" * 64,
        )
        with self.assertRaisesRegex(RuntimeError, "metadata"):
            self.worker.validate_task_catalog(catalog=catalog, records=[record], repository=repository,
                                             prepared_recipe_sha256=after, base_recipe_sha256="e" * 64)

    def test_frozen_catalog_validates_inputs_and_publishes_an_immutable_reference(self):
        record = {
            "instance_id": "owner__repo-1", "repo": "owner/repo",
            "version": "1.0", "base_commit": "a" * 40,
        }
        prepared_recipe = self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1])
        base_recipe = "e" * 64
        cache_key = self.worker.task_image_cache_key(
            record, prepared_dockerfile_sha256=prepared_recipe,
            base_dockerfile_sha256=base_recipe,
        )
        repository = "public.ecr.aws/example/tasks"
        published = {record["instance_id"]: {
            "cache_key": cache_key, "preparation_compatibility": {},
            "evaluator_image": {"resolved_digest": repository + "@sha256:" + "a" * 64},
            "agent_image": {"resolved_digest": repository + "@sha256:" + "b" * 64},
        }}
        catalog = self.worker.task_catalog_payload(
            published=published, repository=repository,
            prepared_recipe_sha256=prepared_recipe,
            base_recipe_sha256=base_recipe,
        )
        self.worker.validate_task_catalog(
            catalog=catalog, records=[record], repository=repository,
            prepared_recipe_sha256=prepared_recipe,
            base_recipe_sha256=base_recipe,
        )
        superset = json.loads(json.dumps(catalog))
        superset["tasks"]["unused__task-2"] = superset["tasks"][record["instance_id"]]
        normalized = self.worker.validate_task_catalog(
            catalog=superset, records=[record], repository=repository,
            prepared_recipe_sha256=prepared_recipe,
            base_recipe_sha256=base_recipe,
        )
        self.assertEqual(set(normalized["tasks"]), {record["instance_id"]})
        with self.assertRaisesRegex(RuntimeError, "metadata"):
            self.worker.validate_task_catalog(
                catalog=dict(catalog, dataset_revision="0" * 40), records=[record],
                repository=repository, prepared_recipe_sha256=prepared_recipe,
                base_recipe_sha256=base_recipe,
            )

        calls = []
        digest_reference = repository + "@sha256:" + "c" * 64
        def execute(command, **kwargs):
            calls.append(command)
            if command[:3] == ["docker", "image", "inspect"]:
                payload = {
                    "Id": "sha256:" + "d" * 64,
                    "RepoDigests": [digest_reference],
                    "Config": {"Labels": {}},
                }
                return mock.Mock(stdout=json.dumps(payload))
            return mock.Mock(returncode=0, stdout="")

        with tempfile.TemporaryDirectory() as directory:
            reference = self.worker.publish_task_catalog_image(
                catalog=catalog, repository=repository,
                output=pathlib.Path(directory), execute=execute,
            )
        self.assertEqual(reference, digest_reference)
        self.assertTrue(any(command[:2] == ["docker", "build"] for command in calls))
        self.assertTrue(any(command[:2] == ["docker", "push"] for command in calls))

    def test_prepared_image_is_harness_neutral_and_uses_one_immutable_parent(self):
        calls = []

        def execute(command, **kwargs):
            calls.append(command)
            if command[:3] == ["docker", "image", "inspect"]:
                reference = command[-1]
                value = "a" if reference.endswith("parent-task-image") else "e"
                return mock.Mock(stdout="sha256:" + value * 64 + "\n")
            return mock.Mock(returncode=0, stdout="")

        result = self.worker.build_prepared_task_image(
            source=SCRIPT.parent.parent,
            run_id="run-1",
            instance_id="owner__repo-1",
            task_image_id="sha256:" + "a" * 64,
            cache_key="c" * 64,
            execute=execute,
        )
        build = next(command for command in calls if command[:2] == ["docker", "build"])
        rendered = "\n".join(build)
        tag_commands = [command for command in calls if command[:3] == ["docker", "image", "tag"]]
        self.assertEqual(tag_commands, [[
            "docker", "image", "tag", "sha256:" + "a" * 64,
            "swebench-run-1-prepared-owner__repo-1-parent-task-image",
        ]])
        self.assertIn("TASK_IMAGE=swebench-run-1-prepared-owner__repo-1-parent-task-image", rendered)
        self.assertIn("TASK_CACHE_KEY=" + "c" * 64, rendered)
        self.assertNotIn("HARNESS_IMAGE", rendered)
        self.assertEqual(result["image_id"], "sha256:" + "e" * 64)
        self.assertEqual(result["tag"], "swebench-run-1-prepared-owner__repo-1")
        self.assertNotIn("harness", result)

    def test_pull_only_resolver_validates_pair_and_retags_evaluator_for_swebench(self):
        record = {
            "instance_id": "owner__repo-1", "repo": "owner/repo",
            "version": "1.0", "base_commit": "a" * 40,
        }
        dockerfile_hash = self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1])
        cache_key = self.worker.task_image_cache_key(
            record, prepared_dockerfile_sha256=dockerfile_hash,
            base_dockerfile_sha256="e" * 64,
        )
        refs = self.worker.task_image_references("public.ecr.aws/example/tasks", cache_key)
        evaluator_id = "sha256:" + "a" * 64
        agent_id = "sha256:" + "b" * 64
        evaluator_digest = "public.ecr.aws/example/tasks@sha256:" + "c" * 64
        agent_digest = "public.ecr.aws/example/tasks@sha256:" + "d" * 64
        catalog = self.worker.task_catalog_payload(
            published={record["instance_id"]: {
                "cache_key": cache_key, "preparation_compatibility": {},
                "evaluator_image": {"resolved_digest": evaluator_digest},
                "agent_image": {"resolved_digest": agent_digest},
            }},
            repository="public.ecr.aws/example/tasks",
            prepared_recipe_sha256=dockerfile_hash,
            base_recipe_sha256="e" * 64,
        )
        calls = []

        def inspect_payload(reference):
            if reference in {evaluator_digest, "official:owner__repo-1"}:
                return {
                    "Id": evaluator_id,
                    "RepoDigests": ["public.ecr.aws/example/tasks@sha256:" + "c" * 64],
                    "Config": {"Labels": {}},
                }
            return {
                "Id": agent_id,
                "RepoDigests": ["public.ecr.aws/example/tasks@sha256:" + "d" * 64],
                "Config": {"Labels": {
                    "org.carry.swebench.task-cache-key": cache_key,
                    "org.carry.swebench.evaluator-image-id": evaluator_id,
                }},
            }

        def execute(command, **kwargs):
            calls.append(command)
            if command[:3] == ["docker", "image", "inspect"]:
                if command[4] == "{{.Id}}":
                    return mock.Mock(stdout=evaluator_id + "\n")
                return mock.Mock(stdout=json.dumps(inspect_payload(command[-1])) + "\n")
            return mock.Mock(returncode=0, stdout="")

        specs = [types.SimpleNamespace(
            instance_id=record["instance_id"], instance_image_key="official:owner__repo-1",
        )]
        with tempfile.TemporaryDirectory() as directory:
            resolved = self.worker.resolve_task_environments(
                records=[record], source=SCRIPT.parents[1],
                repository="public.ecr.aws/example/tasks",
                output=pathlib.Path(directory), get_specs=lambda _records, **kwargs: specs,
                base_dockerfile_sha256="e" * 64,
                catalog=catalog,
                execute=execute,
            )
            persisted = json.loads((pathlib.Path(directory) / "preparation.json").read_text())
        item = resolved[record["instance_id"]]
        self.assertEqual(item["agent_image"]["image_id"], agent_id)
        self.assertEqual(item["evaluator_image"]["image_id"], evaluator_id)
        self.assertEqual(item["agent_image"]["resolved_digest"], "public.ecr.aws/example/tasks@sha256:" + "d" * 64)
        self.assertEqual(persisted, resolved)
        self.assertEqual(
            [command for command in calls if command[:2] == ["docker", "pull"]],
            [["docker", "pull", evaluator_digest], ["docker", "pull", agent_digest]],
        )
        self.assertIn(
            ["docker", "image", "tag", evaluator_digest, "official:owner__repo-1"],
            calls,
        )
        self.assertFalse(any(command[:2] == ["docker", "build"] for command in calls))

    def test_pull_only_resolver_fails_closed_on_mismatched_agent_parent(self):
        record = {"instance_id": "task-1", "repo": "owner/repo", "base_commit": "a" * 40}
        dockerfile_hash = self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1])
        key = self.worker.task_image_cache_key(
            record, prepared_dockerfile_sha256=dockerfile_hash,
            base_dockerfile_sha256="e" * 64,
        )
        refs = self.worker.task_image_references("registry.example/tasks", key)
        evaluator_digest = "registry.example/tasks@sha256:" + "c" * 64
        agent_digest = "registry.example/tasks@sha256:" + "d" * 64
        catalog = self.worker.task_catalog_payload(
            published={"task-1": {
                "cache_key": key, "preparation_compatibility": {},
                "evaluator_image": {"resolved_digest": evaluator_digest},
                "agent_image": {"resolved_digest": agent_digest},
            }},
            repository="registry.example/tasks",
            prepared_recipe_sha256=dockerfile_hash,
            base_recipe_sha256="e" * 64,
        )

        def execute(command, **kwargs):
            if command[:3] != ["docker", "image", "inspect"]:
                return mock.Mock(returncode=0, stdout="")
            reference = command[-1]
            payload = {
                "Id": "sha256:" + ("a" if reference == evaluator_digest else "b") * 64,
                "RepoDigests": [evaluator_digest if reference == evaluator_digest else agent_digest],
                "Config": {"Labels": {
                    "org.carry.swebench.task-cache-key": key,
                    "org.carry.swebench.evaluator-image-id": "sha256:" + "f" * 64,
                }},
            }
            return mock.Mock(stdout=json.dumps(payload))

        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(
            RuntimeError, "evaluator image identity"
        ):
            self.worker.resolve_task_environments(
                records=[record], source=SCRIPT.parents[1], repository="registry.example/tasks",
                output=pathlib.Path(directory),
                base_dockerfile_sha256="e" * 64,
                catalog=catalog,
                get_specs=lambda _records, **kwargs: [types.SimpleNamespace(
                    instance_id="task-1", instance_image_key="official:task-1",
                )],
                execute=execute,
            )

    def test_readiness_results_are_consumed_in_completion_order(self):
        import threading
        release = threading.Event()
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            slow = executor.submit(lambda: (release.wait(2), "slow")[1])
            fast = executor.submit(lambda: "fast")
            results = self.worker.fail_fast_completion_order([slow, fast])
            self.assertEqual(next(results), "fast")
            release.set()
            self.assertEqual(next(results), "slow")
            with self.assertRaises(StopIteration):
                next(results)

    def test_readiness_completion_cancels_pending_work_on_first_failure(self):
        failed = concurrent.futures.Future()
        failed.set_exception(RuntimeError("not ready"))
        pending = concurrent.futures.Future()
        with self.assertRaisesRegex(RuntimeError, "not ready"):
            next(self.worker.fail_fast_completion_order([failed, pending]))
        self.assertTrue(pending.cancelled())

    def test_readiness_script_excludes_hidden_test_patch_but_runs_public_test_command(self):
        spec = types.SimpleNamespace(eval_script_list=[
            "source /opt/miniconda3/bin/activate",
            "conda activate testbed",
            "cd /testbed",
            "export PUBLIC_TEST_MODE=1",
            "git config --global --add safe.directory /testbed",
            "git status",
            "python -m pip install -e .",
            "git checkout base tests/test_public.py",
            "git apply -v - <<'EOF'\nHIDDEN GOLD TEST\nEOF",
            ": '>>>>> Start Test Output'",
            "pytest -rA tests/test_public.py",
            ": '>>>>> End Test Output'",
        ])
        script, test_command = self.worker.trusted_readiness_script(
            spec, public_test_command="pytest -rA",
        )
        self.assertEqual(test_command, "pytest -rA -vv --maxfail=1")
        self.assertIn("export PUBLIC_TEST_MODE=1", script)
        self.assertNotIn("python -m pip install -e .", script)
        self.assertIn("pytest -rA", script)
        self.assertNotIn("tests/test_public.py", script)
        self.assertNotIn("HIDDEN GOLD TEST", script)
        self.assertNotIn("git apply", script)

    def test_sympy_readiness_executes_a_fixed_public_file_in_small_historical_suites(self):
        # Old SymPy split_list partitions FILES with floor division: fewer than
        # 500 files makes split 1/500 empty, even though bin/test exits zero.
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "bin").mkdir()
            tests = root / "sympy/core/tests"
            tests.mkdir(parents=True)
            (tests / "test_basic.py").write_text(
                "from pathlib import Path\n"
                "def test_public_basic():\n"
                "    Path('executed').write_text('public basic test')\n"
                "    assert 2 + 2 == 4\n"
            )
            (tests / "test_unrelated.py").write_text(
                "raise RuntimeError('readiness must not run the entire suite')\n"
            )
            runner = root / "bin/test"
            runner.write_text(
                f"#!{sys.executable}\n"
                "import argparse, pathlib, runpy\n"
                "p = argparse.ArgumentParser()\n"
                "p.add_argument('-C', action='store_true')\n"
                "p.add_argument('--verbose', action='store_true')\n"
                "p.add_argument('--timeout', type=int)\n"
                "p.add_argument('--split')\n"
                "p.add_argument('paths', nargs='*')\n"
                "a = p.parse_args()\n"
                "assert a.verbose and a.timeout == 15\n"
                "files = sorted(pathlib.Path('sympy').rglob('test_*.py'))\n"
                "if a.paths:\n"
                "    files = [f for f in files if any(s in str(f) for s in a.paths)]\n"
                "if a.split:\n"
                "    i, n = map(int, a.split.split('/'))\n"
                "    files = files[(i-1)*len(files)//n:i*len(files)//n]\n"
                "for f in files:\n"
                "    for name, test in runpy.run_path(str(f)).items():\n"
                "        if name.startswith('test_'):\n"
                "            test()\n"
                "            print(name + ' ok', flush=True)\n"
            )
            runner.chmod(0o755)
            command = self.worker.streamable_public_test_command(
                "PYTHONWARNINGS='ignore::UserWarning,ignore::SyntaxWarning' bin/test -C --verbose"
            )
            result = subprocess.run(
                ["bash", "-c", command], cwd=root, text=True,
                capture_output=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((root / "executed").exists(), result.stdout)
            self.assertEqual(result.stdout.strip(), "test_public_basic ok")

    def test_streamable_public_test_command_bounds_each_sympy_test(self):
        bounded = self.worker.streamable_public_test_command(
            "PYTHONWARNINGS='ignore::UserWarning,ignore::SyntaxWarning' bin/test -C --verbose"
        )
        self.assertEqual(
            shlex.split(bounded),
            [
                "PYTHONWARNINGS=ignore::UserWarning,ignore::SyntaxWarning",
                "bin/test", "-C", "--verbose", "--timeout", "15",
                "sympy/core/tests/test_basic.py",
            ],
        )
        existing = self.worker.streamable_public_test_command(
            "bin/test -C --verbose --timeout 17"
        )
        self.assertEqual(
            shlex.split(existing),
            ["bin/test", "-C", "--verbose", "--timeout", "17", "sympy/core/tests/test_basic.py"],
        )

    def test_run_task_readiness_persists_diagnostics_and_accepts_test_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo = root / "repo"; output = root / "output"
            repo.mkdir(); output.mkdir()
            process = mock.Mock(returncode=1, stdout="FAILED tests/test_public.py::test_bug\n", stderr="")
            with mock.patch.object(self.worker.subprocess, "run", return_value=process):
                result = self.worker.run_task_readiness(
                    instance_id="owner__repo-1",
                    image="prepared:task",
                    repo=repo,
                    script="pytest -rA tests/test_public.py",
                    test_command="pytest -rA tests/test_public.py",
                    parser=lambda output, _spec: {"tests/test_public.py::test_bug": "FAILED"},
                    test_spec=object(),
                    output=output,
                    timeout_seconds=60,
                )
            self.assertEqual(result["status"], "ready")
            self.assertEqual(result["baseline_exit_code"], 1)
            self.assertEqual((output / "test-output.txt").read_text(), process.stdout)
            metadata = json.loads((output / "metadata.json").read_text())
            self.assertEqual(metadata["test_command"], "pytest -rA tests/test_public.py")

    def test_dependency_manifest_records_packages_and_build_overlay_identity(self):
        responses = iter((
            mock.Mock(stdout='[{"name":"pytest","version":"8.0"}]\n'),
            mock.Mock(stdout=("a" * 64) + "  /opt/swebench-prepared/testbed-overlay.tar\n"),
        ))
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            result = self.worker.capture_dependency_manifest(
                image="prepared:task", output=output,
                execute=lambda *_args, **_kwargs: next(responses),
            )
            payload = json.loads((output / "dependencies.json").read_text())
        self.assertEqual(result["package_count"], 1)
        self.assertEqual(result["build_overlay_sha256"], "a" * 64)
        self.assertEqual(payload["packages"][0]["name"], "pytest")

    def test_swebench_base_image_dependency_sources_are_https_only(self):
        templates = {"py": (
            "FROM --platform={platform} ubuntu:{ubuntu_version}\n"
            "ENV TZ=Etc/UTC\nRUN apt update && apt install -y git "
            "&& rm -rf /var/lib/apt/lists/*\n"
        )}
        ca_image = "node@sha256:" + "a" * 64
        first = self.worker.enforce_https_swebench_base_images(templates, ca_image)
        second = self.worker.enforce_https_swebench_base_images(templates, ca_image)
        self.assertEqual(first, second)
        self.assertIn("sed -i 's|http://|https://|g'", templates["py"])
        self.assertIn(ca_image, templates["py"])
        self.assertIn("COPY --from=trusted_certs /etc/ssl/certs", templates["py"])
        self.assertNotIn("\nRUN apt update", templates["py"])

    def test_swebench_base_image_apt_retry_is_bounded_and_exercised(self):
        templates = {"py": (
            "FROM --platform={platform} ubuntu:{ubuntu_version}\n"
            "ENV TZ=Etc/UTC\n"
            "RUN apt update && apt install -y git && rm -rf /var/lib/apt/lists/*\n"
        )}
        self.worker.enforce_https_swebench_base_images(
            templates, "node@sha256:" + "a" * 64,
        )
        run = next(line[4:] for line in templates["py"].splitlines()
                   if line.startswith("RUN sed -i"))
        for always_fail, expected_status, expected_installs in ((False, 0, 2), (True, 1, 4)):
            with self.subTest(always_fail=always_fail), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                bin_dir = root / "bin"
                bin_dir.mkdir()
                apt = bin_dir / "apt"
                apt.write_text("#!/bin/sh\n"
                               "printf '%s\\n' \"$1\" >> \"$APT_LOG\"\n"
                               "if [ \"$1\" = install ] && "
                               "{ [ \"$ALWAYS_FAIL\" = 1 ] || [ ! -e \"$RETRIED\" ]; }; then\n"
                               "  /usr/bin/touch \"$RETRIED\"\n  exit 100\nfi\n")
                apt.chmod(0o755)
                for name in ("sed", "rm", "sleep"):
                    stub = bin_dir / name
                    stub.write_text("#!/bin/sh\nexit 0\n")
                    stub.chmod(0o755)
                log = root / "apt.log"
                env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
                           APT_LOG=str(log), RETRIED=str(root / "retried"),
                           ALWAYS_FAIL="1" if always_fail else "0")
                result = subprocess.run(["/bin/sh", "-eu", "-c", run], env=env,
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, expected_status, result.stderr)
                self.assertEqual(log.read_text().splitlines().count("install"), expected_installs)

    @contextlib.contextmanager
    def preparation_fixture(self, count=3):
        """Run real publication/readiness helpers against a stateful Docker seam."""
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            records = [dict(instance_id=f"owner__repo-{index}", repo="owner/repo",
                            version="1.0", base_commit=f"{index:040x}")
                       for index in range(1, count + 1)]
            specs = [RecipeSpec(
                instance_id=row["instance_id"], repo=row["repo"], version=row["version"],
                eval_script_list=["cd /testbed", "git config --global --add safe.directory /testbed",
                                  "git apply -v hidden", ": '>>>>> Start Test Output'", "pytest -rA"],
            ) for row in records]
            state = types.SimpleNamespace(
                root=root, records=records, specs=specs, images={}, remote={}, calls=[],
                builds=[], built_specs=[], build_inputs=[], omitted=set(), build_failed=set(), readiness_failed=set(),
                push_failed=set(), prepared_failed=set(), checkpoints=[], build_error=False,
            )

            class ImageNotFound(Exception):
                pass

            def get_image(key):
                if key not in state.images:
                    raise ImageNotFound(key)
                payload = state.images[key]
                state.images[payload["Id"]] = payload
                return types.SimpleNamespace(id=payload["Id"])

            def build_instances(client, dataset, **kwargs):
                self.assertEqual(kwargs["max_workers"], 5)
                # Like upstream, records regenerate recipes; TestSpecs pass through.
                effective = [next(spec for spec in specs if spec.instance_id == item["instance_id"])
                             if isinstance(item, dict) else item for item in dataset]
                state.build_inputs.extend(dataset)
                state.built_specs.extend(effective)
                state.builds.append([spec.instance_id for spec in effective])
                for spec in effective:
                    task = spec.instance_id
                    if task in state.omitted:
                        continue
                    state.images[spec.env_image_key] = {"Id": "sha256:" + "e" * 64}
                    if task not in state.build_failed:
                        state.images[spec.instance_image_key] = {
                            "Id": "sha256:" + hashlib.sha256(task.encode()).hexdigest(),
                            "Config": {"Labels": {}},
                        }
                if state.build_error:
                    raise RuntimeError("upstream failed after building independent images")
                # Model upstream's omission of tasks blocked by failed environments.
                return [], [spec for spec in specs if spec.instance_id in state.build_failed]

            def execute(command, **kwargs):
                state.calls.append(command)
                if command[:2] == ["docker", "pull"]:
                    state.images[command[-1]] = state.remote[command[-1]]
                elif command[:3] == ["docker", "image", "tag"]:
                    state.images[command[-1]] = state.images[command[-2]]
                elif command[:2] == ["docker", "build"]:
                    tag = command[command.index("--tag") + 1]
                    task = next(row["instance_id"] for row in records
                                if tag == f"swebench-run-prepared-{row['instance_id']}")
                    if task in state.prepared_failed:
                        raise subprocess.CalledProcessError(1, command)
                    args = dict(command[index + 1].split("=", 1)
                                for index, arg in enumerate(command) if arg == "--build-arg")
                    payload = {"Id": "sha256:" + hashlib.sha256(tag.encode()).hexdigest(),
                               "Config": {"Labels": {
                                   "org.carry.swebench.task-cache-key": args["TASK_CACHE_KEY"],
                                   "org.carry.swebench.evaluator-image-id": args["TASK_IMAGE_ID"],
                               }}}
                    state.images[tag] = payload
                elif command[:2] == ["docker", "push"]:
                    reference = command[-1]
                    task = next(task for task, refs in state.refs.items() if reference in refs.values())
                    if (task, "agent" if "swebench-ready-" in reference else "evaluator") in state.push_failed:
                        raise subprocess.CalledProcessError(1, command)
                    payload = dict(state.images[reference])
                    payload["RepoDigests"] = ["registry.example/tasks@sha256:" +
                                              hashlib.sha256(reference.encode()).hexdigest()]
                    state.remote[reference] = payload
                    state.remote[payload["RepoDigests"][0]] = payload
                    state.checkpoints.append(json.loads(
                        (state.kwargs["output"] / "preparation-attempt.json").read_text()
                    ) if (state.kwargs["output"] / "preparation-attempt.json").exists() else None)
                elif command[:3] == ["docker", "image", "inspect"]:
                    payload = state.images[command[-1]]
                    return types.SimpleNamespace(returncode=0, stdout=(
                        payload["Id"] if command[4] == "{{.Id}}" else json.dumps(payload)), stderr="")
                elif command[:2] == ["docker", "run"]:
                    if "conda list --json" in command[-1]:
                        return types.SimpleNamespace(returncode=0, stdout='[{"name":"pytest","version":"8"}]', stderr="")
                    if "sha256sum" in command:
                        return types.SimpleNamespace(returncode=0, stdout="a" * 64 +
                                                     "  /opt/swebench-prepared/testbed-overlay.tar\n", stderr="")
                    image = next(arg for arg in command if arg.startswith("swebench-run-prepared-"))
                    task = image.removeprefix("swebench-run-prepared-")
                    return types.SimpleNamespace(returncode=1,
                                                 stdout="" if task in state.readiness_failed else "FAILED test_public\n",
                                                 stderr="")
                else:
                    raise AssertionError(f"unexpected Docker operation: {command}")
                # Make source image IDs addressable, as in real Docker.
                state.images.update({payload["Id"]: payload for payload in list(state.images.values())})
                return types.SimpleNamespace(returncode=0, stdout="", stderr="")

            def clone(repo, commit, destination):
                destination.mkdir(parents=True)

            state.refs = {row["instance_id"]: self.worker.task_image_references(
                "registry.example/tasks", self.worker.task_image_cache_key(
                    row, prepared_dockerfile_sha256=self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1]),
                    base_dockerfile_sha256="e" * 64,
                )) for row in records}
            state.kwargs = dict(
                records=records, source=SCRIPT.parents[1], run_id="run", repository="registry.example/tasks",
                work=root / "work", output=root / "output", clone=clone,
                client=types.SimpleNamespace(images=types.SimpleNamespace(get=get_image)),
                build_instances=build_instances, get_specs=lambda rows: specs, swebench_version="4.1.0",
                parsers={"owner/repo": lambda text, spec: {"test_public": "FAILED"} if text else {}},
                repo_specs={"owner/repo": {"1.0": {"test_cmd": "pytest -rA"}}},
                base_dockerfile_sha256="e" * 64,
                remote_exists=lambda reference: reference in state.remote, execute=execute,
            )
            # Readiness invokes subprocess dynamically; image/manifest helpers use execute.
            with mock.patch.object(self.worker.subprocess, "run", side_effect=execute):
                yield state

    @contextlib.contextmanager
    def repaired_preparation_fixture(self):
        with self.preparation_fixture(count=1) as state:
            record, original = state.records[0], state.specs[0]
            record.update(repo="scikit-learn/scikit-learn", version="1.3")
            original.repo, original.version = record["repo"], record["version"]
            original.env_script_list = compat.SKLEARN_ENV.copy()
            original.repo_script_list = [compat.SKLEARN_INSTALL]
            state.kwargs["parsers"][record["repo"]] = lambda text, spec: {"public": "FAILED"} if text else {}
            state.kwargs["repo_specs"][record["repo"]] = {"1.3": {"test_cmd": "pytest -rA"}}
            task = record["instance_id"]
            state.refs[task] = self.worker.task_image_references(
                state.kwargs["repository"], self.worker.task_image_cache_key(
                    record, prepared_dockerfile_sha256=self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1]),
                    base_dockerfile_sha256="e" * 64,
                ),
            )
            yield state

    def test_publisher_builds_repaired_specs_and_persists_recipe_provenance(self):
        with self.repaired_preparation_fixture() as state:
            original = state.specs[0]
            task = original.instance_id
            with mock.patch.object(self.worker, "trusted_readiness_script", wraps=self.worker.trusted_readiness_script) as readiness, \
                    mock.patch.object(self.worker, "run_task_readiness", wraps=self.worker.run_task_readiness) as probe:
                published = self.worker.publish_task_environments(**state.kwargs)
            built = state.built_specs[0]
            self.assertIn(compat.PIP_PIN, built.repo_script_list)
            self.assertIs(state.build_inputs[0], built, "upstream must receive specs, not regenerate records")
            self.assertIs(readiness.call_args.args[0], built)
            self.assertIs(probe.call_args.kwargs["test_spec"], built)
            self.assertNotIn(compat.PIP_PIN, original.repo_script_list)
            self.assertEqual(built.eval_script_list, original.eval_script_list)
            self.assertNotEqual(built.instance_image_key, original.instance_image_key)
            self.assertEqual(published[task]["source_task_image"], built.instance_image_key)
            provenance = published[task]["preparation_compatibility"]
            self.assertEqual(provenance["repairs"], ["sklearn-legacy-pip-25.2"])
            self.assertEqual(provenance["compatibility_sha256"], compat.preparation_compatibility_sha256())
            self.assertEqual(provenance["swebench_version"], "4.1.0")
            self.assertNotEqual(provenance["original_recipe_sha256"], provenance["effective_recipe_sha256"])
            self.assertEqual(provenance["instance_image_tag"], built.instance_image_tag)
            attempt = json.loads((state.kwargs["output"] / "preparation-attempt.json").read_text())
            prepared = json.loads((state.kwargs["output"] / "preparation.json").read_text())
            self.assertEqual(attempt["tasks"][task]["preparation_compatibility"], provenance)
            self.assertEqual(prepared[task]["preparation_compatibility"], provenance)
            cached = self.worker.publish_task_environments(**state.kwargs)
            self.assertEqual(cached[task]["status"], "cached")
            self.assertEqual(cached[task]["preparation_compatibility"], provenance)
            self.assertEqual(len(state.builds), 1)
            catalog = self.worker.task_catalog_payload(
                published=cached, repository=state.kwargs["repository"],
                prepared_recipe_sha256=self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1]),
                base_recipe_sha256="e" * 64,
            )
            self.assertIn("preparation_compatibility", catalog["tasks"][task])
            self.assertEqual(catalog["tasks"][task]["preparation_compatibility"], provenance)
            normalized = self.worker.validate_task_catalog(
                catalog=catalog, records=state.records, repository=state.kwargs["repository"],
                prepared_recipe_sha256=self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1]),
                base_recipe_sha256="e" * 64,
            )
            self.assertEqual(normalized["tasks"][task]["preparation_compatibility"], provenance)

    def test_pulled_repaired_evaluator_is_retagged_for_the_official_subprocess(self):
        with self.repaired_preparation_fixture() as state:
            published = self.worker.publish_task_environments(**state.kwargs)
            task = state.records[0]["instance_id"]
            catalog = self.worker.task_catalog_payload(
                published=published, repository=state.kwargs["repository"],
                prepared_recipe_sha256=self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1]),
                base_recipe_sha256="e" * 64,
            )
            # A different worker has only registry digests, not the publisher's tags.
            state.images.clear()
            state.calls.clear()

            def get_specs(records, namespace=None, instance_image_tag="latest", **kwargs):
                specs = copy.deepcopy(state.specs)
                for spec in specs:
                    spec.namespace = namespace
                    spec.instance_image_tag = instance_image_tag
                return specs

            resolved = self.worker.resolve_task_environments(
                records=state.records, source=SCRIPT.parents[1], repository=state.kwargs["repository"],
                output=state.root / "resolved", base_dockerfile_sha256="e" * 64,
                catalog=catalog, get_specs=get_specs, execute=state.kwargs["execute"],
            )
            official_spec = get_specs(state.records, namespace="swebench")[0]
            self.assertIn(official_spec.instance_image_key, state.images)
            evaluator = published[task]["evaluator_image"]
            self.assertEqual(state.images[official_spec.instance_image_key]["Id"], evaluator["image_id"])
            self.assertNotEqual(official_spec.instance_image_key, published[task]["source_task_image"])
            self.assertEqual(resolved[task]["preparation_compatibility"],
                             published[task]["preparation_compatibility"])
            self.assertFalse(any(command[:2] == ["docker", "build"] for command in state.calls))

            def evaluator_process(command, **kwargs):
                self.assertEqual(
                    command[1], str(SCRIPT.with_name("swebench_evaluator_compat.py").resolve())
                )
                # run_evaluation regenerates unmodified recipes, with this namespace
                # and tag. Its remote-image branch reuses images.get(key), no build.
                namespace = command[command.index("--namespace") + 1]
                tag = command[command.index("--instance_image_tag") + 1]
                regenerated = get_specs(state.records, namespace=namespace, instance_image_tag=tag)[0]
                self.assertNotIn(compat.PIP_PIN, regenerated.repo_script_list)
                self.assertEqual(state.images[regenerated.instance_image_key]["Id"], evaluator["image_id"])
                return types.SimpleNamespace(returncode=0)

            with mock.patch.object(self.worker.subprocess, "run", side_effect=evaluator_process) as process, \
                    mock.patch.object(self.worker, "cleanup_evaluator_containers"):
                self.worker.run_official_evaluation(
                    predictions=state.root / "predictions.json", canonical_dataset=state.root / "dataset.json",
                    instance_ids=[task], run_id="evaluator", output=state.root, environment={},
                )
            process.assert_called_once()

    def test_sympy_parser_keeps_duplicate_failure_after_later_pass(self):
        from scripts import swebench_evaluator_compat as evaluator_compat

        parsed = evaluator_compat.parse_log_sympy_fail_closed(
            "test_MatrixElement_printing F\n"
            "test_MatrixElement_printing ok\n",
            None,
        )

        self.assertEqual(parsed["test_MatrixElement_printing"], "FAILED")

    def test_sympy_parser_keeps_duplicate_error_after_later_pass(self):
        from scripts import swebench_evaluator_compat as evaluator_compat

        parsed = evaluator_compat.parse_log_sympy_fail_closed(
            "test_MatrixElement_printing E\n"
            "test_MatrixElement_printing ok\n",
            None,
        )

        self.assertEqual(parsed["test_MatrixElement_printing"], "ERROR")

    def test_sympy_compat_installs_fail_closed_parser(self):
        from scripts import swebench_evaluator_compat as evaluator_compat

        parsers = {"sympy/sympy": object()}
        evaluator_compat.install_sympy_parser(parsers)

        self.assertIs(parsers["sympy/sympy"], evaluator_compat.parse_log_sympy_fail_closed)

    def test_sympy_compat_wrapper_installs_parser_before_official_module(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            package = root / "swebench" / "harness" / "log_parsers"
            package.mkdir(parents=True)
            metadata = root / "swebench-4.1.0.dist-info"
            metadata.mkdir()
            metadata.joinpath("METADATA").write_text(
                "Metadata-Version: 2.1\nName: swebench\nVersion: 4.1.0\n",
                encoding="utf-8",
            )
            (root / "swebench" / "__init__.py").write_text("", encoding="utf-8")
            (root / "swebench" / "harness" / "__init__.py").write_text(
                "from . import run_evaluation\n", encoding="utf-8"
            )
            package.joinpath("__init__.py").write_text(
                "def original(log, spec):\n"
                "    return {'duplicate': 'PASSED'}\n"
                "MAP_REPO_TO_PARSER = {'sympy/sympy': original}\n",
                encoding="utf-8",
            )
            marker = root / "official-ran"
            (root / "swebench" / "harness" / "run_evaluation.py").write_text(
                "import os\n"
                "from pathlib import Path\n"
                "from swebench.harness.log_parsers import MAP_REPO_TO_PARSER\n"
                "def main():\n"
                "    parsed = MAP_REPO_TO_PARSER['sympy/sympy']('test_duplicate F\\nduplicate ok\\n', None)\n"
                "    if parsed != {'test_duplicate': 'FAILED'}:\n"
                "        raise SystemExit(9)\n"
                "    Path(os.environ['WRAPPER_MARKER']).write_text('ok', encoding='utf-8')\n"
                "if __name__ == '__main__':\n"
                "    main()\n",
                encoding="utf-8",
            )
            environment = dict(os.environ, PYTHONPATH=str(root), WRAPPER_MARKER=str(marker))

            completed = subprocess.run(
                [sys.executable, str(SCRIPT.with_name("swebench_evaluator_compat.py"))],
                cwd=root, env=environment, check=False, capture_output=True, text=True,
            )

            self.assertEqual(completed.returncode, 0)
            self.assertEqual(completed.stderr, "")
            self.assertEqual(marker.read_text(encoding="utf-8"), "ok")

    def test_publisher_preserves_independent_pairs_and_reuses_them_after_partial_build_failure(self):
        with self.preparation_fixture() as state:
            state.omitted.add("owner__repo-1")
            state.build_failed.add("owner__repo-2")
            with self.assertRaises(RuntimeError):
                self.worker.publish_task_environments(**state.kwargs)
            self.assertIn(state.refs["owner__repo-3"]["agent"], state.remote)
            progress = json.loads((state.root / "output" / "preparation.json").read_text())
            self.assertEqual(set(progress), {"owner__repo-3"})
            attempt = json.loads((state.root / "output" / "preparation-attempt.json").read_text())
            self.assertEqual(attempt["denominator"], 3)
            self.assertEqual(set(attempt["tasks"]), {row["instance_id"] for row in state.records})
            self.assertEqual(attempt["tasks"]["owner__repo-1"]["status"], "blocked_by_environment")
            self.assertEqual(attempt["tasks"]["owner__repo-2"]["status"], "failed")
            self.assertEqual(attempt["tasks"]["owner__repo-3"]["status"], "published")
            self.assertTrue(all(state.checkpoints))
            state.omitted.clear()
            state.build_failed.clear()
            published = self.worker.publish_task_environments(**state.kwargs)
            self.assertEqual(len(published), 3)
            self.assertEqual(published["owner__repo-3"]["status"], "cached")
            self.assertEqual(state.builds[-1], ["owner__repo-1", "owner__repo-2"])

    def test_publisher_records_monotonic_stage_timings_cache_counts_and_readiness_failure(self):
        with self.preparation_fixture() as state:
            state.readiness_failed.add("owner__repo-1")
            with mock.patch.object(self.worker.time, "monotonic", side_effect=itertools.count(1000)), \
                    mock.patch.object(self.worker.time, "time", side_effect=AssertionError("wall clock")):
                with self.assertRaises(RuntimeError):
                    self.worker.publish_task_environments(**state.kwargs)
            attempt = json.loads((state.root / "output" / "preparation-attempt.json").read_text())
            self.assertGreater(attempt.get("elapsed_seconds", 0), 0)
            self.assertEqual(attempt["cache_counts"], {"hit": 0, "miss": 3, "error": 0})
            build = attempt["stages"]["dependency_build"]
            self.assertGreater(build["elapsed_seconds"], 0)
            self.assertEqual(build["status"], "completed")
            self.assertEqual(build["expected_task_count"], 3)
            self.assertEqual(build["verified_image_count"], 3)
            stages = {"cache_lookup", "instance_image", "prepared_image", "dependency_manifest",
                      "clone", "readiness", "evaluator_push", "agent_push", "pair_verification"}
            for task in ("owner__repo-2", "owner__repo-3"):
                self.assertEqual(set(attempt["tasks"][task]["stages"]), stages)
                for stage in attempt["tasks"][task]["stages"].values():
                    self.assertEqual(stage["status"], "completed")
                    self.assertGreater(stage["elapsed_seconds"], 0)
            failure = attempt["tasks"]["owner__repo-1"]
            self.assertEqual(failure["failure_stage"], "readiness")
            self.assertEqual(failure["stages"]["readiness"]["status"], "failed")
            self.assertGreater(failure["stages"]["readiness"]["elapsed_seconds"], 0)
            self.assertNotIn("agent_push", failure["stages"])
            self.assertNotIn(state.refs["owner__repo-1"]["agent"], state.remote)
            self.assertEqual(attempt["status_counts"], {"failed": 1, "published": 2})
            self.assertEqual(attempt["phase"], "failed")
            self.assertFalse((state.root / "work" / "readiness").exists())
            # Earlier successful tasks are durable while later pair pushes run.
            self.assertTrue(any(checkpoint["status_counts"].get("published", 0) == 1
                                for checkpoint in state.checkpoints))
            state.readiness_failed.clear()
            self.worker.publish_task_environments(**state.kwargs)
            self.worker.publish_task_environments(**state.kwargs)
            cached = json.loads((state.root / "output" / "preparation-attempt.json").read_text())
            self.assertEqual(cached["cache_counts"], {"hit": 3, "miss": 0, "error": 0})
            self.assertEqual(cached["stages"]["dependency_build"]["status"], "skipped")
            self.assertEqual(cached["phase"], "complete")

    def test_full_preparation_cli_withholds_catalog_until_all_fifty_pairs_are_ready(self):
        with self.preparation_fixture(count=50) as state:
            source = state.root / "source"
            (source / "benchmarks").mkdir(parents=True)
            (source / "containers").symlink_to(SCRIPT.parents[1] / "containers", target_is_directory=True)
            (source / "scripts").symlink_to(SCRIPT.parent, target_is_directory=True)
            manifest, _ = self.worker.selection_manifest_names("prepare-50")
            (source / "benchmarks" / manifest).write_text(json.dumps({
                "instance_ids": [row["instance_id"] for row in state.records],
            }))
            output = state.root / "cli-output"
            state.kwargs["output"] = output / "preparation"
            state.omitted.add("owner__repo-1")
            argv = [str(SCRIPT), "--prepare-images", "--source", str(source),
                    "--work", str(state.root / "work"), "--output", str(output)]
            environment = {
                "BASE_IMAGE": "node@sha256:" + "a" * 64, "CODEX_VERSION": "1.2.3",
                "PI_VERSION": "0.84.2", "MODEL": "gpt-5.6-luna", "REASONING": "medium",
                "TASK_IMAGE_REPOSITORY": "registry.example/tasks", "RUN_ID": "run",
                "BENCHMARK_MODE": "prepare-50",
            }
            real_publish = self.worker.publish_task_environments

            def publish_with_fake_docker(**kwargs):
                self.assertEqual(kwargs["records"], state.records)
                for key in ("client", "build_instances", "get_specs", "parsers", "repo_specs",
                            "execute", "remote_exists", "clone", "swebench_version"):
                    kwargs[key] = state.kwargs[key]
                return real_publish(**kwargs)

            dataset_module = types.ModuleType("datasets")
            dataset_module.load_dataset = mock.Mock(return_value=state.records)
            dockerfiles_module = types.ModuleType("swebench.harness.dockerfiles")
            dockerfiles_module._DOCKERFILE_BASE = {"py": "fixture"}
            with mock.patch.dict(os.environ, environment, clear=True), \
                    mock.patch.object(sys, "argv", argv), \
                    mock.patch.dict(sys.modules, {"datasets": dataset_module,
                                                 "swebench.harness.dockerfiles": dockerfiles_module}), \
                    mock.patch.object(self.worker, "enforce_https_swebench_base_images", return_value="e" * 64), \
                    mock.patch.object(self.worker, "publish_task_environments", side_effect=publish_with_fake_docker), \
                    mock.patch.object(self.worker, "publish_task_catalog_image", return_value=
                                      "registry.example/tasks@sha256:" + "f" * 64) as catalog, \
                    mock.patch.object(self.worker, "execute_benchmark") as benchmark:
                with self.assertRaisesRegex(RuntimeError, "49/50"):
                    self.worker.main()
                catalog.assert_not_called()
                benchmark.assert_not_called()
                self.assertFalse((output / "catalog.json").exists())
                self.assertFalse((output / "preparation-report.json").exists())
                attempt = json.loads((output / "preparation" / "preparation-attempt.json").read_text())
                progress = json.loads((output / "preparation" / "preparation.json").read_text())
                self.assertEqual(attempt["denominator"], 50)
                self.assertEqual(len(attempt["tasks"]), 50)
                self.assertEqual(len(progress), 49)
                state.omitted.clear()
                self.assertEqual(self.worker.main(), 0)
                catalog.assert_called_once()
                self.assertEqual(len(catalog.call_args.kwargs["catalog"]["tasks"]), 50)
                self.assertEqual(state.builds[-1], ["owner__repo-1"])
                report = json.loads((output / "preparation-report.json").read_text())
                self.assertEqual(report["denominator"], 50)
                self.assertEqual(report["status_counts"], {"cached": 49, "published": 1})
                for task, item in report["tasks"].items():
                    self.assertIn("preparation_compatibility", item)
                    self.assertEqual(item["preparation_compatibility"],
                                     catalog.call_args.kwargs["catalog"]["tasks"][task]["preparation_compatibility"])
                benchmark.assert_not_called()

    def test_publisher_retains_other_pairs_after_prepared_or_push_failures(self):
        for failure in ("prepared_image", "evaluator_push", "agent_push"):
            with self.subTest(stage=failure), self.preparation_fixture() as state:
                if failure == "prepared_image":
                    state.prepared_failed.add("owner__repo-1")
                else:
                    state.push_failed.add(("owner__repo-1", failure.removesuffix("_push")))
                with self.assertRaises(RuntimeError):
                    self.worker.publish_task_environments(**state.kwargs)
                progress = json.loads((state.root / "output" / "preparation.json").read_text())
                self.assertEqual(set(progress), {"owner__repo-2", "owner__repo-3"})
                attempt = json.loads((state.root / "output" / "preparation-attempt.json").read_text())
                self.assertEqual(attempt["tasks"]["owner__repo-1"]["failure_stage"], failure)
                self.assertEqual(attempt["tasks"]["owner__repo-1"]["stages"][failure]["status"], "failed")
                self.assertNotIn(state.refs["owner__repo-1"]["agent"], state.remote)
                self.assertFalse((state.root / "work" / "readiness").exists())
                state.prepared_failed.clear()
                state.push_failed.clear()
                published = self.worker.publish_task_environments(**state.kwargs)
                self.assertEqual(len(published), 3)
                self.assertEqual(state.builds[-1], ["owner__repo-1"])

    def test_publisher_diagnoses_environment_omissions_even_with_no_reported_build_failures(self):
        with self.preparation_fixture() as state:
            state.omitted.add("owner__repo-1")
            with self.assertRaises(RuntimeError):
                self.worker.publish_task_environments(**state.kwargs)
            attempt = json.loads((state.root / "output" / "preparation-attempt.json").read_text())
            build = attempt["stages"]["dependency_build"]
            self.assertEqual(build["status"], "incomplete")
            self.assertEqual(attempt["build_reported_failure_count"], 0)
            self.assertEqual(build["verified_image_count"], 2)
            self.assertEqual(build["expected_task_count"], 3)
            task = attempt["tasks"]["owner__repo-1"]
            self.assertEqual(task["status"], "blocked_by_environment")
            self.assertEqual(task["environment_image"], state.specs[0].env_image_key)
            self.assertEqual(task["source_task_image"], state.specs[0].instance_image_key)

    def test_publisher_retains_pairs_after_batch_exception_and_rejects_invalid_cached_pair(self):
        with self.preparation_fixture() as state:
            state.omitted.add("owner__repo-1")
            state.build_error = True
            with self.assertRaises(RuntimeError):
                self.worker.publish_task_environments(**state.kwargs)
            attempt = json.loads((state.root / "output" / "preparation-attempt.json").read_text())
            self.assertEqual(attempt["stages"]["dependency_build"]["status"], "failed")
            self.assertEqual(attempt["build_error_type"], "RuntimeError")
            self.assertEqual(attempt["status_counts"], {"blocked_by_environment": 1, "published": 2})
            state.build_error = False
            state.omitted.clear()
            agent = state.remote[state.refs["owner__repo-2"]["agent"]]
            agent["Config"]["Labels"]["org.carry.swebench.evaluator-image-id"] = "sha256:" + "0" * 64
            with self.assertRaises(RuntimeError):
                self.worker.publish_task_environments(**state.kwargs)
            progress = json.loads((state.root / "output" / "preparation.json").read_text())
            self.assertEqual(set(progress), {"owner__repo-1", "owner__repo-3"})
            attempt = json.loads((state.root / "output" / "preparation-attempt.json").read_text())
            self.assertEqual(attempt["cache_counts"], {"hit": 1, "miss": 1, "error": 1})
            self.assertEqual(attempt["tasks"]["owner__repo-2"]["failure_stage"], "cache_lookup")
            self.assertEqual(state.builds[-1], ["owner__repo-1"])

    def test_publisher_checkpoints_all_ready_pairs_but_does_not_hide_a_batch_exception(self):
        with self.preparation_fixture() as state:
            state.build_error = True
            with self.assertRaisesRegex(RuntimeError, "3/3"):
                self.worker.publish_task_environments(**state.kwargs)
            progress = json.loads((state.root / "output" / "preparation.json").read_text())
            attempt = json.loads((state.root / "output" / "preparation-attempt.json").read_text())
            self.assertEqual(len(progress), 3)
            self.assertEqual(attempt["phase"], "failed")
            self.assertEqual(attempt["build_error_type"], "RuntimeError")
            self.assertEqual(attempt["status_counts"], {"published": 3})
            published = self.worker.publish_task_environments(**state.kwargs)
            self.assertTrue(all(task["status"] == "cached" for task in published.values()))
            self.assertEqual(len(state.builds), 1)

    def test_catalog_publisher_builds_only_ready_cache_misses_and_pushes_ready_last(self):
        records = [
            {"instance_id": "owner__repo-1", "repo": "owner/repo", "version": "1.0", "base_commit": "a" * 40},
            {"instance_id": "owner__repo-2", "repo": "owner/repo", "version": "1.0", "base_commit": "b" * 40},
        ]
        specs = [types.SimpleNamespace(
            instance_id=record["instance_id"], instance_image_key=f"source:{record['instance_id']}",
            repo=record["repo"], version=record["version"], instance_image_tag="latest",
            env_script_list=["conda activate testbed"], repo_script_list=["pip install -e ."],
            eval_script_list=[
                "source /opt/miniconda3/bin/activate", "conda activate testbed", "cd /testbed",
                "git config --global --add safe.directory /testbed",
                "git apply -v hidden", ": '>>>>> Start Test Output'",
                "pytest -rA tests/test_public.py", ": '>>>>> End Test Output'",
            ],
        ) for record in records]
        source_ids = {
            "source:owner__repo-1": "sha256:" + "a" * 64,
            "source:owner__repo-2": "sha256:" + "e" * 64,
        }
        client = types.SimpleNamespace(images=types.SimpleNamespace(
            get=lambda key: types.SimpleNamespace(id=source_ids[key])
        ))
        events = []

        def build_instances(_client, dataset, **kwargs):
            events.append(("build", [spec.instance_id for spec in dataset]))
            return [spec.instance_image_key for spec in dataset], []

        def clone(_repo, _commit, destination):
            destination.mkdir(parents=True)

        def fake_prepared(**kwargs):
            events.append(("prepared", kwargs["instance_id"]))
            return {
                "tag": "local:prepared", "image_id": "sha256:" + "f" * 64,
                "task_image_id": kwargs["task_image_id"], "cache_key": kwargs["cache_key"],
                "dockerfile_sha256": "d" * 64,
            }

        def fake_readiness(**kwargs):
            events.append(("readiness", kwargs["instance_id"]))
            return {"status": "ready", "parsed_test_count": 1,
                    "test_command_sha256": "e" * 64}

        dockerfile_hash = self.worker.prepared_image_recipe_sha256(SCRIPT.parents[1])
        keys = {
            record["instance_id"]: self.worker.task_image_cache_key(
                record, prepared_dockerfile_sha256=dockerfile_hash,
                base_dockerfile_sha256="e" * 64,
            ) for record in records
        }
        refs = {
            instance_id: self.worker.task_image_references("registry.example/tasks", key)
            for instance_id, key in keys.items()
        }
        calls = []

        def remote_exists(reference):
            return reference in set(refs["owner__repo-1"].values())

        def execute(command, **kwargs):
            calls.append(command)
            if command[:3] != ["docker", "image", "inspect"]:
                return mock.Mock(returncode=0, stdout="")
            reference = command[-1]
            if command[4] == "{{.Id}}":
                return mock.Mock(stdout=source_ids["source:owner__repo-2"] + "\n")
            is_first = keys["owner__repo-1"] in reference
            evaluator_id = source_ids["source:owner__repo-1"] if is_first else source_ids["source:owner__repo-2"]
            image_id = evaluator_id if "evaluator" in reference else "sha256:" + ("b" if is_first else "f") * 64
            key = keys["owner__repo-1"] if is_first else keys["owner__repo-2"]
            payload = {
                "Id": image_id,
                "RepoDigests": ["registry.example/tasks@sha256:" + ("c" if "evaluator" in reference else "d") * 64],
                "Config": {"Labels": {} if "evaluator" in reference else {
                    "org.carry.swebench.task-cache-key": key,
                    "org.carry.swebench.evaluator-image-id": evaluator_id,
                }},
            }
            return mock.Mock(stdout=json.dumps(payload))

        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(self.worker, "build_prepared_task_image", side_effect=fake_prepared), \
                mock.patch.object(self.worker, "run_task_readiness", side_effect=fake_readiness), \
                mock.patch.object(self.worker, "capture_dependency_manifest", return_value={
                    "package_count": 2, "sha256": "1" * 64,
                }):
            root = pathlib.Path(directory)
            published = self.worker.publish_task_environments(
                records=records, source=SCRIPT.parents[1], run_id="run-1",
                repository="registry.example/tasks", work=root / "work", output=root / "output",
                clone=clone, client=client, build_instances=build_instances,
                get_specs=lambda dataset: specs, swebench_version="4.1.0",
                parsers={"owner/repo": lambda *_args: {"test": "PASSED"}},
                repo_specs={"owner/repo": {"1.0": {"test_cmd": "pytest -rA"}}},
                dockerfile_templates={"py": (
                    "FROM --platform={platform} ubuntu:{ubuntu_version}\n"
                    "ENV TZ=Etc/UTC\nRUN apt update\n"
                )},
                trusted_ca_image="node@sha256:" + "a" * 64,
                base_dockerfile_sha256="e" * 64,
                remote_exists=remote_exists, execute=execute,
            )
        self.assertEqual(events[0], ("build", ["owner__repo-2"]))
        self.assertEqual(events[1:], [("prepared", "owner__repo-2"), ("readiness", "owner__repo-2")])
        self.assertEqual(published["owner__repo-1"]["status"], "cached")
        self.assertEqual(published["owner__repo-2"]["status"], "published")
        pushes = [command for command in calls if command[:2] == ["docker", "push"]]
        self.assertEqual(pushes, [
            ["docker", "push", refs["owner__repo-2"]["evaluator"]],
            ["docker", "push", refs["owner__repo-2"]["agent"]],
        ])
        self.assertIn("resolved_digest", published["owner__repo-2"]["agent_image"])

    def test_agent_network_is_internal_and_reaches_only_the_openai_proxy(self):
        calls = []

        def execute(command, **kwargs):
            calls.append((command, kwargs))
            if command[:2] == ["docker", "inspect"] and "--format" in command:
                return mock.Mock(returncode=0, stdout="172.28.0.2\n")
            return mock.Mock(returncode=0, stdout="")

        with tempfile.TemporaryDirectory() as directory:
            proxy_script = pathlib.Path(directory) / "openai_proxy.js"
            proxy_script.write_text("// proxy fixture\n")
            network = self.worker.start_agent_network(
                identity="slot-one", proxy_image="node@sha256:" + "a" * 64,
                proxy_script=proxy_script, execute=execute,
            )

        commands = [command for command, _ in calls]
        self.assertIn(
            ["docker", "network", "create", "--internal", network["internal"]],
            commands,
        )
        self.assertIn(
            ["docker", "network", "connect", "--alias", "openai-proxy", network["internal"], network["proxy"]],
            commands,
        )
        proxy_run = next(command for command in commands if command[:3] == ["docker", "run", "--detach"])
        self.assertEqual(proxy_run[proxy_run.index("--network") + 1], network["egress"])
        self.assertIn("no-new-privileges", proxy_run)
        self.assertIn("--cap-drop=ALL", proxy_run)
        self.assertEqual(network["api_base"], "http://openai-proxy:8080/v1")
        probes = [command for command in commands if command[:2] == ["docker", "run"] and "--detach" not in command]
        self.assertEqual(len(probes), 2)
        self.assertTrue(all(command[command.index("--network") + 1] == network["internal"] for command in probes))
        self.assertTrue(all(command[command.index("--dns") + 1] == "127.0.0.1" for command in probes))
        self.assertTrue(all(f"openai-proxy:{network['proxy_ip']}" in command for command in probes))
        self.assertTrue(any("github.com" in " ".join(command) for command in probes))
        self.assertTrue(any("openai-proxy:8080/healthz" in " ".join(command) for command in probes))

    def test_agent_network_fails_closed_when_direct_internet_is_reachable(self):
        def execute(command, **kwargs):
            if command[:2] == ["docker", "inspect"] and "--format" in command:
                return mock.Mock(returncode=0, stdout="172.28.0.2\n")
            if "github.com" in " ".join(command):
                return mock.Mock(returncode=42, stdout="")
            if command[:2] == ["docker", "inspect"] or command[:3] == ["docker", "network", "inspect"]:
                return mock.Mock(returncode=1, stdout="")
            return mock.Mock(returncode=0, stdout="")

        with tempfile.TemporaryDirectory() as directory:
            proxy_script = pathlib.Path(directory) / "openai_proxy.js"
            proxy_script.write_text("// proxy fixture\n")
            with self.assertRaisesRegex(RuntimeError, "direct internet"):
                self.worker.start_agent_network(
                    identity="slot-two", proxy_image="node@sha256:" + "a" * 64,
                    proxy_script=proxy_script, execute=execute,
                )

    def test_agent_network_cleanup_fails_if_proxy_remains(self):
        network = {"internal": "internal", "egress": "egress", "proxy": "proxy"}

        def execute(command, **kwargs):
            if command[:2] == ["docker", "inspect"]:
                return mock.Mock(returncode=0, stdout="proxy still exists\n")
            return mock.Mock(returncode=0, stdout="")

        with self.assertRaisesRegex(self.worker.ContainerCleanupError, "proxy.*remains"):
            self.worker.cleanup_agent_network(network, execute=execute)

    @unittest.skipUnless(shutil.which("docker"), "Docker is required for the network namespace test")
    def test_agent_network_namespace_blocks_external_fetch_and_proxy_escape(self):
        image = "node@sha256:afff6d8c97964a438d2e6a9c96509367e45d8bf93f790ad561a1eaea926303d9"
        network = self.worker.start_agent_network(
            identity=f"integration-{os.getpid()}", proxy_image=image,
            proxy_script=SCRIPT.with_name("openai_proxy.js"),
        )
        try:
            result = subprocess.run(
                [
                    "docker", "run", "--rm", "--network", network["internal"],
                    "--dns", "127.0.0.1", "--add-host", f"openai-proxy:{network['proxy_ip']}",
                    "--entrypoint", "node", image, "-e",
                    "fetch('http://openai-proxy:8080/v1/models')"
                    ".then(r => process.exit(r.status === 403 ? 0 : 1))"
                    ".catch(() => process.exit(2))",
                ],
                check=False, timeout=30,
            )
            self.assertEqual(result.returncode, 0)
        finally:
            self.worker.cleanup_agent_network(network)

    def test_isolated_agent_always_removes_its_proxy_and_networks(self):
        network = {
            "internal": "internal", "egress": "egress", "proxy": "proxy",
            "proxy_ip": "172.28.0.2", "api_base": "http://openai-proxy:8080/v1",
        }
        with mock.patch.object(self.worker, "start_agent_network", return_value=network), \
                mock.patch.object(self.worker, "run_agent", side_effect=RuntimeError("agent crash")) as run, \
                mock.patch.object(self.worker, "cleanup_agent_network") as cleanup, \
                self.assertRaisesRegex(RuntimeError, "agent crash"):
            self.worker.run_isolated_agent(
                instance_id="task-1", harness="carry", image="carry:run",
                harness_bundle=pathlib.Path("harness"),
                proxy_image="node@sha256:" + "a" * 64,
                proxy_script=pathlib.Path("openai_proxy.js"),
                repo=pathlib.Path("repo"), task_input=pathlib.Path("input"),
                output=pathlib.Path("output"), model="gpt-5.6-luna", reasoning="medium",
            )
        self.assertEqual(run.call_args.kwargs["network"], "internal")
        self.assertEqual(run.call_args.kwargs["proxy_ip"], "172.28.0.2")
        self.assertEqual(run.call_args.kwargs["api_base"], "http://openai-proxy:8080/v1")
        cleanup.assert_called_once_with(network)

    def test_openai_proxy_rejects_non_responses_targets(self):
        proxy = SCRIPT.with_name("openai_proxy.js")
        check = """
const { isAllowedRequest } = require(process.argv[1]);
if (!isAllowedRequest('POST', '/v1/responses')) process.exit(1);
if (!isAllowedRequest('POST', '/v1/responses/compact')) process.exit(2);
if (!isAllowedRequest('GET', '/healthz')) process.exit(3);
if (isAllowedRequest('GET', '/v1/models')) process.exit(4);
if (isAllowedRequest('GET', 'https://github.com/owner/repo')) process.exit(5);
if (isAllowedRequest('POST', '/v1/responses/../../models')) process.exit(6);
"""
        subprocess.run(["node", "-e", check, str(proxy)], check=True)

    def test_pi_adapter_uses_the_isolated_openai_base_url(self):
        entrypoint = SCRIPT.parents[1] / "containers" / "swebench-harness" / "entrypoint.py"
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo, home, output = root / "repo", root / "home", root / "output"
            repo.mkdir()
            home.mkdir()
            subprocess.run(["git", "init", "--quiet", "--initial-branch=main"], cwd=repo, check=True)
            subprocess.run(["git", "config", "user.name", "Benchmark Test"], cwd=repo, check=True)
            subprocess.run(["git", "config", "user.email", "benchmark@example.invalid"], cwd=repo, check=True)
            (repo / "tracked.txt").write_text("base\n")
            subprocess.run(["git", "add", "tracked.txt"], cwd=repo, check=True)
            subprocess.run(["git", "commit", "--quiet", "-m", "base"], cwd=repo, check=True)
            prompt = root / "task.md"
            prompt.write_text("task\n")
            fake_agent = root / "verify_pi_config.py"
            fake_agent.write_text(
                "import json, os, pathlib, sys\n"
                "p = pathlib.Path(os.environ['HOME']) / '.pi/agent/models.json'\n"
                "d = json.loads(p.read_text())\n"
                "actual = d['providers']['openai-benchmark']['baseUrl']\n"
                "model = d['providers']['openai-benchmark']['models'][0]\n"
                "assert model['id'] == 'gpt-6-luna', model\n"
                "assert model['contextWindow'] == 1050000, model\n"
                "sys.exit(0 if actual == os.environ['OPENAI_BASE_URL'] else 7)\n"
            )
            env = {
                **os.environ,
                "AGENT_COMMAND": f"python3 {fake_agent}",
                "AGENT_HARNESS": "pi",
                "BENCHMARK_WORKSPACE": str(repo),
                "HOME": str(home),
                "OPENAI_API_KEY": "not-a-real-key",
                "OPENAI_BASE_URL": "http://openai-proxy:8080/v1",
            }
            subprocess.run(
                ["python3", str(entrypoint), "run", "--model", "gpt-6-luna", "--reasoning", "medium",
                 "--prompt", str(prompt), "--output", str(output)],
                check=True, env=env,
            )

    def test_finalize_accepts_one_selected_harness_as_the_exact_denominator(self):
        tasks = [{"instance_id": f"task-{number}"} for number in range(5)]
        records = [
            {
                "instance_id": task["instance_id"],
                "harness": "carry",
                "status": "evaluated",
                "patch": "",
            }
            for task in tasks
        ]
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            self.worker.finalize(
                tasks=tasks,
                records=records,
                output=output,
                provenance={},
                harnesses=("carry",),
            )
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(report["denominator"], 5)
            self.assertEqual(set(report["harnesses"]), {"carry"})
            self.assertEqual(len(json.loads((output / "records.json").read_text())), 5)

    def test_finalize_preserves_three_independent_attempts_per_task_and_harness(self):
        tasks = [{"instance_id": f"task-{number}"} for number in range(5)]
        records = [
            {
                "instance_id": task["instance_id"], "harness": "carry", "attempt": attempt,
                "status": "evaluated", "patch": "", "resolved": attempt != 2,
                "estimated_cost_usd": 0.1 * attempt,
            }
            for task in tasks for attempt in (1, 2, 3)
        ]
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            self.worker.finalize(
                tasks=tasks, records=records, output=output, provenance={"mode": "official-50"},
                harnesses=("carry",), attempt_numbers=(1, 2, 3),
            )
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(report["denominator"], 15)
            self.assertEqual(report["attempts_per_task_harness"], 3)
            self.assertEqual(report["harnesses"]["carry"]["denominator"], 15)
            summary = report["task_harnesses"]["task-0/carry"]
            self.assertEqual(summary["attempts"], 3)
            self.assertEqual(summary["completed"], 3)
            self.assertEqual(summary["estimated_cost_usd"], 0.6)
            self.assertEqual(summary["resolved"], 2)
            self.assertEqual(summary["resolve_rate"], 2 / 3)
            self.assertTrue(summary["solved_at_least_once"])
            self.assertEqual(len(summary["wilson_95_interval"]), 2)
            self.assertLess(summary["wilson_95_interval"][0], 2 / 3)
            self.assertGreater(summary["wilson_95_interval"][1], 2 / 3)
            summary = (output / "report.md").read_text()
            self.assertIn("Independent attempts per task/harness: 3", summary)
            self.assertIn("| Attempt |", summary)
            self.assertIn("slots/task-0/carry/attempt-01", summary)
            self.assertIn("| task-0 | carry | 3 | 2/3 |", summary)

        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, "15 unique"):
            self.worker.finalize(
                tasks=tasks, records=records[:-1] + [records[0]], output=pathlib.Path(directory),
                provenance={}, harnesses=("carry",), attempt_numbers=(1, 2, 3),
            )

    def test_finalize_accepts_twenty_task_retained_session_denominator(self):
        tasks = [{"instance_id": f"task-{number}"} for number in range(20)]
        records = [
            {"instance_id": task["instance_id"], "harness": "carry", "status": "not-run", "patch": ""}
            for task in tasks
        ]
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            self.worker.finalize(tasks=tasks, records=records, output=output, provenance={}, harnesses=("carry",))
            self.assertEqual(json.loads((output / "report.json").read_text())["denominator"], 20)

        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(
            ValueError, "20 unique"
        ):
            self.worker.finalize(
                tasks=tasks,
                records=records + [{**records[0], "harness": "codex"}],
                output=pathlib.Path(directory),
                provenance={},
                harnesses=("carry",),
            )

    def test_finalize_preserves_failed_slots_and_writes_official_predictions(self):
        tasks = [{"instance_id": f"task-{number}"} for number in range(5)]
        slots = [
            {"instance_id": task["instance_id"], "harness": harness, "status": "agent-failed", "patch": "", "error": "failed"}
            for task in tasks for harness in ("carry", "codex", "pi")
        ]
        slots[0].update(status="evaluated", patch="diff --git a/a b/a\n", resolved=True, error=None)
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            self.worker.finalize(tasks=tasks, records=slots, output=output, provenance={"model": "gpt-5.6-luna"})
            records = json.loads((output / "records.json").read_text())
            self.assertEqual(len(records), 15)
            predictions = [json.loads(line) for line in (output / "predictions.jsonl").read_text().splitlines()]
            self.assertEqual(len(predictions), 15)
            self.assertEqual(predictions[0]["model_patch"], "diff --git a/a b/a\n")
            self.assertEqual(predictions[1]["model_patch"], "")
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(report["denominator"], 15)
            self.assertEqual(report["completed"], 1)
            self.assertEqual(report["resolved"], 1)
            self.assertEqual(report["harnesses"]["carry"]["resolved"], 1)
            self.assertEqual(report["harnesses"]["codex"]["statuses"], {"agent-failed": 5})
            summary = (output / "report.md").read_text()
            self.assertIn("Denominator: 15", summary)
            self.assertIn("Completed: 1", summary)
            self.assertIn("Resolved: 1", summary)

    def test_agent_usage_is_normalized_from_each_harness(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            carry = root / "carry"
            carry.mkdir()
            (carry / "result.json").write_text(json.dumps({"usage": {
                "input_tokens": 100, "cached_input_tokens": 40,
                "cache_write_input_tokens": 10, "output_tokens": 20,
                "reasoning_tokens": 5, "total_tokens": 120,
            }}))
            codex = root / "codex"
            codex.mkdir()
            (codex / "trace.log").write_text("\n".join((
                json.dumps({"type": "turn.completed", "usage": {
                    "input_tokens": 200, "cached_input_tokens": 150,
                    "cache_write_input_tokens": 40, "output_tokens": 30,
                    "reasoning_output_tokens": 7,
                }}),
                json.dumps({"type": "unrelated", "usage": {"input_tokens": 999}}),
            )))
            pi = root / "pi"
            pi.mkdir()
            (pi / "trace.log").write_text("\n".join((
                json.dumps({"type": "message_end", "message": {"role": "assistant", "usage": {
                    "input": 3, "cacheRead": 4, "cacheWrite": 5,
                    "output": 6, "reasoning": 2, "totalTokens": 18,
                }}}),
                json.dumps({"type": "message_end", "message": {"role": "assistant", "usage": {
                    "input": 7, "cacheRead": 8, "cacheWrite": 9,
                    "output": 10, "reasoning": 3, "totalTokens": 34,
                }}}),
            )))

            self.assertEqual(self.worker.load_agent_usage("carry", carry), {
                "input_tokens": 100, "cached_input_tokens": 40,
                "cache_write_input_tokens": 10, "output_tokens": 20,
                "reasoning_tokens": 5, "total_tokens": 120,
            })
            self.assertEqual(self.worker.load_agent_usage("codex", codex), {
                "input_tokens": 200, "cached_input_tokens": 150,
                "cache_write_input_tokens": 40, "output_tokens": 30,
                "reasoning_tokens": 7, "total_tokens": 230,
            })
            self.assertEqual(self.worker.load_agent_usage("pi", pi), {
                "input_tokens": 36, "cached_input_tokens": 12,
                "cache_write_input_tokens": 14, "output_tokens": 16,
                "reasoning_tokens": 5, "total_tokens": 52,
            })

    def test_carry_usage_falls_back_to_completed_trace_responses_when_result_is_missing(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            (output / "result.json").write_text(
                json.dumps({"usage": {}}), encoding="utf-8"
            )
            (output / "trace.jsonl").write_text("\n".join((
                json.dumps({"event": "model_response", "data": {"usage": {
                    "input_tokens": 100, "cached_input_tokens": 40,
                    "cache_write_input_tokens": 10, "output_tokens": 20,
                    "reasoning_tokens": 5, "total_tokens": 120,
                }}}),
                json.dumps({"event": "model_response", "data": {"usage": {
                    "input_tokens": 200, "cached_input_tokens": 80,
                    "cache_write_input_tokens": 20, "output_tokens": 30,
                    "reasoning_tokens": 7, "total_tokens": 230,
                }}}),
                json.dumps({"event": "model_error", "data": {"usage": {"input_tokens": 999}}}),
            )), encoding="utf-8")

            self.assertEqual(self.worker.load_agent_usage("carry", output), {
                "input_tokens": 300, "cached_input_tokens": 120,
                "cache_write_input_tokens": 30, "output_tokens": 50,
                "reasoning_tokens": 12, "total_tokens": 350,
            })

    def test_model_pricing_accounts_for_reads_writes_and_output(self):
        pricing = self.worker.pricing_for_model("gpt-5.6-luna")
        usage = {"input_tokens": 1_000_000, "cached_input_tokens": 400_000,
                 "cache_write_input_tokens": 200_000, "output_tokens": 100_000,
                 "reasoning_tokens": 5, "total_tokens": 1_100_000}
        self.assertEqual(self.worker.estimate_cost_usd(usage, pricing), 0.258)
        self.assertIsNone(self.worker.pricing_for_model("unknown-model"))

    def test_gpt6_luna_pricing_requires_round_size_and_rejects_long_context_estimate(self):
        pricing = self.worker.pricing_for_model("gpt-6-luna")
        self.assertIsNotNone(pricing)
        usage = {"input_tokens": 1_000_000, "cached_input_tokens": 400_000,
                 "cache_write_input_tokens": 200_000, "output_tokens": 100_000}
        self.assertEqual(self.worker.estimate_cost_usd(
            usage, pricing, max_round_input_tokens=272_000,
            observed_round_input_tokens=1_000_000,
        ), 0.119)
        self.assertIsNone(self.worker.estimate_cost_usd(
            usage, pricing, max_round_input_tokens=272_000,
            observed_round_input_tokens=999_999,
        ))
        self.assertIsNone(self.worker.estimate_cost_usd(
            usage, pricing, max_round_input_tokens=272_001,
            observed_round_input_tokens=1_000_000,
        ))
        self.assertIsNone(self.worker.estimate_cost_usd(usage, pricing))

    def test_finalize_reports_per_agent_time_tokens_and_configured_cost(self):
        tasks = [{"instance_id": f"task-{number}"} for number in range(5)]
        records = []
        for task_number, task in enumerate(tasks):
            for index, harness in enumerate(("carry", "codex", "pi"), 1):
                records.append({
                    "instance_id": task["instance_id"], "harness": harness, "status": "evaluated",
                    "patch": "", "error": None,
                    "resolved": task_number == 0 and harness == "carry",
                    "elapsed_seconds": index + 0.25 if task_number == 0 else 0,
                    "estimated_cost_usd": index / 10 if task_number == 0 else 0,
                    "usage": {"input_tokens": index * 10 if task_number == 0 else 0,
                              "cached_input_tokens": index if task_number == 0 else 0,
                              "cache_write_input_tokens": 0,
                              "output_tokens": index * 2 if task_number == 0 else 0,
                              "reasoning_tokens": index if task_number == 0 else 0,
                              "total_tokens": index * 12 if task_number == 0 else 0},
                })
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            self.worker.finalize(tasks=tasks, records=records, output=output, provenance={})
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(report["harnesses"]["carry"]["elapsed_seconds"], 1.25)
            self.assertEqual(report["harnesses"]["codex"]["usage"]["input_tokens"], 20)
            self.assertEqual(report["harnesses"]["pi"]["estimated_cost_usd"], 0.3)
            summary = (output / "report.md").read_text()
            self.assertIn("## Agent runs", summary)
            self.assertIn("| task-0 | carry | evaluated | yes | 1.250 | 12 | $0.100000 |", summary)

    def test_finalize_rejects_missing_duplicate_or_replaced_slots(self):
        tasks = [{"instance_id": f"task-{number}"} for number in range(5)]
        records = [
            {"instance_id": task["instance_id"], "harness": harness, "status": "failed", "patch": ""}
            for task in tasks for harness in ("carry", "codex", "pi")
        ]
        for broken in (records[:-1], records[:-1] + [records[0]], records[:-1] + [{**records[-1], "instance_id": "replacement"}]):
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, "15 unique"):
                self.worker.finalize(tasks=tasks, records=broken, output=pathlib.Path(directory), provenance={})

    def test_official_finalize_requires_exactly_150_slots(self):
        tasks = [{"instance_id": f"task-{number:02d}"} for number in range(50)]
        records = [
            {"instance_id": task["instance_id"], "harness": harness, "status": "agent-failed", "patch": ""}
            for task in tasks for harness in ("carry", "codex", "pi")
        ]
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            self.worker.finalize(tasks=tasks, records=records, output=output, provenance={})
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(report["denominator"], 150)
            self.assertEqual({harness: values["denominator"] for harness, values in report["harnesses"].items()}, {
                "carry": 50, "codex": 50, "pi": 50,
            })
            self.assertEqual(len(json.loads((output / "records.json").read_text())), 150)

        for broken in (records[:-1], records[:-1] + [records[0]]):
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, "150 unique"):
                self.worker.finalize(tasks=tasks, records=broken, output=pathlib.Path(directory), provenance={})

    def test_mode_selection_and_shards_preserve_frozen_order(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        smoke = [frozen[index] for index in (0, 25, 40, 45, 49)]
        self.assertEqual(self.worker.selection_for_mode(frozen, "smoke-5", smoke), smoke)
        self.assertEqual(self.worker.selection_for_mode(frozen, "long-smoke-5", smoke), smoke)
        self.assertEqual(self.worker.selection_for_mode(frozen, "official-50", smoke), frozen)
        self.assertEqual(self.worker.selection_for_mode(frozen, "long-official-50", smoke), frozen)
        self.assertEqual(self.worker.selection_for_mode(frozen, "session-20", smoke), frozen[:20])

    def test_long_modes_use_the_separate_long_trajectory_manifests(self):
        self.assertEqual(
            self.worker.selection_manifest_names("long-smoke-5"),
            ("swe-bench-verified-long-trajectory-50.json", "swe-bench-verified-long-trajectory-smoke-5.json"),
        )
        self.assertEqual(
            self.worker.selection_manifest_names("long-official-50"),
            ("swe-bench-verified-long-trajectory-50.json", None),
        )
        self.assertEqual(
            self.worker.selection_manifest_names("smoke-5"),
            ("swe-bench-verified-50.json", "swe-bench-verified-smoke-5.json"),
        )

    def test_official_mode_selects_the_frozen_manifest_with_one_declared_attempt(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        self.assertEqual(self.worker.selection_for_mode(frozen, "official-50"), frozen)
        self.assertEqual(
            self.worker.official_attempt_numbers(
                {"BENCHMARK_ATTEMPTS": "4", "BENCHMARK_ATTEMPT": "2"}, "official-50"
            ),
            (2,),
        )
        for config in (
            {"BENCHMARK_ATTEMPTS": "0", "BENCHMARK_ATTEMPT": "1"},
            {"BENCHMARK_ATTEMPTS": "4", "BENCHMARK_ATTEMPT": "0"},
            {"BENCHMARK_ATTEMPTS": "4", "BENCHMARK_ATTEMPT": "5"},
            {"BENCHMARK_ATTEMPTS": "11", "BENCHMARK_ATTEMPT": "1"},
        ):
            with self.subTest(config=config), self.assertRaisesRegex(ValueError, "official-50 attempts"):
                self.worker.official_attempt_numbers(config, "official-50")

    def test_session_smoke_uses_the_frozen_smoke_order_and_permits_exactly_one_native_harness(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        smoke = [frozen[index] for index in (0, 25, 40, 45, 49)]
        self.assertEqual(
            self.worker.selection_for_mode(frozen, "session-smoke-5", smoke), smoke,
        )
        for harness in ("carry", "codex", "pi"):
            with self.subTest(harness=harness):
                self.worker.validate_session_mode("session-smoke-5", (harness,))
        self.assertEqual(self.worker.agent_concurrency_for_mode({}, "session-smoke-5"), 1)
        for harnesses in (("carry", "codex"), ("carry", "pi"), ("codex", "pi"),
                          ("carry", "codex", "pi")):
            with self.subTest(harnesses=harnesses), self.assertRaisesRegex(ValueError, "exactly one"):
                self.worker.validate_session_mode("session-smoke-5", harnesses)
        shards = self.worker.ordered_shards(frozen, 10)
        self.assertEqual([len(shard) for shard in shards], [10] * 5)
        self.assertEqual([item for shard in shards for item in shard], frozen)
        with self.assertRaises(ValueError):
            self.worker.selection_for_mode(frozen[:-1], "official-50", smoke)
        with self.assertRaisesRegex(ValueError, "smoke manifest"):
            self.worker.selection_for_mode(frozen, "smoke-5", smoke[:-1] + ["outside"])

    def test_official_mode_defaults_to_five_agent_slots(self):
        self.assertEqual(self.worker.agent_concurrency_for_mode({}, "official-50"), 5)
        self.assertEqual(self.worker.agent_concurrency_for_mode({}, "smoke-5"), 3)
        self.assertEqual(self.worker.agent_concurrency_for_mode({"AGENT_CONCURRENCY": "4"}, "official-50"), 4)

    def test_official_phase_budgets_fit_the_five_and_a_quarter_hour_worker_envelope(self):
        limits = self.worker.official_phase_limits()
        self.assertEqual(limits["worker_seconds"], 18_900)
        self.assertEqual(limits["agent_seconds"], 4_500)
        self.assertEqual(
            limits["preparation_seconds"] + limits["agent_seconds"]
            + limits["evaluation_seconds"] + limits["setup_reserve_seconds"],
            limits["worker_seconds"],
        )
        readiness_worst_case = 10 * 180  # Fifty tasks, five concurrent checks.
        self.assertLess(readiness_worst_case, limits["preparation_seconds"])
        evaluator_worst_case = 30 * (270 + 45)  # Thirty all-harness evaluator shards.
        self.assertLess(evaluator_worst_case, limits["evaluation_seconds"])
        # Five bounded slots cannot reserve every per-slot maximum inside one
        # phase, so the phase deadline remains the fail-closed control.
        self.assertEqual(150 * 360 // 5, 10_800)
        self.assertLess(limits["agent_seconds"], 150 * 360 // 5)

    def test_official_outcomes_require_exact_nonoverlapping_coverage(self):
        outcomes = {
            "completed_ids": {"a", "b"}, "resolved_ids": {"a"}, "unresolved_ids": {"b"},
            "empty_patch_ids": {"c"}, "error_ids": {"d"}, "incomplete_ids": {"e"},
        }
        self.worker.validate_official_outcomes(outcomes, ["a", "b", "c", "d", "e"])
        for broken in (
            {**outcomes, "resolved_ids": {"a", "c"}},
            {**outcomes, "error_ids": set()},
            {**outcomes, "unresolved_ids": {"a", "b"}},
        ):
            with self.assertRaises(ValueError):
                self.worker.validate_official_outcomes(broken, ["a", "b", "c", "d", "e"])

    def test_official_outcomes_accept_swebench_completed_error_overlap(self):
        # SWE-bench 4.1.0 adds malformed existing reports to both completed_ids
        # and error_ids, but not to resolved_ids or unresolved_ids.
        outcomes = {
            "completed_ids": {"resolved", "unresolved", "malformed"},
            "resolved_ids": {"resolved"}, "unresolved_ids": {"unresolved"},
            "empty_patch_ids": {"empty"},
            "error_ids": {"malformed", "missing-report"},
            "incomplete_ids": {"incomplete"},
        }
        expected = ["resolved", "unresolved", "malformed", "empty", "missing-report", "incomplete"]
        self.worker.validate_official_outcomes(outcomes, expected)
        self.assertEqual(
            self.worker.status_for_official_outcome("malformed", outcomes),
            "evaluation-error",
        )

    def test_official_completion_fails_closed_on_unknown_evaluator_outcomes(self):
        records = [
            {"instance_id": "resolved", "harness": "carry", "status": "evaluated"},
            {"instance_id": "empty", "harness": "carry", "status": "empty-patch"},
            {"instance_id": "error", "harness": "carry", "status": "evaluation-error"},
            {"instance_id": "missing", "harness": "carry", "status": "evaluation-incomplete"},
            {"instance_id": "failed", "harness": "carry", "status": "evaluator-failed"},
            {"instance_id": "agent", "harness": "carry", "status": "agent-failed"},
        ]
        with self.assertRaisesRegex(
                RuntimeError, "official evaluation incomplete for 4 slots.*agent.*error.*failed.*missing"):
            self.worker.require_complete_official_evaluations(records)

        self.worker.require_complete_official_evaluations(records[:2])

    def test_official_agent_timeout_without_patch_is_terminal_failure(self):
        records = [{
            "instance_id": "timed-out", "harness": "carry", "status": "agent-failed",
            "timed_out": True, "patch": "", "resolved": True, "phase_budget_limited": False,
        }]
        outcomes = {
            "resolved_ids": set(), "unresolved_ids": set(), "empty_patch_ids": set(),
            "error_ids": set(), "incomplete_ids": set(), "completed_ids": set(),
        }
        self.worker.apply_official_outcomes(records, outcomes)
        self.assertEqual(records[0]["status"], "agent-failed")
        self.assertFalse(records[0]["resolved"])
        self.worker.require_complete_official_evaluations(records)

    def test_official_evaluator_can_resolve_captured_patch_after_agent_timeout(self):
        records = [{
            "instance_id": "timed-out", "harness": "carry", "status": "agent-failed",
            "timed_out": True, "phase_budget_limited": False,
            "patch": "diff --git a/a.py b/a.py\n", "error": "agent timed out",
            "resolved": False,
        }]
        outcomes = {
            "resolved_ids": {"timed-out"}, "unresolved_ids": set(), "empty_patch_ids": set(),
            "error_ids": set(), "incomplete_ids": set(), "completed_ids": {"timed-out"},
        }
        self.worker.apply_official_outcomes(records, outcomes)
        self.assertEqual(records[0]["status"], "evaluated")
        self.assertTrue(records[0]["resolved"])
        self.assertTrue(records[0]["timed_out"])
        self.assertEqual(records[0]["error"], "agent timed out")
        self.worker.require_complete_official_evaluations(records)

    def test_timeout_with_captured_patch_requires_official_grade(self):
        records = [{
            "instance_id": "timed-out", "harness": "carry", "status": "agent-failed",
            "timed_out": True, "phase_budget_limited": False,
            "patch": "diff --git a/a.py b/a.py\n", "resolved": False,
        }]
        outcomes = {
            "resolved_ids": set(), "unresolved_ids": set(), "empty_patch_ids": set(),
            "error_ids": {"timed-out"}, "incomplete_ids": set(), "completed_ids": set(),
        }
        self.worker.apply_official_outcomes(records, outcomes)
        with self.assertRaisesRegex(RuntimeError, "official evaluation incomplete.*timed-out"):
            self.worker.require_complete_official_evaluations(records)

    def test_timeout_with_patch_obeys_official_empty_patch_verdict(self):
        records = [{
            "instance_id": "timed-out", "harness": "carry", "status": "agent-failed",
            "timed_out": True, "phase_budget_limited": False,
            "patch": "diff --git a/a.py b/a.py\n", "resolved": False,
        }]
        outcomes = {
            "resolved_ids": set(), "unresolved_ids": set(), "empty_patch_ids": {"timed-out"},
            "error_ids": set(), "incomplete_ids": set(), "completed_ids": set(),
        }
        self.worker.apply_official_outcomes(records, outcomes)
        self.assertEqual(records[0]["status"], "empty-patch")
        self.assertFalse(records[0]["resolved"])
        self.worker.require_complete_official_evaluations(records)

    def test_task_timeout_does_not_exhaust_the_agent_phase_budget(self):
        self.assertFalse(self.worker.agent_phase_budget_exhausted({
            "status": "agent-failed", "timed_out": True, "phase_budget_limited": False,
        }))
        self.assertTrue(self.worker.agent_phase_budget_exhausted({
            "status": "agent-failed", "timed_out": True, "phase_budget_limited": True,
        }))
        self.assertTrue(self.worker.agent_phase_budget_exhausted({
            "status": "agent-budget-exhausted", "timed_out": False,
        }))

    def test_official_outcomes_do_not_overwrite_agent_failures(self):
        records = [
            {"instance_id": "failed", "harness": "codex", "status": "agent-failed", "resolved": False},
            {"instance_id": "finished", "harness": "codex", "status": "agent-completed", "resolved": False},
        ]
        outcomes = {
            "resolved_ids": set(), "unresolved_ids": set(),
            "empty_patch_ids": {"failed", "finished"},
            "error_ids": set(), "incomplete_ids": set(), "completed_ids": set(),
        }
        self.worker.apply_official_outcomes(records, outcomes)
        self.assertEqual(records[0]["status"], "agent-failed")
        self.assertEqual(records[1]["status"], "empty-patch")

    def test_official_all_harness_execution_shares_preparation_and_uses_five_task_evaluator_shards(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        dataset = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
             "problem_statement": f"problem {instance_id}", "test_patch": "gold"}
            for instance_id in frozen
        ]
        fake_datasets = types.SimpleNamespace(load_dataset=lambda *args, **kwargs: dataset)
        materialized_shards = []
        evaluation_shards = []
        execution_events = []

        def fake_materialize(*, records, selected_ids, root, clone, harnesses):
            materialized_shards.append(list(selected_ids))
            tasks = []
            for instance_id in selected_ids:
                task = {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
                        "problem_statement": f"problem {instance_id}"}
                tasks.append(task)
                for harness in harnesses:
                    (root / "tasks" / instance_id / harness / "repo").mkdir(parents=True)
                (root / "tasks" / instance_id / "input").mkdir(parents=True)
            return tasks

        def fake_agent(**kwargs):
            execution_events.append(("agent", kwargs["instance_id"]))
            self.assertEqual(kwargs["image"], f"prepared:{kwargs['instance_id']}")
            self.assertEqual(kwargs["harness_bundle"].name, kwargs["harness"])
            if kwargs["instance_id"] == frozen[0] and kwargs["harness"] == "carry":
                return {"instance_id": kwargs["instance_id"], "harness": kwargs["harness"],
                        "status": "agent-failed", "patch": "", "error": "task timed out",
                        "attempts": 1, "retries": 0, "response_retries": 0, "timed_out": True}
            return {"instance_id": kwargs["instance_id"], "harness": kwargs["harness"],
                    "status": "agent-completed", "patch": "", "error": None,
                    "attempts": 1, "retries": 0,
                    "response_retries": 2 if kwargs["harness"] == "carry" else 0}

        def fake_evaluation(*, instance_ids, output, **kwargs):
            evaluation_shards.append(list(instance_ids))
            output.mkdir(parents=True, exist_ok=True)
            (output / "report.json").write_text(json.dumps({
                "completed_ids": list(instance_ids), "resolved_ids": [],
                "unresolved_ids": list(instance_ids), "empty_patch_ids": [],
                "error_ids": [], "incomplete_ids": [],
            }))

        def fake_resolve(*, records, **_kwargs):
            execution_events.append(("resolve", len(records)))
            return {
                record["instance_id"]: {
                    "cache_key": "a" * 64,
                    "agent_image": {
                        "tag": f"prepared:{record['instance_id']}",
                        "image_id": "sha256:" + "d" * 64,
                        "resolved_digest": "registry.example/tasks@sha256:" + "f" * 64,
                    },
                    "evaluator_image": {
                        "tag": f"evaluator:{record['instance_id']}",
                        "image_id": "sha256:" + "e" * 64,
                        "resolved_digest": "registry.example/tasks@sha256:" + "0" * 64,
                    },
                    "source_task_image": f"source:{record['instance_id']}",
                    "dockerfile_sha256": "1" * 64,
                }
                for record in records
            }

        config = {
            "BENCHMARK_MODE": "official-50", "BENCHMARK_HARNESS": "all",
            "RUN_ID": "official-test",
            "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CARRY_BASE_IMAGE": "rust@sha256:" + "b" * 64,
            "CODEX_VERSION": "1.2.3", "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna", "REASONING": "medium",
            "TASK_IMAGE_REPOSITORY": "registry.example/tasks",
            "TASK_IMAGE_CATALOG": "registry.example/tasks@sha256:" + "f" * 64,
            "AGENT_CONCURRENCY": "5",
        }
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source, work, output = root / "source", root / "work", root / "output"
            (source / "benchmarks").mkdir(parents=True)
            (source / "benchmarks" / "swe-bench-verified-50.json").write_text(
                json.dumps({"instance_ids": frozen})
            )
            secret = root / "openai-key"
            secret.write_text("not-a-real-key")
            with mock.patch.dict(sys.modules, {"datasets": fake_datasets}), \
                    mock.patch.object(self.worker, "materialize", side_effect=fake_materialize), \
                    mock.patch.object(self.worker, "build_images", return_value={
                        harness: {"tag": f"image:{harness}", "image_id": "sha256:" + "c" * 64}
                        for harness in self.worker.HARNESSES
                    }) as build, \
                    mock.patch.object(self.worker, "resolve_task_environments", side_effect=fake_resolve), \
                    mock.patch.object(self.worker, "export_harness_bundles", side_effect=lambda _images, root: {
                        harness: root / harness for harness in self.worker.HARNESSES
                    }) as export, \
                    mock.patch.object(self.worker, "run_isolated_agent", side_effect=fake_agent), \
                    mock.patch.object(self.worker, "run_official_evaluation", side_effect=fake_evaluation), \
                    mock.patch.object(
                        self.worker, "require_complete_official_evaluations",
                        wraps=self.worker.require_complete_official_evaluations,
                    ) as require_complete:
                with mock.patch.dict(os.environ, {
                    "OPENAI_API_KEY": "not-a-real-key", "OPENAI_SECRET_FILE": str(secret),
                }, clear=False):
                    self.worker.execute_benchmark(
                        source=source, work=work, output=output, config=config
                    )
                    self.assertNotIn("OPENAI_API_KEY", os.environ)
                    self.assertNotIn("OPENAI_SECRET_FILE", os.environ)

            self.assertEqual(build.call_count, 1)
            export.assert_called_once()
            self.assertEqual(execution_events[0], ("resolve", 50))
            self.assertEqual(len(execution_events), 151)
            self.assertTrue(all(event[0] == "agent" for event in execution_events[1:]))
            require_complete.assert_called_once()
            self.assertEqual(len(require_complete.call_args.args[0]), 150)
            self.assertEqual([len(shard) for shard in materialized_shards], [10] * 5)
            self.assertEqual(len(evaluation_shards), 30)
            self.assertTrue(all(len(shard) == 5 for shard in evaluation_shards))
            self.assertFalse(secret.exists())
            self.assertFalse((work / "agent-shards").exists())
            report = json.loads((output / "report.json").read_text())
            self.assertEqual((report["denominator"], report["completed"]), (150, 149))
            self.assertEqual(report["resolved"], 0)
            self.assertEqual(report["harnesses"]["carry"]["statuses"], {"agent-failed": 1, "evaluated": 49})
            self.assertEqual(set(report["harnesses"]), set(self.worker.HARNESSES))
            self.assertEqual(report["harnesses"]["carry"]["response_retries"], 98)
            self.assertEqual(report["harnesses"]["codex"]["response_retries"], 0)
            self.assertEqual(report["harnesses"]["pi"]["response_retries"], 0)
            limits = report["provenance"]["images"]["execution_limits"]
            self.assertEqual(limits["agent_shard_size"], 10)
            self.assertEqual(limits["agent_concurrency"], 5)
            self.assertEqual(limits["evaluator_shard_size"], 5)
            self.assertEqual(limits["evaluator_concurrency"], 5)

    def test_session_smoke_runs_carry_slots_in_manifest_order_with_one_retained_context(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        smoke = [frozen[index] for index in (0, 25, 40, 45, 49)]
        dataset = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
             "problem_statement": f"problem {instance_id}"}
            for instance_id in frozen
        ]
        fake_datasets = types.SimpleNamespace(load_dataset=lambda *args, **kwargs: dataset)
        materialized, executed, evaluated = [], [], []

        def fake_materialize(*, selected_ids, root, harnesses, **_kwargs):
            materialized.append(list(selected_ids))
            tasks = []
            for instance_id in selected_ids:
                tasks.append({"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
                              "problem_statement": f"problem {instance_id}"})
                (root / "tasks" / instance_id / "carry" / "repo").mkdir(parents=True)
                (root / "tasks" / instance_id / "input").mkdir(parents=True)
            self.assertEqual(harnesses, ("carry",))
            return tasks

        def fake_agent(**kwargs):
            position = len(executed) + 1
            self.assertEqual(kwargs["instance_id"], smoke[position - 1])
            self.assertEqual(kwargs["harness"], "carry")
            if position == 1:
                self.assertIsNone(kwargs["resume_session"])
            else:
                self.assertEqual(
                    kwargs["resume_session"],
                    output / "slots" / smoke[position - 2] / "carry" / "attempt-01",
                )
            (kwargs["output"] / "context-state.json").write_text(
                f"context-{position}", encoding="utf-8"
            )
            executed.append(kwargs["instance_id"])
            return {"instance_id": kwargs["instance_id"], "harness": "carry",
                    "status": "agent-completed", "patch": "", "error": None,
                    "attempts": 1, "retries": 0, "response_retries": 0}

        def fake_evaluation(*, instance_ids, output, **_kwargs):
            evaluated.append(list(instance_ids))
            output.mkdir(parents=True, exist_ok=True)
            (output / "report.json").write_text(json.dumps({
                "completed_ids": list(instance_ids), "resolved_ids": [],
                "unresolved_ids": list(instance_ids), "empty_patch_ids": [],
                "error_ids": [], "incomplete_ids": [],
            }))

        def fake_resolve(*, records, **_kwargs):
            return {record["instance_id"]: {
                "cache_key": "a" * 64,
                "agent_image": {"tag": f"prepared:{record['instance_id']}"},
                "evaluator_image": {"tag": f"evaluator:{record['instance_id']}"},
                "source_task_image": f"source:{record['instance_id']}", "dockerfile_sha256": "1" * 64,
            } for record in records}

        config = {
            "BENCHMARK_MODE": "session-smoke-5", "BENCHMARK_HARNESS": "carry", "RUN_ID": "session-test",
            "CARRY_LEASE_REVIEW_POLICY": "batch-ordinary",
            "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CARRY_BASE_IMAGE": "rust@sha256:" + "b" * 64,
            "CODEX_VERSION": "1.2.3", "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna", "REASONING": "medium",
            "TASK_IMAGE_REPOSITORY": "registry.example/tasks",
            "TASK_IMAGE_CATALOG": "registry.example/tasks@sha256:" + "f" * 64,
        }
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source, work, output = root / "source", root / "work", root / "output"
            (source / "benchmarks").mkdir(parents=True)
            (source / "benchmarks" / "swe-bench-verified-50.json").write_text(json.dumps({"instance_ids": frozen}))
            (source / "benchmarks" / "swe-bench-verified-smoke-5.json").write_text(json.dumps({"instance_ids": smoke}))
            secret = root / "openai-key"; secret.write_text("not-a-real-key")
            with mock.patch.dict(sys.modules, {"datasets": fake_datasets}), \
                    mock.patch.object(self.worker, "materialize", side_effect=fake_materialize), \
                    mock.patch.object(self.worker, "build_images", return_value={
                        harness: {"tag": f"image:{harness}", "image_id": "sha256:" + "c" * 64}
                        for harness in self.worker.HARNESSES
                    }), \
                    mock.patch.object(self.worker, "export_harness_bundles", side_effect=lambda _images, bundle_root: {
                        harness: bundle_root / harness for harness in self.worker.HARNESSES
                    }), \
                    mock.patch.object(self.worker, "resolve_task_environments", side_effect=fake_resolve), \
                    mock.patch.object(self.worker, "run_isolated_agent", side_effect=fake_agent), \
                    mock.patch.object(self.worker, "run_official_evaluation", side_effect=fake_evaluation), \
                    mock.patch.dict(os.environ, {"OPENAI_API_KEY": "not-a-real-key", "OPENAI_SECRET_FILE": str(secret)}, clear=False):
                self.worker.execute_benchmark(source=source, work=work, output=output, config=config)

            records = json.loads((output / "records.json").read_text())
            self.assertEqual(materialized, [smoke])
            self.assertEqual(executed, smoke)
            self.assertEqual(evaluated, [smoke])
            self.assertEqual([record["session_position"] for record in records], [1, 2, 3, 4, 5])
            self.assertEqual(
                [record["session_state_sha256"] for record in records],
                [hashlib.sha256(f"context-{position}".encode()).hexdigest() for position in range(1, 6)],
            )
            self.assertEqual(
                [record["source_session_state_sha256"] for record in records[1:]],
                [hashlib.sha256(f"context-{position}".encode()).hexdigest() for position in range(1, 5)],
            )
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(report["denominator"], 5)
            self.assertEqual(report["provenance"]["carry_lease_review_policy"], "batch-ordinary")
            limits = report["provenance"]["images"]["execution_limits"]
            self.assertEqual((limits["agent_concurrency"], limits["agent_shard_size"]), (1, 5))

    def test_session_smoke_runs_pi_slots_with_one_native_worker_local_session(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        smoke = [frozen[index] for index in (0, 25, 40, 45, 49)]
        dataset = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
             "problem_statement": f"problem {instance_id}"}
            for instance_id in frozen
        ]
        fake_datasets = types.SimpleNamespace(load_dataset=lambda *args, **kwargs: dataset)
        executed = []

        def fake_materialize(*, selected_ids, root, harnesses, **_kwargs):
            self.assertEqual(harnesses, ("pi",))
            tasks = []
            for instance_id in selected_ids:
                tasks.append({"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
                              "problem_statement": f"problem {instance_id}"})
                (root / "tasks" / instance_id / "pi" / "repo").mkdir(parents=True)
                (root / "tasks" / instance_id / "input").mkdir(parents=True)
            return tasks

        def fake_agent(**kwargs):
            position = len(executed) + 1
            session_file = kwargs["pi_session_dir"] / "session.jsonl"
            self.assertEqual(kwargs["harness"], "pi")
            self.assertIsNone(kwargs["resume_session"])
            self.assertEqual(kwargs["pi_session_dir"], work / "session" / "pi")
            if position > 1:
                self.assertEqual(session_file.read_text(), f"session-{position - 1}")
            session_file.write_text(f"session-{position}", encoding="utf-8")
            executed.append(kwargs["instance_id"])
            return {"instance_id": kwargs["instance_id"], "harness": "pi",
                    "status": "agent-completed", "patch": "", "error": None,
                    "attempts": 1, "retries": 0, "response_retries": 0}

        def fake_evaluation(*, instance_ids, output, **_kwargs):
            output.mkdir(parents=True, exist_ok=True)
            (output / "report.json").write_text(json.dumps({
                "completed_ids": list(instance_ids), "resolved_ids": [],
                "unresolved_ids": list(instance_ids), "empty_patch_ids": [],
                "error_ids": [], "incomplete_ids": [],
            }))

        def fake_resolve(*, records, **_kwargs):
            return {record["instance_id"]: {
                "cache_key": "a" * 64,
                "agent_image": {"tag": f"prepared:{record['instance_id']}"},
                "evaluator_image": {"tag": f"evaluator:{record['instance_id']}"},
                "source_task_image": f"source:{record['instance_id']}", "dockerfile_sha256": "1" * 64,
            } for record in records}

        config = {
            "BENCHMARK_MODE": "session-smoke-5", "BENCHMARK_HARNESS": "pi", "RUN_ID": "session-test",
            "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CARRY_BASE_IMAGE": "rust@sha256:" + "b" * 64,
            "CODEX_VERSION": "1.2.3", "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna", "REASONING": "medium",
            "TASK_IMAGE_REPOSITORY": "registry.example/tasks",
            "TASK_IMAGE_CATALOG": "registry.example/tasks@sha256:" + "f" * 64,
        }
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source, work, output = root / "source", root / "work", root / "output"
            (source / "benchmarks").mkdir(parents=True)
            (source / "benchmarks" / "swe-bench-verified-50.json").write_text(json.dumps({"instance_ids": frozen}))
            (source / "benchmarks" / "swe-bench-verified-smoke-5.json").write_text(json.dumps({"instance_ids": smoke}))
            secret = root / "openai-key"; secret.write_text("not-a-real-key")
            with mock.patch.dict(sys.modules, {"datasets": fake_datasets}), \
                    mock.patch.object(self.worker, "materialize", side_effect=fake_materialize), \
                    mock.patch.object(self.worker, "build_images", return_value={
                        harness: {"tag": f"image:{harness}", "image_id": "sha256:" + "c" * 64}
                        for harness in self.worker.HARNESSES
                    }), \
                    mock.patch.object(self.worker, "export_harness_bundles", side_effect=lambda _images, bundle_root: {
                        harness: bundle_root / harness for harness in self.worker.HARNESSES
                    }), \
                    mock.patch.object(self.worker, "resolve_task_environments", side_effect=fake_resolve), \
                    mock.patch.object(self.worker, "run_isolated_agent", side_effect=fake_agent), \
                    mock.patch.object(self.worker, "run_official_evaluation", side_effect=fake_evaluation), \
                    mock.patch.dict(os.environ, {"OPENAI_API_KEY": "not-a-real-key", "OPENAI_SECRET_FILE": str(secret)}, clear=False):
                self.worker.execute_benchmark(source=source, work=work, output=output, config=config)

            records = json.loads((output / "records.json").read_text())
            self.assertEqual(executed, smoke)
            self.assertEqual([record["session_position"] for record in records], [1, 2, 3, 4, 5])
            self.assertEqual({record["session_id"] for record in records}, {"session-test:pi"})
            self.assertEqual({record["session_file"] for record in records}, {"session.jsonl"})
            self.assertEqual(
                [record["session_file_sha256"] for record in records],
                [hashlib.sha256(f"session-{position}".encode()).hexdigest() for position in range(1, 6)],
            )
            self.assertEqual(
                [record["source_session_file_sha256"] for record in records[1:]],
                [hashlib.sha256(f"session-{position}".encode()).hexdigest() for position in range(1, 5)],
            )
            provenance = json.loads((output / "report.json").read_text())["provenance"]
            self.assertEqual(provenance["session_id"], "session-test:pi")
            self.assertEqual(provenance["session_file"], "session.jsonl")
            self.assertEqual(provenance["session_storage"], "worker-local")

    def test_preparation_failure_stops_before_model_spend(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        dataset = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
             "problem_statement": f"problem {instance_id}"}
            for instance_id in frozen
        ]
        config = {
            "BENCHMARK_MODE": "official-50", "BENCHMARK_HARNESS": "carry",
            "RUN_ID": "official-test", "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CARRY_BASE_IMAGE": "rust@sha256:" + "b" * 64,
            "CODEX_VERSION": "1.2.3", "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna", "REASONING": "medium",
            "TASK_IMAGE_REPOSITORY": "registry.example/tasks",
            "TASK_IMAGE_CATALOG": "registry.example/tasks@sha256:" + "f" * 64,
        }
        fake_datasets = types.SimpleNamespace(load_dataset=lambda *args, **kwargs: dataset)
        images = {
            harness: {"tag": f"image:{harness}", "image_id": "sha256:" + "c" * 64}
            for harness in self.worker.HARNESSES
        }
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source, work, output = root / "source", root / "work", root / "output"
            (source / "benchmarks").mkdir(parents=True)
            (source / "benchmarks" / "swe-bench-verified-50.json").write_text(
                json.dumps({"instance_ids": frozen})
            )
            with mock.patch.dict(sys.modules, {"datasets": fake_datasets}), \
                    mock.patch.object(self.worker, "build_images", return_value=images), \
                    mock.patch.object(self.worker, "export_harness_bundles", return_value={
                        harness: work / "bundles" / harness for harness in self.worker.HARNESSES
                    }), \
                    mock.patch.object(
                        self.worker, "resolve_task_environments",
                        side_effect=RuntimeError("catalog image unavailable"),
                    ), \
                    mock.patch.object(self.worker, "run_isolated_agent") as run_agent:
                with self.assertRaisesRegex(RuntimeError, "catalog image unavailable"):
                    self.worker.execute_benchmark(
                        source=source, work=work, output=output, config=config
                    )
            run_agent.assert_not_called()
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(report["provenance"]["phase"], "preparation-failed")
            self.assertIsNone(report["harnesses"]["carry"]["estimated_cost_usd"])

    def test_official_build_failure_still_preserves_all_selected_planned_slots(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        dataset = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
             "problem_statement": f"problem {instance_id}"}
            for instance_id in frozen
        ]
        config = {
            "BENCHMARK_MODE": "official-50", "BENCHMARK_HARNESS": "carry",
            "RUN_ID": "official-test",
            "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CARRY_BASE_IMAGE": "rust@sha256:" + "b" * 64,
            "CODEX_VERSION": "1.2.3", "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna", "REASONING": "medium",
            "TASK_IMAGE_REPOSITORY": "registry.example/tasks",
            "TASK_IMAGE_CATALOG": "registry.example/tasks@sha256:" + "f" * 64,
        }
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source, work, output = root / "source", root / "work", root / "output"
            (source / "benchmarks").mkdir(parents=True)
            (source / "benchmarks" / "swe-bench-verified-50.json").write_text(
                json.dumps({"instance_ids": frozen})
            )
            fake_datasets = types.SimpleNamespace(load_dataset=lambda *args, **kwargs: dataset)
            with mock.patch.dict(sys.modules, {"datasets": fake_datasets}), \
                    mock.patch.object(self.worker, "build_images", side_effect=RuntimeError("build failed")):
                with self.assertRaisesRegex(RuntimeError, "build failed"):
                    self.worker.execute_benchmark(
                        source=source, work=work, output=output, config=config
                    )
            records = json.loads((output / "records.json").read_text())
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(len(records), 50)
            self.assertEqual({record["status"] for record in records}, {"not-run"})
            self.assertEqual(report["provenance"]["phase"], "planned")

    def test_run_agent_failure_is_a_record_with_empty_patch_and_no_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output"):
                (root / name).mkdir()
            with mock.patch.object(self.worker.subprocess, "run", side_effect=RuntimeError("agent stopped")) as run:
                record = self.worker.run_agent(
                    instance_id="task-1", harness="codex", image="codex:run",
                    repo=root / "repo", harness_bundle=root / "repo",
                    task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium",
                    network="internal", proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                )
            self.assertEqual(run.call_count, 1)
            self.assertEqual(record["status"], "agent-failed")
            self.assertEqual(record["patch"], "")
            self.assertIn("agent stopped", record["error"])
            self.assertEqual((record["attempts"], record["retries"]), (1, 0))
            self.assertEqual(record["response_retries"], 0)

    def test_run_agent_marks_container_timeout_exit_124(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output"):
                (root / name).mkdir()
            def fake_run(command, **_kwargs):
                if command[:2] == ["docker", "run"]:
                    raise subprocess.CalledProcessError(124, command)
                return mock.Mock(returncode=0, stdout="")

            (root / "output" / "trace.jsonl").write_text(json.dumps({
                "event": "model_response", "data": {"usage": {
                    "input_tokens": 100, "cached_input_tokens": 40,
                    "cache_write_input_tokens": 10, "output_tokens": 20,
                    "reasoning_tokens": 5, "total_tokens": 120,
                }},
            }) + "\n", encoding="utf-8")
            pricing = self.worker.pricing_for_model("gpt-5.6-luna")
            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run):
                record = self.worker.run_agent(
                    instance_id="task-1", harness="carry", image="carry:run",
                    repo=root / "repo", harness_bundle=root / "repo",
                    task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium", pricing=pricing,
                    network="internal", proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                )
            self.assertEqual(record["status"], "agent-failed")
            self.assertTrue(record["timed_out"])
            self.assertEqual(record["usage"]["total_tokens"], 120)
            self.assertEqual(record["estimated_cost_usd"], 0.000037)

    def test_run_agent_surfaces_carry_response_retries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output"):
                (root / name).mkdir()

            def fake_run(*_args, **_kwargs):
                (root / "output" / "final.patch").write_text("diff --git a/a b/a\n", encoding="utf-8")
                (root / "output" / "result.json").write_text(
                    json.dumps({"response_retries": 4, "usage": {
                        "input_tokens": 100, "cached_input_tokens": 40,
                        "cache_write_input_tokens": 10, "output_tokens": 20,
                        "reasoning_tokens": 5, "total_tokens": 120,
                    }}), encoding="utf-8"
                )

            pricing = self.worker.pricing_for_model("gpt-5.6-luna")
            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run), \
                    mock.patch.object(self.worker.time, "monotonic", side_effect=[10.0, 12.5]), \
                    mock.patch("sys.stdout", new_callable=__import__("io").StringIO) as stdout:
                record = self.worker.run_agent(
                    instance_id="task-1", harness="carry", image="carry:run",
                    repo=root / "repo", harness_bundle=root / "repo",
                    task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium", pricing=pricing,
                    network="internal", proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                )

            self.assertEqual(record["status"], "agent-completed")
            self.assertEqual(record["retries"], 0)
            self.assertEqual(record["response_retries"], 4)
            self.assertEqual(record["elapsed_seconds"], 2.5)
            self.assertEqual(record["usage"]["total_tokens"], 120)
            self.assertEqual(record["estimated_cost_usd"], 0.000037)
            progress = [json.loads(line.removeprefix("BENCHMARK_PROGRESS "))
                        for line in stdout.getvalue().splitlines()]
            self.assertEqual([event["state"] for event in progress], ["started", "completed"])
            self.assertEqual(progress[-1]["instance_id"], "task-1")

    def test_run_agent_force_removes_the_exact_container_after_host_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output"):
                (root / name).mkdir()
            calls = []

            def fake_run(command, **kwargs):
                calls.append((command, kwargs))
                if command[:2] == ["docker", "run"]:
                    raise subprocess.TimeoutExpired(command, kwargs["timeout"])
                return mock.Mock(returncode=0, stdout="")

            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run):
                record = self.worker.run_agent(
                    instance_id="task-1", harness="codex", image="codex:run",
                    repo=root / "repo", harness_bundle=root / "repo",
                    task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium", timeout_seconds=360,
                    network="internal", proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                )

            run_command = calls[0][0]
            container_name = run_command[run_command.index("--name") + 1]
            self.assertEqual(calls[0][1]["timeout"], 360)
            self.assertIn("AGENT_TIMEOUT_SECONDS=315", run_command)
            self.assertEqual(calls[1][0], ["docker", "rm", "--force", container_name])
            self.assertEqual(
                calls[2][0],
                ["docker", "ps", "--all", "--quiet", "--filter",
                 f"name=^/{container_name}$"],
            )
            self.assertEqual(record["status"], "agent-failed")
            self.assertTrue(record["timed_out"])

    def test_run_agent_fails_closed_when_container_absence_cannot_be_verified(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output"):
                (root / name).mkdir()

            def fake_run(command, **kwargs):
                if command[:2] == ["docker", "run"]:
                    raise subprocess.TimeoutExpired(command, kwargs["timeout"])
                if command[:2] == ["docker", "ps"]:
                    return mock.Mock(returncode=1, stdout="")
                return mock.Mock(returncode=1, stdout="")

            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run), \
                    mock.patch.object(self.worker.time, "sleep"), \
                    self.assertRaisesRegex(
                        self.worker.ContainerCleanupError, "could not prove container stopped"
                    ):
                self.worker.run_agent(
                    instance_id="task-1", harness="carry", image="carry:run",
                    repo=root / "repo", harness_bundle=root / "repo",
                    task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium", timeout_seconds=10,
                    network="internal", proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                )

    def test_official_evaluation_removes_model_credentials_and_uses_pinned_dataset(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            predictions = root / "predictions.jsonl"
            predictions.write_text("{}\n")
            canonical = root / "canonical.json"
            canonical.write_text("[]\n")
            captured = {}
            def fake_run(command, **kwargs):
                if command[1] == str(SCRIPT.with_name("swebench_evaluator_compat.py").resolve()):
                    captured.update(command=command, kwargs=kwargs)
                return mock.Mock(returncode=0, stdout="")
            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run):
                self.worker.run_official_evaluation(
                    predictions=predictions, canonical_dataset=canonical,
                    instance_ids=["task-1"], run_id="run-codex", output=root,
                    environment={"PATH": "/bin", "OPENAI_API_KEY": "secret", "OPENAI_MODEL": "model"},
                )
            command = captured["command"]
            self.assertEqual(
                command[1], str(SCRIPT.with_name("swebench_evaluator_compat.py").resolve())
            )
            self.assertIn(str(canonical), command)
            self.assertEqual(command[command.index("--max_workers") + 1], "5")
            self.assertEqual(command[command.index("--cache_level") + 1], "instance")
            self.assertNotIn("--dataset_revision", command)
            self.assertNotIn("OPENAI_API_KEY", captured["kwargs"]["env"])
            self.assertNotIn("OPENAI_MODEL", captured["kwargs"]["env"])
            self.assertEqual(captured["kwargs"]["cwd"], root)
            self.assertFalse(captured["kwargs"]["check"])

            with self.assertRaisesRegex(ValueError, "max_workers must be between 1 and 5"):
                self.worker.run_official_evaluation(
                    predictions=predictions, canonical_dataset=canonical,
                    instance_ids=["task-1"], run_id="run-codex", output=root,
                    max_workers=10,
                )

    def test_official_evaluation_timeout_removes_only_exact_run_containers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            predictions = root / "predictions.jsonl"
            canonical = root / "canonical.json"
            predictions.write_text("{}\n")
            canonical.write_text("[]\n")
            calls = []
            container_id = "a" * 64

            def fake_run(command, **kwargs):
                calls.append((command, kwargs))
                if command[1] == str(SCRIPT.with_name("swebench_evaluator_compat.py").resolve()):
                    raise subprocess.TimeoutExpired(command, kwargs["timeout"])
                if command[:2] == ["docker", "ps"] and "name=" in command[-1]:
                    return mock.Mock(returncode=0, stdout=container_id + "\ninvalid\n")
                return mock.Mock(returncode=0, stdout="")

            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run), \
                    self.assertRaises(subprocess.TimeoutExpired):
                self.worker.run_official_evaluation(
                    predictions=predictions, canonical_dataset=canonical,
                    instance_ids=["task-1"], run_id="exact-run-carry-00", output=root,
                    process_timeout_seconds=360,
                )

            self.assertEqual(
                calls[1][0],
                ["docker", "ps", "--all", "--quiet", "--filter",
                 r"name=\.exact-run-carry-00$"],
            )
            self.assertEqual(calls[2][0], ["docker", "rm", "--force", container_id])
            self.assertEqual(
                calls[3][0],
                ["docker", "ps", "--all", "--quiet", "--filter", f"id={container_id}"],
            )

    def test_official_evaluation_normal_return_removes_residual_exact_run_containers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            predictions = root / "predictions.jsonl"
            canonical = root / "canonical.json"
            predictions.write_text("{}\n")
            canonical.write_text("[]\n")
            calls = []
            container_id = "b" * 64

            def fake_run(command, **kwargs):
                calls.append(command)
                if command[:2] == ["docker", "ps"] and "name=" in command[-1]:
                    return mock.Mock(returncode=0, stdout=container_id + "\n")
                return mock.Mock(returncode=0, stdout="")

            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run):
                self.worker.run_official_evaluation(
                    predictions=predictions, canonical_dataset=canonical,
                    instance_ids=["task-1"], run_id="exact-run-carry-00", output=root,
                    process_timeout_seconds=345,
                )

            self.assertEqual(
                calls[1],
                ["docker", "ps", "--all", "--quiet", "--filter",
                 r"name=\.exact-run-carry-00$"],
            )
            self.assertEqual(calls[2], ["docker", "rm", "--force", container_id])
            self.assertEqual(
                calls[3],
                ["docker", "ps", "--all", "--quiet", "--filter", f"id={container_id}"],
            )

    def test_official_evaluation_timeout_fails_closed_when_cleanup_is_unverifiable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            predictions = root / "predictions.jsonl"
            canonical = root / "canonical.json"
            predictions.write_text("{}\n")
            canonical.write_text("[]\n")

            def fake_run(command, **kwargs):
                if command[1] == str(SCRIPT.with_name("swebench_evaluator_compat.py").resolve()):
                    raise subprocess.TimeoutExpired(command, kwargs["timeout"])
                return mock.Mock(returncode=1, stdout="")

            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run), \
                    self.assertRaisesRegex(
                        self.worker.ContainerCleanupError,
                        "could not enumerate evaluator containers",
                    ):
                self.worker.run_official_evaluation(
                    predictions=predictions, canonical_dataset=canonical,
                    instance_ids=["task-1"], run_id="exact-run-carry-00", output=root,
                    process_timeout_seconds=345,
                )

    def test_materialization_clones_only_the_selected_harness(self):
        selected = [f"task-{number}" for number in range(5)]
        records = [
            {
                "instance_id": instance_id,
                "repo": "owner/repo",
                "base_commit": f"commit-{number}",
                "problem_statement": f"problem {number}",
            }
            for number, instance_id in enumerate(selected)
        ]
        clones = []

        def clone(repo, commit, destination):
            clones.append((repo, commit, destination))
            destination.mkdir(parents=True)

        with tempfile.TemporaryDirectory() as directory:
            self.worker.materialize(
                records=records,
                selected_ids=selected,
                root=pathlib.Path(directory),
                clone=clone,
                harnesses=("carry",),
            )
        self.assertEqual(len(clones), 5)
        self.assertTrue(all(destination.parts[-2:] == ("carry", "repo") for _, _, destination in clones))

    def test_clone_excludes_commits_after_the_task_base(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "source"
            mirror = root / "mirror.git"
            destination = root / "destination"
            source.mkdir()
            subprocess.run(["git", "init", "--quiet", "--initial-branch=main"], cwd=source, check=True)
            subprocess.run(["git", "config", "user.name", "Benchmark Test"], cwd=source, check=True)
            subprocess.run(["git", "config", "user.email", "benchmark@example.invalid"], cwd=source, check=True)
            (source / "value.txt").write_text("base\n")
            subprocess.run(["git", "add", "value.txt"], cwd=source, check=True)
            subprocess.run(["git", "commit", "--quiet", "-m", "base"], cwd=source, check=True)
            base = subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=source, check=True, text=True, capture_output=True,
            ).stdout.strip()
            (source / "value.txt").write_text("future solution\n")
            subprocess.run(["git", "commit", "--quiet", "-am", "future solution"], cwd=source, check=True)
            future = subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=source, check=True, text=True, capture_output=True,
            ).stdout.strip()
            subprocess.run(["git", "tag", "future-release", future], cwd=source, check=True)
            subprocess.run(["git", "clone", "--quiet", "--mirror", str(source), str(mirror)], check=True)

            self.worker._clone("owner/repo", base, destination, mirror)

            head = subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=destination, check=True, text=True, capture_output=True,
            ).stdout.strip()
            self.assertEqual(head, base)
            self.assertNotEqual(
                subprocess.run(
                    ["git", "cat-file", "-e", f"{future}^{{commit}}"], cwd=destination,
                    check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                ).returncode,
                0,
            )
            self.assertEqual(
                subprocess.run(
                    ["git", "remote"], cwd=destination, check=True, text=True, capture_output=True,
                ).stdout,
                "",
            )
            self.assertEqual(
                subprocess.run(
                    ["git", "for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"],
                    cwd=destination, check=True, text=True, capture_output=True,
                ).stdout,
                "",
            )
            self.assertEqual(
                subprocess.run(
                    ["git", "tag"], cwd=destination, check=True, text=True, capture_output=True,
                ).stdout,
                "",
            )
            self.assertEqual(
                subprocess.run(
                    ["git", "reflog"], cwd=destination, check=True, text=True, capture_output=True,
                ).stdout,
                "",
            )
            self.assertFalse((destination / ".git" / "objects" / "info" / "alternates").exists())
            self.assertFalse((destination / ".git" / "info" / "grafts").exists())
            self.assertFalse((destination / ".git" / "shallow").exists())
            self.assertEqual(
                subprocess.run(
                    ["git", "fsck", "--connectivity-only", "--no-reflogs", "--unreachable", "--no-progress"],
                    cwd=destination, check=True, text=True, capture_output=True,
                ).stdout,
                "",
            )

    def test_clone_accepts_reachable_historical_commit_with_invalid_timezone(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            mirror = root / "mirror.git"
            destination = root / "destination"
            subprocess.run(["git", "init", "--quiet", "--bare", str(mirror)], check=True)
            empty_tree = subprocess.run(
                ["git", "--git-dir", str(mirror), "mktree"], input="", check=True,
                text=True, capture_output=True,
            ).stdout.strip()
            commit_text = (
                f"tree {empty_tree}\n"
                "author Historical Author <author@example.invalid> 1 +051800\n"
                "committer Historical Author <author@example.invalid> 1 +051800\n\n"
                "historical malformed timezone\n"
            )
            object_data = f"commit {len(commit_text.encode())}\0{commit_text}".encode()
            commit = hashlib.sha1(object_data).hexdigest()
            object_path = mirror / "objects" / commit[:2] / commit[2:]
            object_path.parent.mkdir()
            object_path.write_bytes(zlib.compress(object_data))
            subprocess.run(
                ["git", "--git-dir", str(mirror), "update-ref", "refs/heads/main", commit], check=True,
            )

            self.worker._clone("owner/repo", commit, destination, mirror)

            self.assertEqual(
                subprocess.run(
                    ["git", "rev-parse", "HEAD"], cwd=destination, check=True,
                    text=True, capture_output=True,
                ).stdout.strip(),
                commit,
            )

    def test_materialization_keeps_gold_data_out_of_agent_inputs_and_clones_per_harness(self):
        selected = [f"task-{number}" for number in range(5)]
        records = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": f"commit-{number}",
             "problem_statement": f"problem {number}", "test_patch": "secret gold test", "patch": "gold patch"}
            for number, instance_id in enumerate(selected)
        ]
        clones = []
        def clone(repo, commit, destination):
            clones.append((repo, commit, destination))
            destination.mkdir(parents=True)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tasks = self.worker.materialize(records=records, selected_ids=selected, root=root, clone=clone)
            self.assertEqual([task["instance_id"] for task in tasks], selected)
            self.assertEqual(len(clones), 15)
            self.assertEqual(len({destination for _, _, destination in clones}), 15)
            for instance_id in selected:
                agent_input = (root / "tasks" / instance_id / "input" / "task.json").read_text()
                self.assertNotIn("test_patch", agent_input)
                self.assertNotIn("gold patch", agent_input)
            canonical = json.loads((root / "canonical-dataset.json").read_text())
            self.assertEqual(canonical, records)

    def test_builds_only_the_selected_harness_image(self):
        calls = []

        def execute(command, **kwargs):
            calls.append(command)
            if command[:2] == ["docker", "image"]:
                return mock.Mock(stdout="sha256:" + "c" * 64 + "\n")
            return mock.Mock(stdout="")

        config = {
            "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CARRY_BASE_IMAGE": "rust@sha256:" + "b" * 64,
            "CODEX_VERSION": "1.2.3",
            "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna",
            "REASONING": "medium",
        }
        provenance = self.worker.build_images(
            source=SCRIPT.parents[1],
            run_id="run1",
            config=config,
            execute=execute,
            harnesses=("carry",),
        )
        builds = [command for command in calls if command[:2] == ["docker", "build"]]
        self.assertEqual(len(builds), 1)
        self.assertEqual(set(provenance), {"carry"})

    def test_exports_each_harness_bundle_once(self):
        calls = []

        def execute(command, **kwargs):
            calls.append(command)
            if command[:2] == ["docker", "cp"]:
                destination = pathlib.Path(command[-1])
                (destination / "bin").mkdir(parents=True, exist_ok=True)
                (destination / "bin" / "adapter").write_text("adapter")
            return mock.Mock(returncode=0, stdout="")

        images = {
            harness: {"image_id": "sha256:" + character * 64}
            for harness, character in zip(self.worker.HARNESSES, "abc")
        }
        with tempfile.TemporaryDirectory() as directory:
            bundles = self.worker.export_harness_bundles(
                images, pathlib.Path(directory), execute=execute,
            )
            self.assertEqual(set(bundles), set(self.worker.HARNESSES))
            self.assertTrue(all(path.is_dir() for path in bundles.values()))
        self.assertEqual(sum(command[:2] == ["docker", "create"] for command in calls), 3)
        self.assertEqual(sum(command[:2] == ["docker", "cp"] for command in calls), 3)
        self.assertEqual(sum(command[:3] == ["docker", "rm", "--force"] for command in calls), 3)

    def test_builds_each_harness_once_and_records_local_image_identity(self):
        calls = []
        def execute(command, **kwargs):
            calls.append(command)
            if command[:2] == ["docker", "image"]:
                return mock.Mock(stdout="sha256:" + command[-1][-1] * 64 + "\n")
            return mock.Mock(stdout="")
        config = {
            "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CARRY_BASE_IMAGE": "rust@sha256:" + "b" * 64,
            "CODEX_VERSION": "1.2.3", "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna", "REASONING": "medium",
            "TASK_IMAGE_REPOSITORY": "registry.example/tasks",
            "TASK_IMAGE_CATALOG": "registry.example/tasks@sha256:" + "f" * 64,
        }
        provenance = self.worker.build_images(
            source=SCRIPT.parents[1], run_id="run1", config=config, execute=execute
        )
        builds = [command for command in calls if command[:2] == ["docker", "build"]]
        self.assertEqual(len(builds), 3)
        self.assertEqual(set(provenance), {"carry", "codex", "pi"})
        self.assertEqual(provenance["pi"]["package_version"], "0.84.2")
        self.assertTrue(all(item["image_id"].startswith("sha256:") for item in provenance.values()))

    def test_resolution_comes_only_from_official_report_ids(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "report.json").write_text(json.dumps({
                "completed_ids": ["task-1", "task-2"],
                "resolved_ids": ["task-1"], "unresolved_ids": ["task-2"],
                "empty_patch_ids": [], "error_ids": [], "incomplete_ids": [],
            }))
            self.assertEqual(self.worker.load_resolved_ids(root), {"task-1"})
            (root / "report.json").write_text(json.dumps({"resolved": 2}))
            with self.assertRaisesRegex(RuntimeError, "outcome ID sets"):
                self.worker.load_resolved_ids(root)


class SklearnReadinessParserTests(unittest.TestCase):
    """Replay public output through SWE-bench 4.1.0, not a stand-in parser."""

    # Verbatim lines from preparation 35553663118, sklearn-25102/test-output.txt.
    # Raw artifact SHA-256: 307847441b4090a47c511994b9a4322cd0bd6d49a3361605a9898595d2000a34.
    # Keep tiny excerpts inline; CI must not depend on the retained local artifact.
    PASSED_NODE = "sklearn/_config.py::sklearn._config.config_context"
    PASSED_LINE = (
        PASSED_NODE + " \x1b[32mPASSED\x1b[0m\x1b[33m                [  0%]\x1b[0m\n"
    )

    SKIPPED_NODE = (
        "sklearn/cluster/tests/test_affinity_propagation.py::test_affinity_propagation[42-float32]"
    )
    SKIPPED_LINE = SKIPPED_NODE + " \x1b[33mSKIPPED\x1b[0m\x1b[33m [  5%]\x1b[0m\n"
    SPACE_ID_LINE = (
        "sklearn/_loss/tests/test_loss.py::test_init_gradient_and_hessian_raises[params0-Valid "
        "options for 'dtype' are .* Got dtype=<class 'numpy.int64'> instead.-HalfSquaredError] "
        "\x1b[32mPASSED\x1b[0m\x1b[33m [  5%]\x1b[0m\n"
    )
    # Preserve the upstream whitespace-tokenization quirk, not an invented pass.
    SPACE_ID_PARSED = {
        "sklearn/_loss/tests/test_loss.py::test_init_gradient_and_hessian_raises[params0-Valid": "options",
    }

    @classmethod
    def setUpClass(cls):
        from importlib.metadata import PackageNotFoundError, version

        try:
            if version("swebench") != "4.1.0":
                raise unittest.SkipTest("requires pinned SWE-bench 4.1.0")
            from swebench.harness.log_parsers import MAP_REPO_TO_PARSER
        except (ImportError, PackageNotFoundError):
            raise unittest.SkipTest("pinned SWE-bench harness unavailable")
        cls.parser = staticmethod(MAP_REPO_TO_PARSER["scikit-learn/scikit-learn"])
        spec = importlib.util.spec_from_file_location("swebench_smoke", SCRIPT)
        cls.worker = importlib.util.module_from_spec(spec)
        assert spec.loader
        spec.loader.exec_module(cls.worker)

    def assert_readiness(self, captured, normalized, expected, *, ready=True,
                         timed_out=False, returncode=124):
        # Only the Docker transport is mocked; parsing, readiness and persistence run.
        parser = mock.Mock(wraps=self.parser)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo = root / "repo"
            repo.mkdir()
            output = root / "evidence"
            with mock.patch.object(self.worker.subprocess, "run", return_value=mock.Mock(
                returncode=returncode, stdout=captured, stderr=""
            )) as run, mock.patch.object(self.worker, "force_remove_container") as cleanup:
                if timed_out:
                    run.side_effect = subprocess.TimeoutExpired(
                        "docker", 180, output=captured.encode("utf-8"), stderr=b"",
                    )
                kwargs = dict(
                    instance_id="scikit-learn__scikit-learn-25102", image="probe", repo=repo,
                    script="pytest -rA -vv --maxfail=1", test_command="pytest -rA -vv --maxfail=1",
                    parser=parser, test_spec=None, output=output, timeout_seconds=180,
                )
                if ready:
                    result = self.worker.run_task_readiness(**kwargs)
                else:
                    with self.assertRaisesRegex(RuntimeError, "PASSED or FAILED required"):
                        self.worker.run_task_readiness(**kwargs)
            if timed_out:
                cleanup.assert_called_once()
            else:
                cleanup.assert_not_called()
            parser.assert_called_once_with(normalized, None)
            self.assertEqual(self.parser(normalized, None), expected)
            self.assertEqual((output / "test-output.txt").read_bytes(), captured.encode("utf-8"))
            metadata = json.loads((output / "metadata.json").read_text())
            self.assertEqual(metadata["status"], "ready" if ready else "not-ready")
            self.assertEqual(metadata["baseline_exit_code"], 124 if timed_out else returncode)
            self.assertEqual(metadata["timed_out_after_tests_started"], timed_out)
            if ready:
                self.assertEqual(metadata, result)
                self.assertEqual(result["parsed_test_count"], len(expected))
                self.assertEqual(result["executed_test_count"], sum(
                    status in ("PASSED", "FAILED") for status in expected.values()
                ))
                self.assertEqual(result["parsed_statuses"], {
                    status: list(expected.values()).count(status) for status in set(expected.values())
                })

    def test_combined_ansi_progress_replay_preserves_raw_evidence(self):
        self.assertEqual(self.parser(self.PASSED_LINE, None), {})
        for timed_out in (False, True):
            with self.subTest(timed_out=timed_out):
                self.assert_readiness(
                    self.PASSED_LINE, self.PASSED_NODE + " PASSED\n",
                    {self.PASSED_NODE: "PASSED"}, timed_out=timed_out,
                )

    def test_ansi_only_retains_existing_parser_result(self):
        captured = self.PASSED_LINE.replace("\x1b[33m                [  0%]\x1b[0m", "")
        expected = {self.PASSED_NODE: "PASSED"}
        # 4.1.0 already handles these simple SGR escapes without a progress field.
        self.assertEqual(self.parser(captured, None), expected)
        self.assert_readiness(captured, self.PASSED_NODE + " PASSED\n", expected)

    def test_progress_only_becomes_parseable(self):
        captured = self.PASSED_LINE.replace("\x1b[32m", "").replace("\x1b[33m", "").replace("\x1b[0m", "")
        self.assertEqual(self.parser(captured, None), {})
        self.assert_readiness(captured, self.PASSED_NODE + " PASSED\n", {self.PASSED_NODE: "PASSED"})

    def test_plain_status_lines_are_unchanged(self):
        # Explicit synthetic status/orientation controls derived from the same node.
        for status in ("PASSED", "FAILED", "SKIPPED", "ERROR", "XFAIL", "UNKNOWN"):
            for status_first in (False, True):
                with self.subTest(status=status, status_first=status_first):
                    captured = (
                        f"{status} {self.PASSED_NODE}\n" if status_first
                        else f"{self.PASSED_NODE} {status}\n"
                    )
                    expected = {} if status == "UNKNOWN" else {self.PASSED_NODE: status}
                    self.assertEqual(self.parser(captured, None), expected)
                    self.assert_readiness(
                        captured, captured, expected, ready=status in ("PASSED", "FAILED"),
                        returncode=1 if status == "FAILED" else 0,
                    )

    def test_decorated_nonexecuted_statuses_remain_not_ready(self):
        self.assert_readiness(
            self.SKIPPED_LINE, self.SKIPPED_NODE + " SKIPPED\n",
            {self.SKIPPED_NODE: "SKIPPED"}, ready=False, returncode=0,
        )
        # These are negative mutations, NOT outcomes claimed for the retained run.
        for status in ("ERROR", "XFAIL", "UNKNOWN"):
            with self.subTest(status=status):
                captured = self.PASSED_LINE.replace("PASSED", status)
                normalized = self.PASSED_NODE + f" {status}\n"
                expected = {self.PASSED_NODE: status}
                if status == "UNKNOWN":
                    # Unknown status is not in the normalization/parser allowlist.
                    normalized = self.PASSED_NODE + " UNKNOWN                [  0%]\n"
                    expected = {}
                self.assert_readiness(captured, normalized, expected, ready=False, returncode=0)

    def test_decorated_failed_baseline_is_ready(self):
        # Synthetic failure control; the retained excerpt actually passed.
        self.assert_readiness(
            self.PASSED_LINE.replace("PASSED", "FAILED"), self.PASSED_NODE + " FAILED\n",
            {self.PASSED_NODE: "FAILED"}, returncode=1,
        )

    def test_whitespace_parameter_unknown_status_is_not_promoted_to_passed(self):
        normalized = self.SPACE_ID_LINE.replace("\x1b[32m", "").replace("\x1b[0m", "")
        normalized = normalized.replace("\x1b[33m [  5%]", "")
        self.assert_readiness(self.SPACE_ID_LINE, normalized, self.SPACE_ID_PARSED, ready=False, returncode=0)

    def test_mixed_results_count_only_executed_statuses(self):
        normalized_space_id = self.SPACE_ID_LINE.replace("\x1b[32m", "").replace("\x1b[0m", "")
        normalized_space_id = normalized_space_id.replace("\x1b[33m [  5%]", "")
        self.assert_readiness(
            self.PASSED_LINE + self.SKIPPED_LINE + self.SPACE_ID_LINE,
            self.PASSED_NODE + " PASSED\n" + self.SKIPPED_NODE + " SKIPPED\n" + normalized_space_id,
            {self.PASSED_NODE: "PASSED", self.SKIPPED_NODE: "SKIPPED", **self.SPACE_ID_PARSED},
        )

    def test_no_completed_test_lines_remain_not_ready(self):
        # Verbatim collection and interrupted final-test lines from the artifact.
        captured = (
            "\x1b[1mcollecting ... \x1b[0mcollected 27814 items / 2 skipped\n"
            "sklearn/cluster/tests/test_k_means.py::test_minibatch_with_many_reassignments "
        )
        normalized = captured.replace("\x1b[1m", "").replace("\x1b[0m", "")
        for timed_out in (False, True):
            with self.subTest(timed_out=timed_out):
                self.assert_readiness(captured, normalized, {}, ready=False, timed_out=timed_out, returncode=0)
        self.assert_readiness("", "", {}, ready=False, returncode=0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
