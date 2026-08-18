#!/usr/bin/env python3
"""Behavior tests for the fail-closed live SWE-bench runner contract."""
import importlib.util
import json
import pathlib
import os
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("swebench_live_runner.py")


class LiveRunnerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("swebench_live_runner", SCRIPT)
        cls.runner = importlib.util.module_from_spec(spec)
        assert spec.loader
        spec.loader.exec_module(cls.runner)

    def test_manifest_has_the_frozen_150_slot_denominator(self):
        manifest = self.runner.build_manifest(run_id="review-1")
        self.assertEqual(manifest["schema"], "carry.swe-bench-live-plan.v2")
        slots = manifest["slots"]
        self.assertEqual(len(slots), 150)
        self.assertEqual(
            {(slot["instance_id"], slot["harness"]) for slot in slots},
            {
                (instance_id, harness)
                for instance_id in self.runner.benchmark.load_selection()
                for harness in self.runner.benchmark.HARNESSES
            },
        )
        self.assertEqual([slot["ordinal"] for slot in slots], list(range(150)))

    def test_smoke_manifest_is_first_five_frozen_tasks_and_exactly_fifteen_slots(self):
        manifest = self.runner.build_manifest(run_id="smoke-1", preset="smoke-5")
        selected = self.runner.benchmark.load_selection()
        self.assertEqual(manifest["instance_ids"], selected[:5])
        self.assertEqual(manifest["task_count"], 5)
        self.assertEqual(manifest["record_count"], 15)
        self.assertEqual(
            [(slot["instance_id"], slot["harness"]) for slot in manifest["slots"]],
            [(instance_id, harness) for instance_id in selected[:5] for harness in ("carry", "codex", "pi")],
        )

    def test_smoke_manifest_rejects_unknown_preset(self):
        with self.assertRaisesRegex(ValueError, "preset"):
            self.runner.build_manifest(run_id="smoke-2", preset="quick")

    def test_boundaries_separate_agents_from_evaluation_and_expose_only_task_and_output(self):
        boundaries = self.runner.build_boundaries()
        agents = boundaries["agents"]
        evaluator = boundaries["evaluator"]
        self.assertNotEqual(agents["boundary_id"], evaluator["boundary_id"])
        self.assertEqual(agents["kind"], "external-container")
        self.assertEqual(evaluator["kind"], "external-container")
        self.assertEqual(
            {mount["target"] for mount in agents["mounts"]},
            {"/benchmark/task", "/benchmark/output"},
        )
        self.assertEqual(
            {mount["target"] for mount in evaluator["mounts"]},
            {"/benchmark/task", "/benchmark/output"},
        )
        self.assertEqual(agents["environment"], ["OPENAI_API_KEY"])
        self.assertEqual(evaluator["environment"], [])

    def test_boundary_validation_rejects_host_and_credential_exposure(self):
        boundaries = self.runner.build_boundaries()
        unsafe_mounts = ["/home/runner", "/var/run/docker.sock", "/runner/_work/_temp"]
        for source in unsafe_mounts:
            bad = json.loads(json.dumps(boundaries))
            bad["agents"]["mounts"].append({"source": source, "target": "/leak", "mode": "ro"})
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "forbidden host path"):
                self.runner.validate_boundaries(bad)

        for name in ["AWS_ACCESS_KEY_ID", "OPENAI_API_KEY", "OPENAI_MODEL", "STAGING_TOKEN"]:
            bad = json.loads(json.dumps(boundaries))
            bad["evaluator"]["environment"].append(name)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "evaluator environment"):
                self.runner.validate_boundaries(bad)

    def test_boundary_validation_rejects_shared_or_in_process_boundaries(self):
        for mutation in ("shared", "in-process"):
            boundaries = self.runner.build_boundaries()
            if mutation == "shared":
                boundaries["evaluator"]["boundary_id"] = boundaries["agents"]["boundary_id"]
            else:
                boundaries["agents"]["kind"] = "process"
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.runner.validate_boundaries(boundaries)

    def test_cli_writes_reviewable_manifest_without_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "manifest.json"
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "plan", "--run-id", "review-2", "--output", str(output)],
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(manifest["run_id"], "review-2")
            self.assertEqual(manifest["task_count"], 50)
            self.assertEqual(manifest["record_count"], 150)

    def test_worker_invocation_uses_pinned_images_and_forwards_secret_by_name_only(self):
        instance_id = self.runner.benchmark.load_selection()[0]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            task = root / "task"
            output = root / "output"
            task.mkdir()
            output.mkdir()
            (task / "task.json").write_text(json.dumps({"instance_id": instance_id}), encoding="utf-8")
            argv_log = root / "docker.argv"
            docker = root / "docker"
            docker.write_text(
                """#!/bin/sh
if [ -n "${OPENAI_API_KEY:-}" ]; then
  printf '%s\n' KEY_PRESENT >> "$FAKE_DOCKER_ARGV"
else
  printf '%s\n' KEY_ABSENT >> "$FAKE_DOCKER_ARGV"
fi
previous=
for argument do
  printf '%s\n' "$argument" >> "$FAKE_DOCKER_ARGV"
  if [ "$previous" = --mount ]; then
    case "$argument" in
      *,dst=/benchmark/output)
        source=${argument#type=bind,src=}
        source=${source%,dst=/benchmark/output}
        case "$source" in */agent) : > "$source/final.patch" ;; esac
        ;;
    esac
  fi
  previous=$argument
done
""",
                encoding="utf-8",
            )
            docker.chmod(0o755)
            secret = "test-openai-secret-never-log"
            agent_image = "ghcr.io/example/agent@sha256:" + "a" * 64
            evaluator_image = "ghcr.io/example/evaluator@sha256:" + "b" * 64
            env = {
                "PATH": os.environ["PATH"],
                "OPENAI_API_KEY": secret,
                "FAKE_DOCKER_ARGV": str(argv_log),
            }
            result = subprocess.run(
                [
                    sys.executable, str(SCRIPT), "invoke", "--run-id", "review-3",
                    "--instance-id", instance_id, "--harness", "carry",
                    "--task-dir", str(task), "--output-dir", str(output),
                    "--agent-image", agent_image, "--evaluator-image", evaluator_image,
                    "--docker-command", str(docker),
                ],
                text=True, capture_output=True, check=False, env=env,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            argv = argv_log.read_text(encoding="utf-8")
            self.assertEqual(argv.count("OPENAI_API_KEY"), 1)
            self.assertEqual(argv.count("KEY_PRESENT"), 1)
            self.assertEqual(argv.count("KEY_ABSENT"), 1)
            self.assertNotIn(secret, argv + result.stdout + result.stderr)
            self.assertIn(agent_image, argv)
            self.assertIn(evaluator_image, argv)
            self.assertNotIn("/var/run/docker.sock", argv)
            self.assertNotIn(str(pathlib.Path.cwd()), argv)
            self.assertEqual(argv.count(f"type=bind,src={task.resolve()},dst=/benchmark/task,readonly"), 1)
            self.assertIn(f"type=bind,src={output.resolve() / 'evaluator-task'},dst=/benchmark/task,readonly", argv)
            self.assertIn(f"type=bind,src={output.resolve() / 'agent'},dst=/benchmark/output", argv)
            self.assertIn(f"type=bind,src={output.resolve() / 'evaluator'},dst=/benchmark/output", argv)

            metadata = json.loads((output / "run-metadata.json").read_text(encoding="utf-8"))
            self.assertEqual(metadata["schema"], "carry.swe-bench-live-invocation.v2")
            self.assertEqual(metadata["instance_id"], instance_id)
            self.assertEqual(metadata["harness"], "carry")
            self.assertEqual(metadata["images"], {"agent": agent_image, "evaluator": evaluator_image})
            self.assertNotIn(secret, json.dumps(metadata))

    def test_invocation_rejects_nonselected_task_mutable_image_and_missing_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            task = root / "task"
            output = root / "output"
            task.mkdir()
            output.mkdir()
            (task / "task.json").write_text(json.dumps({"instance_id": "not-selected"}), encoding="utf-8")
            base = [
                sys.executable, str(SCRIPT), "invoke", "--run-id", "review-4",
                "--instance-id", "not-selected", "--harness", "carry",
                "--task-dir", str(task), "--output-dir", str(output),
                "--agent-image", "agent:latest",
                "--evaluator-image", "evaluator:latest",
                "--docker-command", "/does/not/matter",
            ]
            result = subprocess.run(base, text=True, capture_output=True, check=False, env={"OPENAI_API_KEY": "x"})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("selected-50", result.stderr)

            selected = self.runner.benchmark.load_selection()[0]
            (task / "task.json").write_text(json.dumps({"instance_id": selected}), encoding="utf-8")
            mutable = [selected if item == "not-selected" else item for item in base]
            result = subprocess.run(mutable, text=True, capture_output=True, check=False, env={"OPENAI_API_KEY": "x"})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("digest", result.stderr)

            pinned = [
                ("image@sha256:" + "c" * 64) if item in {"agent:latest", "evaluator:latest"} else item
                for item in mutable
            ]
            result = subprocess.run(pinned, text=True, capture_output=True, check=False, env={})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("OPENAI_API_KEY", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
