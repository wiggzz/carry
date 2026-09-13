#!/usr/bin/env python3
"""Execute the collector selection patch against nested Carry evidence."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


PATCHER = Path(__file__).with_name("patch_calculate_cost.py")
spec = importlib.util.spec_from_file_location("patch_calculate_cost", PATCHER)
assert spec and spec.loader
patcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(patcher)


def collector_fixture() -> str:
    return (
        "import argparse\nimport json\nfrom pathlib import Path\n\n"
        "def calculate(trial_dir):\n"
        + patcher.OLD
        + "    if len(leaves) != 1:\n"
        "        return {'status': 'infra_invalid', 'raw_result_path': None}\n"
        "    path = leaves[0]\n"
        "    result = json.loads(path.read_text())\n"
        "    reward = (result.get('verifier_result') or {}).get('rewards', {}).get('reward')\n"
        "    started = bool((result.get('agent_execution') or {}).get('started_at'))\n"
        "    usage = result.get('agent_result') or {}\n"
        "    observed = any(usage.get(k) not in (None, 0) for k in ('n_input_tokens', 'n_output_tokens'))\n"
        "    return {'status': 'success' if reward == 1 and started and observed else 'infra_invalid',\n"
        "            'input_tokens': usage.get('n_input_tokens'),\n"
        "            'output_tokens': usage.get('n_output_tokens'),\n"
        "            'raw_result_path': str(path)}\n\n"
        "if __name__ == '__main__':\n"
        "    parser = argparse.ArgumentParser()\n"
        "    parser.add_argument('--trial', type=Path, required=True)\n"
        "    print(json.dumps(calculate(parser.parse_args().trial)))\n"
    )


class CalculateCostIntegrationTests(unittest.TestCase):
    def test_scores_canonical_attempt_not_nested_carry_result(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            collector = root / "calculate-cost.py"
            collector.write_text(collector_fixture())
            patcher.apply(collector)
            trial = root / "trial"
            attempt = trial / "jobs" / "job" / "attempt"
            agent = attempt / "agent"
            agent.mkdir(parents=True)
            (attempt / "result.json").write_text(
                json.dumps(
                    {
                        "agent_result": {"n_input_tokens": 5, "n_output_tokens": 3},
                        "agent_execution": {"started_at": "2026-01-01T00:00:00Z"},
                        "verifier_result": {"rewards": {"reward": 1}},
                    }
                )
            )
            nested = agent / "carry"
            nested.mkdir()
            (nested / "result.json").write_text(json.dumps({"completed": True}))
            completed = subprocess.run(
                ["python3", str(collector), "--trial", str(trial)],
                check=True,
                text=True,
                capture_output=True,
            )
            record = json.loads(completed.stdout)
        self.assertEqual(record["status"], "success")
        self.assertEqual(record["input_tokens"], 5)
        self.assertEqual(record["output_tokens"], 3)
        self.assertTrue(record["raw_result_path"].endswith("jobs/job/attempt/result.json"))


if __name__ == "__main__":
    unittest.main()
