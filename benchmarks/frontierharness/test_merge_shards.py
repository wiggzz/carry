#!/usr/bin/env python3
"""Behavior tests for merging independent hosted FrontierHarness shard evidence."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

MERGER = Path(__file__).parents[2] / "scripts" / "merge_frontierharness_shards.py"


class FrontierHarnessShardMergeTests(unittest.TestCase):
    def write_shard(self, root: Path, run_id: str, task_id: str) -> None:
        run = root / run_id
        trial = run / "trials" / task_id.replace("/", "-")
        trial.mkdir(parents=True)
        (run / "run.json").write_text(json.dumps({
            "run_id": run_id, "checkpoint": "checkpoint", "harness": "carry",
            "model": "accounts/fireworks/models/kimi-k3", "provider": "fireworks",
            "timeout_seconds": 5400, "egress_policy": {"allow": ["api.fireworks.ai"]},
        }))
        (trial / "trial.json").write_text(json.dumps({"id": task_id, "status": "success", "success": True}))

    def invoke(self, expected: str, make_shards) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            expected_path = root / "expected.txt"
            expected_path.write_text(expected)
            shards = root / "shards"
            shards.mkdir()
            make_shards(shards)
            return subprocess.run(
                [sys.executable, str(MERGER), "--expected", str(expected_path), "--shards", str(shards), "--out", str(root / "merged")],
                text=True,
                capture_output=True,
            )

    def test_merges_one_valid_trial_for_every_expected_task(self) -> None:
        completed = self.invoke(
            "terminal-bench/one\ndatacurve/two\n",
            lambda root: (self.write_shard(root, "shard-0", "terminal-bench/one"), self.write_shard(root, "shard-1", "datacurve/two")),
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        summary = json.loads(completed.stdout)
        self.assertEqual(summary["task_count"], 2)
        self.assertEqual(summary["run"]["run_id"], "combined")

    def test_rejects_duplicate_task_evidence(self) -> None:
        completed = self.invoke(
            "terminal-bench/one\n",
            lambda root: (self.write_shard(root, "shard-0", "terminal-bench/one"), self.write_shard(root, "shard-1", "terminal-bench/one")),
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("duplicate trial evidence", completed.stderr)


if __name__ == "__main__":
    unittest.main()
