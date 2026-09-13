#!/usr/bin/env python3
"""Exercise FrontierHarness CI entrypoint argument validation."""

from __future__ import annotations

from pathlib import Path
import subprocess
import tempfile
import unittest

ENTRYPOINT = Path(__file__).parents[2] / "scripts" / "run-frontierharness-ci.sh"


class FrontierHarnessCiEntrypointTests(unittest.TestCase):
    def test_task_subset_is_rejected_for_plan_before_any_evaluator_work(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "source"
            source.mkdir()
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(source), "config", "user.name", "Test"], check=True)
            (source / "file").write_text("fixture\n")
            subprocess.run(["git", "-C", str(source), "add", "file"], check=True)
            subprocess.run(["git", "-C", str(source), "commit", "-qm", "fixture"], check=True)
            commit = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
            tasks = root / "tasks.txt"
            tasks.write_text("terminal-bench/regex-log\n")
            completed = subprocess.run(
                [
                    "bash", str(ENTRYPOINT), "--mode", "plan", "--source", str(source),
                    "--commit", commit, "--repo", "https://example.invalid/carry.git",
                    "--checkpoint", "test-checkpoint", "--run-id", "test-run", "--out", str(root / "out"),
                    "--tasks", str(tasks),
                ],
                text=True,
                capture_output=True,
            )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("--tasks is valid only for smoke-2 and full-30", completed.stderr)

    def test_plan_runs_clean_runtime_installation_contract(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "source"
            tests = source / "benchmarks" / "frontierharness"
            tests.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(source), "config", "user.name", "Test"], check=True)
            for name in (
                "test_adapter.py", "test_agents.py", "test_run_suite.py", "test_normalizer_cwd.py", "test_shards.py",
                "test_transport_retry.py", "test_ci_entrypoint.py", "test_merge_shards.py",
            ):
                (tests / name).write_text("raise SystemExit(0)\n")
            (tests / "test_install.py").write_text("raise SystemExit(37)\n")
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)
            subprocess.run(["git", "-C", str(source), "commit", "-qm", "fixture"], check=True)
            commit = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
            completed = subprocess.run(
                [
                    "bash", str(ENTRYPOINT), "--mode", "plan", "--source", str(source),
                    "--commit", commit, "--repo", "https://example.invalid/carry.git",
                    "--checkpoint", "test-checkpoint", "--run-id", "test-run", "--out", str(root / "out"),
                ],
                text=True,
                capture_output=True,
            )
        self.assertEqual(completed.returncode, 37, completed.stderr)


if __name__ == "__main__":
    unittest.main()
