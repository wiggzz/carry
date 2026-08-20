#!/usr/bin/env python3
"""Behavior tests for the executable protected-worker benchmark."""
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("swebench_smoke.py")


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
        }
        config = self.worker.validate_config(valid)
        self.assertEqual(config["PI_VERSION"], "0.84.2")
        for key, value in (("BASE_IMAGE", "node:22"), ("CODEX_VERSION", "latest")):
            bad = dict(valid)
            bad[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.worker.validate_config(bad)

    def test_selected_harness_defaults_to_carry_and_rejects_unknown(self):
        self.assertEqual(self.worker.selected_harnesses({}), ("carry",))
        for harness in ("carry", "codex", "pi"):
            with self.subTest(harness=harness):
                self.assertEqual(
                    self.worker.selected_harnesses({"BENCHMARK_HARNESS": harness}),
                    (harness,),
                )
        with self.assertRaisesRegex(ValueError, "BENCHMARK_HARNESS"):
            self.worker.selected_harnesses({"BENCHMARK_HARNESS": "all"})

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

    def test_agent_command_mounts_only_workspace_prompt_and_output_and_key_by_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output"):
                (root / name).mkdir()
            command = self.worker.agent_docker_command(
                image="smoke-codex:run", harness="codex", repo=root / "repo",
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
            self.assertNotIn(str(pathlib.Path.home()), rendered)
            self.assertEqual(rendered.count("type=bind"), 3)
            self.assertIn("dst=/workspace", rendered)
            self.assertIn("dst=/benchmark/input,readonly", rendered)
            self.assertIn("dst=/benchmark/output", rendered)
            self.assertIn("HOME=/agent-home", rendered)
            self.assertIn("AGENT_TIMEOUT_SECONDS=315", rendered)
            self.assertIn("carry-agent-codex-test", rendered)
            self.assertIn("/agent-home:rw", rendered)
            self.assertIn("/tmp:rw", rendered)

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
                ["python3", str(entrypoint), "run", "--model", "gpt-test", "--reasoning", "medium",
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

        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(
            ValueError, "5 unique"
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

    def test_model_pricing_accounts_for_reads_writes_and_output(self):
        pricing = self.worker.pricing_for_model("gpt-5.6-luna")
        usage = {"input_tokens": 1_000_000, "cached_input_tokens": 400_000,
                 "cache_write_input_tokens": 200_000, "output_tokens": 100_000,
                 "reasoning_tokens": 5, "total_tokens": 1_100_000}
        self.assertEqual(self.worker.estimate_cost_usd(usage, pricing), 0.258)
        self.assertIsNone(self.worker.pricing_for_model("unknown-model"))

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
        self.assertEqual(self.worker.selection_for_mode(frozen, "smoke-5"), frozen[:5])
        self.assertEqual(self.worker.selection_for_mode(frozen, "official-50"), frozen)
        shards = self.worker.ordered_shards(frozen, 10)
        self.assertEqual([len(shard) for shard in shards], [10] * 5)
        self.assertEqual([item for shard in shards for item in shard], frozen)
        with self.assertRaises(ValueError):
            self.worker.selection_for_mode(frozen[:-1], "official-50")

    def test_official_phase_budgets_fit_the_five_hour_worker_envelope(self):
        limits = self.worker.official_phase_limits()
        self.assertEqual(
            limits["agent_seconds"] + limits["evaluation_seconds"] + limits["setup_reserve_seconds"],
            limits["worker_seconds"],
        )
        evaluator_worst_case = 10 * (300 + 45)  # Ten five-task evaluator shards.
        self.assertLess(evaluator_worst_case, limits["evaluation_seconds"])
        self.assertEqual(150 * 360 // 3, limits["worker_seconds"])
        self.assertLess(limits["agent_seconds"], 150 * 360 // 3)

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
        ]
        with self.assertRaisesRegex(
                RuntimeError, "official evaluation incomplete for 3 slots.*error.*failed.*missing"):
            self.worker.require_complete_official_evaluations(records)

        self.worker.require_complete_official_evaluations(records[:2])

    def test_official_execution_uses_five_task_evaluator_shards(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        dataset = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
             "problem_statement": f"problem {instance_id}", "test_patch": "gold"}
            for instance_id in frozen
        ]
        fake_datasets = types.SimpleNamespace(load_dataset=lambda *args, **kwargs: dataset)
        materialized_shards = []
        evaluation_shards = []

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

        config = {
            "BENCHMARK_MODE": "official-50", "BENCHMARK_HARNESS": "carry",
            "RUN_ID": "official-test",
            "BASE_IMAGE": "node@sha256:" + "a" * 64,
            "CARRY_BASE_IMAGE": "rust@sha256:" + "b" * 64,
            "CODEX_VERSION": "1.2.3", "PI_VERSION": "0.84.2",
            "MODEL": "gpt-5.6-luna", "REASONING": "medium",
            "AGENT_CONCURRENCY": "3",
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
                        "carry": {"tag": "image:carry"}
                    }) as build, \
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
            require_complete.assert_called_once()
            self.assertEqual(len(require_complete.call_args.args[0]), 50)
            self.assertEqual([len(shard) for shard in materialized_shards], [10] * 5)
            self.assertEqual(len(evaluation_shards), 10)
            self.assertTrue(all(len(shard) == 5 for shard in evaluation_shards))
            self.assertFalse(secret.exists())
            self.assertFalse((work / "agent-shards").exists())
            report = json.loads((output / "report.json").read_text())
            self.assertEqual((report["denominator"], report["completed"]), (50, 50))
            self.assertEqual(set(report["harnesses"]), {"carry"})
            self.assertEqual(report["harnesses"]["carry"]["response_retries"], 100)
            limits = report["provenance"]["images"]["execution_limits"]
            self.assertEqual(limits["agent_shard_size"], 10)
            self.assertEqual(limits["evaluator_shard_size"], 5)
            self.assertEqual(limits["evaluator_concurrency"], 5)

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
                    repo=root / "repo", task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium",
                    network="internal", proxy_ip="172.28.0.2", api_base="http://openai-proxy:8080/v1",
                )
            self.assertEqual(run.call_count, 1)
            self.assertEqual(record["status"], "agent-failed")
            self.assertEqual(record["patch"], "")
            self.assertIn("agent stopped", record["error"])
            self.assertEqual((record["attempts"], record["retries"]), (1, 0))
            self.assertEqual(record["response_retries"], 0)

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
                    repo=root / "repo", task_input=root / "input", output=root / "output",
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
                    repo=root / "repo", task_input=root / "input", output=root / "output",
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
                    repo=root / "repo", task_input=root / "input", output=root / "output",
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
                if "swebench.harness.run_evaluation" in command:
                    captured.update(command=command, kwargs=kwargs)
                return mock.Mock(returncode=0, stdout="")
            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run):
                self.worker.run_official_evaluation(
                    predictions=predictions, canonical_dataset=canonical,
                    instance_ids=["task-1"], run_id="run-codex", output=root,
                    environment={"PATH": "/bin", "OPENAI_API_KEY": "secret", "OPENAI_MODEL": "model"},
                )
            command = captured["command"]
            self.assertIn("swebench.harness.run_evaluation", command)
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
                if "swebench.harness.run_evaluation" in command:
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
                if "swebench.harness.run_evaluation" in command:
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
                    ["git", "fsck", "--no-reflogs", "--unreachable", "--no-progress"],
                    cwd=destination, check=True, text=True, capture_output=True,
                ).stdout,
                "",
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


if __name__ == "__main__":
    unittest.main(verbosity=2)
