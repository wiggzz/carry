"""Execute the manual workflow's actual credential-free shell gates.

These are local shell/process tests, not GitHub Actions or Docker qualification.
"""
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml
from scripts import slopcodebench as scb
from scripts.test_benchmark_comparison import contract
from scripts.test_swebench_comparison import events

SOURCE = Path(__file__).resolve().parents[1]
WORKFLOW = SOURCE/'.github/workflows/run-slopcodebench.yml'


class WorkflowShellTests(unittest.TestCase):
    def steps(self):
        return yaml.safe_load(WORKFLOW.read_text())['jobs']['official-scb']['steps']

    def shell(self, name, env):
        step = next(step for step in self.steps() if step.get('name') == name)
        return subprocess.run(['/bin/bash', '-euo', 'pipefail', '-c', step['run']],
            cwd=SOURCE, env={**os.environ, 'SCB_PYTHON': sys.executable, **env},
            capture_output=True, text=True, timeout=10)

    def test_budget_gate_rejects_nonfinite_unsafe_and_out_of_scope_values(self):
        for value, accepted in [('10', True), ('0.1', True), ('20', True), ('21', False),
                                ('0', False), ('-1', False), ('nan', False), ('inf', False),
                                ('10\nINJECT=1', False), ('$(exit 0)', False)]:
            with self.subTest(value=value):
                result = self.shell('Validate requested stop threshold (no secrets)', {'BUDGET_USD': value})
                self.assertEqual(result.returncode == 0, accepted, result.stderr)

    def test_terminal_gate_accepts_only_complete_priced_fixed_denominator(self):
        config = contract('slopcodebench')
        config.update(runner_commit='af5a9cbc311605b5cceb80200ca9832bdff2d7b1',
                      dataset_commit='9cd9ca3a51c3d3e2a99d2488a25baf73a2204451')
        config['selection'] = [{'id': 'cfgpipe', 'checkpoints': ['checkpoint_'+str(i) for i in range(1, 7)]}]
        run_plan = scb.plan(config)
        rows = [{'namespace': slot['namespace'], 'contract_sha256': run_plan['contract_sha256'],
                'checkpoint': cp, 'task_id': slot['task_id'], 'arm': slot['name'], 'status': 'evaluated',
                'resolved': False, 'quality': {},
                'events': [{**event, 'request_id': slot['name']+'-'+cp} for event in events()]}
                for slot in run_plan['slots'] for i, cp in enumerate(slot['checkpoints'])]
        variants = {'valid-unresolved': copy.deepcopy(rows), 'missing': copy.deepcopy(rows[:-1]),
                    'duplicate': copy.deepcopy(rows[:-1]+[rows[0]]), 'infra': copy.deepcopy(rows),
                    'unknown-price': copy.deepcopy(rows)}
        variants['infra'][0].update(status='infrastructure-error', resolved=None)
        variants['unknown-price'][0]['events'] = [{'ledger_capture_incomplete': True}]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); (root/'paired').mkdir()
            (root/'paired/plan.json').write_text(json.dumps(run_plan))
            for name, records in variants.items():
                with self.subTest(name=name):
                    (root/'paired/records.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in records))
                    result = self.shell('Reject incomplete or unpriced comparison (no secrets)', {'SCB_OUTPUT': str(root)})
                    self.assertEqual(result.returncode == 0, name == 'valid-unresolved', result.stderr)


if __name__ == '__main__':
    unittest.main()
