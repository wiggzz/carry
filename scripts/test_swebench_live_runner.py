#!/usr/bin/env python3
"""Behavior tests for the fail-closed live SWE-bench runner contract."""
import importlib.util
import json
import pathlib
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
        slots = manifest["slots"]
        self.assertEqual(len(slots), 150)
        self.assertEqual(
            {(slot["instance_id"], slot["method"]) for slot in slots},
            {
                (instance_id, method)
                for instance_id in self.runner.benchmark.load_selection()
                for method in self.runner.benchmark.METHODS
            },
        )
        self.assertEqual([slot["ordinal"] for slot in slots], list(range(150)))

    def test_boundaries_separate_agents_from_evaluation_and_expose_only_named_inputs(self):
        boundaries = self.runner.build_boundaries()
        agents = boundaries["agents"]
        evaluator = boundaries["evaluator"]
        self.assertNotEqual(agents["boundary_id"], evaluator["boundary_id"])
        self.assertEqual(agents["kind"], "external-container")
        self.assertEqual(evaluator["kind"], "external-container")
        self.assertEqual(
            {mount["target"] for mount in agents["mounts"]},
            {"/benchmark/repo", "/benchmark/task.json", "/benchmark/output"},
        )
        self.assertEqual(
            {mount["target"] for mount in evaluator["mounts"]},
            {"/benchmark/repo", "/benchmark/task.json", "/benchmark/patch.diff", "/benchmark/output"},
        )
        self.assertEqual(evaluator["environment"], [])

    def test_boundary_validation_rejects_host_and_credential_exposure(self):
        boundaries = self.runner.build_boundaries()
        unsafe_mounts = ["/home/runner", "/var/run/docker.sock", "/runner/_work/_temp"]
        for source in unsafe_mounts:
            bad = json.loads(json.dumps(boundaries))
            bad["agents"]["mounts"].append({"source": source, "target": "/leak", "mode": "ro"})
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "forbidden host path"):
                self.runner.validate_boundaries(bad)

        for name in ["AWS_ACCESS_KEY_ID", "OPENAI_API_KEY", "STAGING_TOKEN"]:
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

    def test_live_gate_fails_closed_before_accepting_any_model_credential(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "authorize-live"],
            text=True,
            capture_output=True,
            check=False,
            env={},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("credential broker", result.stderr.lower())


if __name__ == "__main__":
    unittest.main(verbosity=2)
