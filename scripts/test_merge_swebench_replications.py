#!/usr/bin/env python3
"""Behavior tests for merging independently executed SWE-bench replications."""
import json
import pathlib
import subprocess
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("merge_swebench_replications.py")


class ReplicationMergeTests(unittest.TestCase):
    def write_attempt(self, root: pathlib.Path, attempt: int, tasks: list[str],
                      source_commit: str = "a" * 40) -> None:
        artifact = root / f"attempt-{attempt}"
        artifact.mkdir()
        records = [
            {
                "instance_id": task, "harness": harness, "attempt": attempt,
                "status": "evaluated", "patch": "", "resolved": attempt != 2,
                "estimated_cost_usd": 0.01 * attempt,
            }
            for task in tasks for harness in ("carry", "codex", "pi")
        ]
        (artifact / "records.json").write_text(json.dumps(records), encoding="utf-8")
        (artifact / "report.json").write_text(json.dumps({
            "denominator": 150,
            "attempt_numbers": [attempt],
            "provenance": {
                "mode": "replicated-50", "phase": "complete", "source_commit": source_commit,
                "replication": {"attempt": attempt, "attempts_per_task_harness": 3},
            },
        }), encoding="utf-8")

    def test_cli_merges_exactly_three_attempt_artifacts_into_one_replication_study(self):
        tasks = [f"task-{index:02d}" for index in range(50)]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            for attempt in (1, 2, 3):
                self.write_attempt(artifacts, attempt, tasks)
            manifest = root / "tasks.json"
            manifest.write_text(json.dumps({"instance_ids": tasks}), encoding="utf-8")
            output = root / "out"
            result = subprocess.run(
                ["python3", str(SCRIPT), "--artifacts", str(artifacts), "--manifest", str(manifest),
                 "--harness", "all", "--out", str(output)],
                text=True, capture_output=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((output / "report.json").read_text(encoding="utf-8"))
            self.assertEqual((report["denominator"], report["attempts_per_task_harness"]), (450, 3))
            self.assertEqual(report["harnesses"]["carry"]["denominator"], 150)
            self.assertEqual(report["task_harnesses"]["task-00/carry"]["resolved"], 2)
            self.assertIn("replication study", (output / "report.md").read_text(encoding="utf-8"))

    def test_cli_rejects_missing_or_duplicate_attempt_artifacts(self):
        tasks = [f"task-{index:02d}" for index in range(50)]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            self.write_attempt(artifacts, 1, tasks)
            self.write_attempt(artifacts, 2, tasks)
            manifest = root / "tasks.json"
            manifest.write_text(json.dumps({"instance_ids": tasks}), encoding="utf-8")
            result = subprocess.run(
                ["python3", str(SCRIPT), "--artifacts", str(artifacts), "--manifest", str(manifest),
                 "--harness", "all", "--out", str(root / "out")],
                text=True, capture_output=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("attempt artifacts", result.stderr)
    def test_cli_rejects_attempts_from_different_candidate_commits(self):
        tasks = [f"task-{index:02d}" for index in range(50)]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            self.write_attempt(artifacts, 1, tasks)
            self.write_attempt(artifacts, 2, tasks, source_commit="b" * 40)
            self.write_attempt(artifacts, 3, tasks)
            manifest = root / "tasks.json"
            manifest.write_text(json.dumps({"instance_ids": tasks}), encoding="utf-8")
            result = subprocess.run(
                ["python3", str(SCRIPT), "--artifacts", str(artifacts), "--manifest", str(manifest),
                 "--harness", "all", "--out", str(root / "out")],
                text=True, capture_output=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("candidate commits", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
