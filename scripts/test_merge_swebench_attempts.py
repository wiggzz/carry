#!/usr/bin/env python3
"""Behavior tests for merging independent official-50 attempts."""
import json
import pathlib
import subprocess
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("merge_swebench_attempts.py")


class AttemptMergeTests(unittest.TestCase):
    def write_attempt(self, root: pathlib.Path, attempt: int, tasks: list[str],
                      total: int = 3, source_commit: str = "a" * 40) -> None:
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
                "mode": "official-50", "phase": "complete", "source_commit": source_commit,
                "dataset": "SWE-bench/SWE-bench_Verified", "dataset_revision": "frozen",
                "swebench_version": "4.1.0", "model": "test-model", "reasoning": "medium",
                "carry_compaction_policy": "economic", "carry_keep_lease_turns": "0",
                "carry_lease_review_policy": "baseline",
                "carry_compaction_payoff_requests": "1", "carry_compaction_min_payback_percent": "10",
                "carry_compaction_rollout_samples": "16",
                "carry_compaction_rollout_stop_probability_percent": "10",
                "carry_compaction_neutral_high_watermark_tokens": "32768",
                "carry_compaction_neutral_low_watermark_tokens": "24576",
                "pricing_usd_per_million": {"input": 1},
                "images": {
                    "carry": {
                        "tag": f"swebench-attempt-{attempt}-carry",
                        "image_id": f"sha256:{attempt:064x}",
                        "base_resolved_digest": "rust@sha256:test",
                        "dockerfile_sha256": "carry-dockerfile",
                        "package_version": source_commit,
                    },
                    "codex": {
                        "tag": f"swebench-attempt-{attempt}-codex",
                        "image_id": f"sha256:{attempt + 10:064x}",
                        "base_resolved_digest": "node@sha256:test",
                        "dockerfile_sha256": "node-dockerfile",
                        "package_version": "test-codex",
                    },
                    "pi": {
                        "tag": f"swebench-attempt-{attempt}-pi",
                        "image_id": f"sha256:{attempt + 20:064x}",
                        "base_resolved_digest": "node@sha256:test",
                        "dockerfile_sha256": "node-dockerfile",
                        "package_version": "test-pi",
                    },
                    "execution_limits": {"agent_concurrency": 5, "agent_timeout_seconds": 360},
                }, "harnesses": ["carry", "codex", "pi"],
                "attempt": {"number": attempt, "total": total, "independent_fresh_workspaces": True},
            },
        }), encoding="utf-8")

    def test_cli_merges_a_declared_four_attempt_official_study(self):
        tasks = [f"task-{index:02d}" for index in range(50)]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            for attempt in (1, 2, 3, 4):
                self.write_attempt(artifacts, attempt, tasks, total=4)
            manifest = root / "tasks.json"
            manifest.write_text(json.dumps({"instance_ids": tasks}), encoding="utf-8")
            output = root / "out"
            result = subprocess.run(
                ["python3", str(SCRIPT), "--artifacts", str(artifacts), "--manifest", str(manifest),
                 "--harness", "all", "--attempts", "4", "--out", str(output)],
                text=True, capture_output=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((output / "report.json").read_text(encoding="utf-8"))
            self.assertEqual((report["denominator"], report["attempts_per_task_harness"]), (600, 4))
            self.assertEqual(report["harnesses"]["carry"]["denominator"], 200)
            self.assertEqual(report["task_harnesses"]["task-00/carry"]["resolved"], 3)
            self.assertEqual(report["provenance"]["model"], "test-model")
            self.assertEqual(report["provenance"]["source_commit"], "a" * 40)
            self.assertEqual(report["provenance"]["carry_compaction_rollout_samples"], "16")
            self.assertEqual(report["provenance"]["carry_lease_review_policy"], "baseline")
            self.assertEqual(report["provenance"]["carry_compaction_rollout_stop_probability_percent"], "10")
            self.assertEqual(report["provenance"]["carry_compaction_neutral_high_watermark_tokens"], "32768")
            self.assertEqual(report["provenance"]["carry_compaction_neutral_low_watermark_tokens"], "24576")
            self.assertIn("official-50 attempts", (output / "report.md").read_text(encoding="utf-8"))

    def test_new_reports_without_rollout_samples_remain_separate_from_sampled_history(self):
        tasks = [f"task-{index:02d}" for index in range(50)]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            for attempt in (1, 2):
                self.write_attempt(artifacts, attempt, tasks, total=2)
                path = artifacts / f"attempt-{attempt}" / "report.json"
                payload = json.loads(path.read_text(encoding="utf-8"))
                payload["provenance"].pop("carry_compaction_rollout_samples")
                path.write_text(json.dumps(payload), encoding="utf-8")
            manifest = root / "tasks.json"
            manifest.write_text(json.dumps({"instance_ids": tasks}), encoding="utf-8")
            command = ["python3", str(SCRIPT), "--artifacts", str(artifacts),
                       "--manifest", str(manifest), "--harness", "all", "--attempts", "2",
                       "--out", str(root / "out")]
            result = subprocess.run(command, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((root / "out" / "report.json").read_text(encoding="utf-8"))
            self.assertEqual(report["provenance"]["carry_compaction_rollout_samples"], "0")
            path = artifacts / "attempt-2" / "report.json"
            payload = json.loads(path.read_text(encoding="utf-8"))
            payload["provenance"]["carry_compaction_rollout_samples"] = "16"
            path.write_text(json.dumps(payload), encoding="utf-8")
            result = subprocess.run(command, text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("identical immutable provenance", result.stderr)

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
                 "--harness", "all", "--attempts", "3", "--out", str(root / "out")],
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
                 "--harness", "all", "--attempts", "3", "--out", str(root / "out")],
                text=True, capture_output=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("candidate commits", result.stderr)

    def test_cli_rejects_attempts_with_different_immutable_provenance(self):
        tasks = [f"task-{index:02d}" for index in range(50)]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            for attempt in (1, 2):
                self.write_attempt(artifacts, attempt, tasks, total=2)
            second_report = artifacts / "attempt-2" / "report.json"
            payload = json.loads(second_report.read_text(encoding="utf-8"))
            payload["provenance"]["carry_compaction_neutral_high_watermark_tokens"] = "0"
            second_report.write_text(json.dumps(payload), encoding="utf-8")
            manifest = root / "tasks.json"
            manifest.write_text(json.dumps({"instance_ids": tasks}), encoding="utf-8")
            result = subprocess.run(
                ["python3", str(SCRIPT), "--artifacts", str(artifacts), "--manifest", str(manifest),
                 "--harness", "all", "--attempts", "2", "--out", str(root / "out")],
                text=True, capture_output=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("identical immutable provenance", result.stderr)
            payload["provenance"]["carry_compaction_neutral_high_watermark_tokens"] = "32768"
            payload["provenance"]["carry_lease_review_policy"] = "batch-ordinary"
            second_report.write_text(json.dumps(payload), encoding="utf-8")
            result = subprocess.run(
                ["python3", str(SCRIPT), "--artifacts", str(artifacts), "--manifest", str(manifest),
                 "--harness", "all", "--attempts", "2", "--out", str(root / "out")],
                text=True, capture_output=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("identical immutable provenance", result.stderr)

    def test_cli_merges_legacy_default_compaction_provenance(self):
        tasks = [f"task-{index:02d}" for index in range(50)]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            for attempt in (1, 2):
                self.write_attempt(artifacts, attempt, tasks, total=2)
            legacy_report = artifacts / "attempt-1" / "report.json"
            payload = json.loads(legacy_report.read_text(encoding="utf-8"))
            payload["provenance"].pop("carry_compaction_neutral_high_watermark_tokens")
            payload["provenance"].pop("carry_compaction_neutral_low_watermark_tokens")
            payload["provenance"].pop("carry_compaction_min_payback_percent")
            payload["provenance"].pop("carry_lease_review_policy")
            legacy_report.write_text(json.dumps(payload), encoding="utf-8")
            manifest = root / "tasks.json"
            manifest.write_text(json.dumps({"instance_ids": tasks}), encoding="utf-8")
            output = root / "out"
            result = subprocess.run(
                ["python3", str(SCRIPT), "--artifacts", str(artifacts), "--manifest", str(manifest),
                 "--harness", "all", "--attempts", "2", "--out", str(output)],
                text=True, capture_output=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            provenance = json.loads((output / "report.json").read_text(encoding="utf-8"))["provenance"]
            self.assertEqual(provenance["carry_compaction_neutral_high_watermark_tokens"], "32768")
            self.assertEqual(provenance["carry_compaction_neutral_low_watermark_tokens"], "24576")
            self.assertEqual(provenance["carry_compaction_min_payback_percent"], "10")
            self.assertEqual(provenance["carry_lease_review_policy"], "baseline")

    def test_cli_merges_legacy_payback_margin_without_changing_watermarks(self):
        tasks = [f"task-{index:02d}" for index in range(50)]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifacts = root / "artifacts"; artifacts.mkdir()
            for attempt in (1, 2):
                self.write_attempt(artifacts, attempt, tasks, total=2)
            legacy_report = artifacts / "attempt-1" / "report.json"
            payload = json.loads(legacy_report.read_text(encoding="utf-8"))
            payload["provenance"].pop("carry_compaction_min_payback_percent")
            legacy_report.write_text(json.dumps(payload), encoding="utf-8")
            manifest = root / "tasks.json"
            manifest.write_text(json.dumps({"instance_ids": tasks}), encoding="utf-8")
            output = root / "out"
            result = subprocess.run(
                ["python3", str(SCRIPT), "--artifacts", str(artifacts), "--manifest", str(manifest),
                 "--harness", "all", "--attempts", "2", "--out", str(output)],
                text=True, capture_output=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            provenance = json.loads((output / "report.json").read_text(encoding="utf-8"))["provenance"]
            self.assertEqual(provenance["carry_compaction_min_payback_percent"], "10")
            self.assertEqual(provenance["carry_compaction_neutral_high_watermark_tokens"], "32768")
            self.assertEqual(provenance["carry_compaction_neutral_low_watermark_tokens"], "24576")


if __name__ == "__main__":
    unittest.main(verbosity=2)
