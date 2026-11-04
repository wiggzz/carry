"""Real pinned SCB runner/evaluator and Docker isolation; owned fake native/provider.

Run ONLY in disposable hosted CI. All reported scores/usage are synthetic fixtures.
"""
import copy
import json
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

from scripts import slopcodebench_fixture as fixture
from scripts import slopcodebench_runtime as runtime
from scripts import benchmark_comparison as comparison


@unittest.skipUnless(os.environ.get('CARRY_SCB_HOSTED_FIXTURE') == '1', 'disposable hosted Docker fixture only')
class OfficialProtectedRuntimeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(os.environ['SCB_FIXTURE_OUTPUT'])
        cls.contract, cls.prepared, cls.provider = fixture.prepare(
            cls.root, Path(os.environ['SCB_UPSTREAM_ROOT']), Path(os.environ['SCB_CARRY_BINARY']))

    def test_actual_official_gold_and_paired_native_checkpoint_lifecycle(self):
        contract, prepared = self.contract, self.prepared
        runtime.preflight(contract, prepared, verify_sources=False, require_readiness=False)
        readiness = runtime.prepare_readiness(contract, prepared)
        self.assertEqual(readiness['model_requests'], 0)
        run_plan = runtime.preflight(contract, prepared, verify_sources=False)
        with fixture.FixtureNetwork(self.provider) as network:
            report = comparison.dispatch(run_plan, contract, prepared, self.root/'paired', runtime.run_slot, budget_usd=1.0)
        (self.root/'provider-synthetic.log').write_text(network.provider_log)
        rows = [json.loads(line) for line in (self.root/'paired'/'records.jsonl').read_text().splitlines()]
        self.assertEqual(len(rows), run_plan['expected_records'])
        self.assertTrue(report['comparison_valid'], json.dumps(rows, indent=2))
        self.assertTrue(report['cost_comparison_complete'], json.dumps(report, indent=2))
        self.assertEqual(report['evidence_kind'], 'synthetic-fixture')
        self.assertEqual(report['paired']['both_resolved'], 2)
        self.assertEqual(report['performance_qualifier'], 'rewrite-unproven')
        sessions = []
        for slot in run_plan['slots']:
            directory = self.root/'paired'/slot['namespace']
            official = directory/'official'
            final_snapshot = next((official/'checkpoint_2').rglob('synthetic-lifecycle.jsonl'))
            lifecycle = [json.loads(line) for line in final_snapshot.read_text().splitlines()]
            self.assertEqual([entry['checkpoint'] for entry in lifecycle], [1, 1, 2])
            self.assertEqual([entry['resume'] for entry in lifecycle], [False, True, False])
            self.assertEqual(lifecycle[0]['conversation'], lifecycle[1]['conversation'])
            self.assertNotEqual(lifecycle[1]['conversation'], lifecycle[2]['conversation'])
            self.assertEqual(len({entry['pid'] for entry in lifecycle}), 3)
            self.assertEqual(len({entry['home'] for entry in lifecycle}), 1)
            sessions.append(lifecycle[0]['conversation'])
            arm = report['arms'][slot['name']]
            # Initial failed Codex invocation + official retry + next checkpoint.
            self.assertEqual(arm['native']['primary']['requests'], 3)
            self.assertEqual(arm['native']['primary']['completed_requests'], 3)
            self.assertGreater(arm['estimated_api_usd'], 0)
            self.assertTrue(all(row['quality'] for row in rows if row['arm'] == slot['name']))
            gateway_log = (directory/'trusted-network'/'gateway-events.log').read_text()
            boundaries = [json.loads(line.split(' ', 1)[1])['session_id'] for line in gateway_log.splitlines()
                          if line.startswith('BENCHMARK_CONVERSATION ')]
            self.assertEqual(len(set(boundaries)), 2 if slot['name'] == 'compact' else 0)
        self.assertNotEqual(*sessions)
        leftovers = subprocess.check_output(['docker', 'ps', '-aq', '--filter', 'name=carry-scb-'], text=True)
        self.assertEqual(leftovers.strip(), '')
        networks = subprocess.check_output(['docker', 'network', 'ls', '--format', '{{.Name}}'], text=True).splitlines()
        self.assertFalse(any(name.startswith('carry-agent-') for name in networks))

    def test_preflight_missing_or_changed_prepared_inputs_fails_without_model_network(self):
        config = copy.deepcopy(self.contract)
        prepared = copy.deepcopy(self.prepared)
        prepared['agent_image'] = 'mutable:latest'
        with self.assertRaisesRegex(ValueError, 'immutable'):
            runtime.preflight(config, prepared, verify_sources=False)
        config['settings']['codex_sha256'] = '0'*64
        with self.assertRaisesRegex(ValueError, 'native Codex binary'):
            runtime.preflight(config, self.prepared, verify_sources=False)
        prepared = copy.deepcopy(self.prepared)
        prepared['tasks']['synthetic_evolution']['definition_sha256'] = '0'*64
        with self.assertRaisesRegex(ValueError, 'problem definition'):
            runtime.preflight(self.contract, prepared, verify_sources=False)
        with patch.object(runtime, 'verify_checkout', side_effect=ValueError('wrong source')):
            with self.assertRaisesRegex(ValueError, 'wrong source'):
                runtime.preflight(self.contract, self.prepared)


if __name__ == '__main__':
    unittest.main()
