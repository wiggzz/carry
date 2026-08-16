#!/usr/bin/env python3
"""Behavior tests for the executable protected-worker benchmark."""
import importlib.util
import json
import os
import pathlib
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

    def test_agent_command_mounts_only_workspace_prompt_and_output_and_key_by_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output"):
                (root / name).mkdir()
            command = self.worker.agent_docker_command(
                image="smoke-codex:run", method="codex", repo=root / "repo",
                task_input=root / "input", output=root / "output", model="gpt-5.6-luna",
                reasoning="medium", container_name="carry-agent-codex-test",
                agent_timeout_seconds=315,
            )
            rendered = "\n".join(command)
            self.assertIn("--env\nOPENAI_API_KEY", rendered)
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

    def test_finalize_preserves_failed_slots_and_writes_official_predictions(self):
        tasks = [{"instance_id": f"task-{number}"} for number in range(5)]
        slots = [
            {"instance_id": task["instance_id"], "method": method, "status": "agent-failed", "patch": "", "error": "failed"}
            for task in tasks for method in ("carry", "codex", "pi")
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
            self.assertEqual(report["methods"]["carry"]["resolved"], 1)
            self.assertEqual(report["methods"]["codex"]["statuses"], {"agent-failed": 5})
            summary = (output / "report.md").read_text()
            self.assertIn("Denominator: 15", summary)
            self.assertIn("Completed: 1", summary)
            self.assertIn("Resolved: 1", summary)

    def test_finalize_rejects_missing_duplicate_or_replaced_slots(self):
        tasks = [{"instance_id": f"task-{number}"} for number in range(5)]
        records = [
            {"instance_id": task["instance_id"], "method": method, "status": "failed", "patch": ""}
            for task in tasks for method in ("carry", "codex", "pi")
        ]
        for broken in (records[:-1], records[:-1] + [records[0]], records[:-1] + [{**records[-1], "instance_id": "replacement"}]):
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, "15 unique"):
                self.worker.finalize(tasks=tasks, records=broken, output=pathlib.Path(directory), provenance={})

    def test_official_finalize_requires_exactly_150_slots(self):
        tasks = [{"instance_id": f"task-{number:02d}"} for number in range(50)]
        records = [
            {"instance_id": task["instance_id"], "method": method, "status": "agent-failed", "patch": ""}
            for task in tasks for method in ("carry", "codex", "pi")
        ]
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            self.worker.finalize(tasks=tasks, records=records, output=output, provenance={})
            report = json.loads((output / "report.json").read_text())
            self.assertEqual(report["denominator"], 150)
            self.assertEqual({method: values["denominator"] for method, values in report["methods"].items()}, {
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
        evaluator_worst_case = 15 * (300 + 45)  # Leave global orchestration margin.
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

    def test_official_execution_builds_once_runs_five_agent_shards_and_grades_150_slots(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        dataset = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
             "problem_statement": f"problem {instance_id}", "test_patch": "gold"}
            for instance_id in frozen
        ]
        fake_datasets = types.SimpleNamespace(load_dataset=lambda *args, **kwargs: dataset)
        materialized_shards = []
        evaluation_shards = []

        def fake_materialize(*, records, selected_ids, root, clone):
            materialized_shards.append(list(selected_ids))
            tasks = []
            for instance_id in selected_ids:
                task = {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
                        "problem_statement": f"problem {instance_id}"}
                tasks.append(task)
                for method in ("carry", "codex", "pi"):
                    (root / "tasks" / instance_id / method / "repo").mkdir(parents=True)
                (root / "tasks" / instance_id / "input").mkdir(parents=True)
            return tasks

        def fake_agent(**kwargs):
            return {"instance_id": kwargs["instance_id"], "method": kwargs["method"],
                    "status": "agent-completed", "patch": "", "error": None,
                    "attempts": 1, "retries": 0,
                    "response_retries": 2 if kwargs["method"] == "carry" else 0}

        def fake_evaluation(*, instance_ids, output, **kwargs):
            evaluation_shards.append(list(instance_ids))
            output.mkdir(parents=True, exist_ok=True)
            (output / "report.json").write_text(json.dumps({
                "completed_ids": list(instance_ids), "resolved_ids": [],
                "unresolved_ids": list(instance_ids), "empty_patch_ids": [],
                "error_ids": [], "incomplete_ids": [],
            }))

        config = {
            "BENCHMARK_MODE": "official-50", "RUN_ID": "official-test",
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
                        method: {"tag": f"image:{method}"} for method in ("carry", "codex", "pi")
                    }) as build, \
                    mock.patch.object(self.worker, "run_agent", side_effect=fake_agent), \
                    mock.patch.object(self.worker, "run_official_evaluation", side_effect=fake_evaluation):
                with mock.patch.dict(os.environ, {
                    "OPENAI_API_KEY": "not-a-real-key", "OPENAI_SECRET_FILE": str(secret),
                }, clear=False):
                    self.worker.execute_benchmark(
                        source=source, work=work, output=output, config=config
                    )
                    self.assertNotIn("OPENAI_API_KEY", os.environ)
                    self.assertNotIn("OPENAI_SECRET_FILE", os.environ)

            self.assertEqual(build.call_count, 1)
            self.assertEqual([len(shard) for shard in materialized_shards], [10] * 5)
            self.assertEqual(len(evaluation_shards), 15)
            self.assertTrue(all(len(shard) == 10 for shard in evaluation_shards))
            self.assertFalse(secret.exists())
            self.assertFalse((work / "agent-shards").exists())
            report = json.loads((output / "report.json").read_text())
            self.assertEqual((report["denominator"], report["completed"]), (150, 150))
            self.assertEqual(report["methods"]["carry"]["response_retries"], 100)

    def test_official_build_failure_still_preserves_all_150_planned_slots(self):
        frozen = [f"task-{number:02d}" for number in range(50)]
        dataset = [
            {"instance_id": instance_id, "repo": "owner/repo", "base_commit": "a" * 40,
             "problem_statement": f"problem {instance_id}"}
            for instance_id in frozen
        ]
        config = {
            "BENCHMARK_MODE": "official-50", "RUN_ID": "official-test",
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
            self.assertEqual(len(records), 150)
            self.assertEqual({record["status"] for record in records}, {"not-run"})
            self.assertEqual(report["provenance"]["phase"], "planned")

    def test_run_agent_failure_is_a_record_with_empty_patch_and_no_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("repo", "input", "output"):
                (root / name).mkdir()
            with mock.patch.object(self.worker.subprocess, "run", side_effect=RuntimeError("agent stopped")) as run:
                record = self.worker.run_agent(
                    instance_id="task-1", method="codex", image="codex:run",
                    repo=root / "repo", task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium",
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
                    json.dumps({"response_retries": 4}), encoding="utf-8"
                )

            with mock.patch.object(self.worker.subprocess, "run", side_effect=fake_run):
                record = self.worker.run_agent(
                    instance_id="task-1", method="carry", image="carry:run",
                    repo=root / "repo", task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium",
                )

            self.assertEqual(record["status"], "agent-completed")
            self.assertEqual(record["retries"], 0)
            self.assertEqual(record["response_retries"], 4)

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
                    instance_id="task-1", method="codex", image="codex:run",
                    repo=root / "repo", task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium", timeout_seconds=360,
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
                    instance_id="task-1", method="carry", image="carry:run",
                    repo=root / "repo", task_input=root / "input", output=root / "output",
                    model="gpt-5.6-luna", reasoning="medium", timeout_seconds=10,
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
            self.assertNotIn("--dataset_revision", command)
            self.assertNotIn("OPENAI_API_KEY", captured["kwargs"]["env"])
            self.assertNotIn("OPENAI_MODEL", captured["kwargs"]["env"])
            self.assertEqual(captured["kwargs"]["cwd"], root)
            self.assertFalse(captured["kwargs"]["check"])

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

    def test_materialization_keeps_gold_data_out_of_agent_inputs_and_clones_per_method(self):
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

    def test_builds_each_method_once_and_records_local_image_identity(self):
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
