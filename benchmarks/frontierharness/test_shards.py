#!/usr/bin/env python3
"""Behavior tests for the hosted FrontierHarness task sharder."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import cast
import unittest

SHARDER = Path(__file__).parents[2] / "scripts" / "frontierharness_shards.py"


class FrontierHarnessShardTests(unittest.TestCase):
    def run_sharder(self, task_text: str, batch_size: int) -> list[dict[str, object]]:
        with tempfile.TemporaryDirectory() as raw:
            tasks = Path(raw) / "tasks.txt"
            tasks.write_text(task_text)
            completed = subprocess.run(
                [sys.executable, str(SHARDER), "--tasks", str(tasks), "--batch-size", str(batch_size)],
                check=True,
                text=True,
                capture_output=True,
            )
        return json.loads(completed.stdout)["include"]

    def test_shards_cover_each_task_once_in_manifest_order(self) -> None:
        shards = self.run_sharder(
            "terminal-bench/one\ndatacurve/two\nterminal-bench/three\n",
            batch_size=2,
        )
        self.assertEqual(
            shards,
            [
                {"index": 0, "tasks": ["terminal-bench/one", "datacurve/two"]},
                {"index": 1, "tasks": ["terminal-bench/three"]},
            ],
        )

    def test_one_task_batches_exercise_the_same_hosted_shard_path(self) -> None:
        shards = self.run_sharder(
            "terminal-bench/regex-log\ndatacurve/expr-try-catch-errors\n",
            batch_size=1,
        )
        self.assertEqual(len(shards), 2)
        self.assertEqual(
            [task for shard in shards for task in cast(list[str], shard["tasks"])], [
            "terminal-bench/regex-log",
            "datacurve/expr-try-catch-errors",
        ])


if __name__ == "__main__":
    unittest.main()
