#!/usr/bin/env python3
"""Run the pinned collector against an actual nested Carry evidence layout."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).parent
PATCH_ROOT = ROOT / "patch_calculate_cost.py"
USAGE_PATCH_ROOT = ROOT / "patch_usage_details.py"
EVALUATOR_SCRIPTS = Path(
    "/mnt/linux-fast/carry-frontier-provision-34762363164/artifacts/"
    "frontierharness-eval/skills/frontierharness-eval/scripts"
)


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CalculateCostIntegrationTests(unittest.TestCase):
    def test_scores_canonical_attempt_not_nested_carry_result(self) -> None:
        calculate_patch = load(PATCH_ROOT, "patch_calculate_cost")
        usage_patch = load(USAGE_PATCH_ROOT, "patch_usage_details")
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for name in ("calculate-cost.py", "cost_accounting.py", "usage_details.py", "pricing.json"):
                shutil.copy2(EVALUATOR_SCRIPTS / name, root / name)
            calculate_patch.apply(root / "calculate-cost.py")
            usage_patch.apply(root / "usage_details.py")
            trial = root / "trial"
            attempt = trial / "jobs" / "job" / "attempt"
            agent = attempt / "agent"
            agent.mkdir(parents=True)
            (agent / "carry-trace.jsonl").write_text(
                json.dumps(
                    {"event": "model_response", "data": {"usage": {"input_tokens": 7, "output_tokens": 3}}}
                )
                + "\n"
            )
            (attempt / "result.json").write_text(
                json.dumps(
                    {
                        "agent_info": {"name": "carry", "model_info": {"name": "accounts/fireworks/models/kimi-k3"}},
                        "agent_result": {"n_input_tokens": 5, "n_cache_tokens": 0, "n_output_tokens": 3},
                        "agent_execution": {"started_at": "2026-01-01T00:00:00Z"},
                        "verifier_result": {"rewards": {"reward": 1}},
                        "started_at": "2026-01-01T00:00:00Z",
                        "finished_at": "2026-01-01T00:00:01Z",
                    }
                )
            )
            nested = agent / "carry"
            nested.mkdir()
            (nested / "result.json").write_text(json.dumps({"completed": True, "usage": {"input_tokens": 7, "output_tokens": 3}}))
            completed = subprocess.run(
                ["python3", str(root / "calculate-cost.py"), "--trial", str(trial), "--harness", "carry", "--score"],
                check=True,
                text=True,
                capture_output=True,
            )
            record = json.loads(completed.stdout)
        self.assertEqual(record["status"], "success")
        self.assertTrue(record["agent_execution_started"])
        self.assertEqual(record["input_tokens"], 5)
        self.assertEqual(record["output_tokens"], 3)
        self.assertEqual(record["turns"], 1)
        self.assertTrue(record["raw_result_path"].endswith("jobs/job/attempt/result.json"))


if __name__ == "__main__":
    unittest.main()
