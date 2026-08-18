#!/usr/bin/env python3
"""Behavior tests for deduplicated benchmark progress rendering."""
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("benchmark_progress.py")


class BenchmarkProgressTests(unittest.TestCase):
    def test_renders_only_new_valid_progress_events_across_polls(self):
        started = "BENCHMARK_PROGRESS " + json.dumps({
            "instance_id": "task-1", "harness": "carry", "state": "started",
        }, sort_keys=True)
        completed = "BENCHMARK_PROGRESS " + json.dumps({
            "elapsed_seconds": 2.5, "instance_id": "task-1", "harness": "carry",
            "state": "completed", "status": "agent-completed",
        }, sort_keys=True)
        with tempfile.TemporaryDirectory() as directory:
            state = pathlib.Path(directory) / "seen.json"
            first = subprocess.run(
                [sys.executable, str(SCRIPT), "--state", str(state)],
                input="boot noise\n" + started + "\n", text=True,
                capture_output=True, check=True,
            )
            second = subprocess.run(
                [sys.executable, str(SCRIPT), "--state", str(state)],
                input=started + "\nmalformed BENCHMARK_PROGRESS nope\n" + completed + "\n",
                text=True, capture_output=True, check=True,
            )
        self.assertEqual(first.stdout, "[carry] task-1 started\n")
        self.assertEqual(second.stdout, "[carry] task-1 completed: agent-completed in 2.500s\n")


if __name__ == "__main__":
    unittest.main()
