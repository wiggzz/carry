"""Model-free command-path fixtures, not benchmark measurements."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
COMMAND = ROOT / 'scripts/benchmark_comparison.py'


def contract(benchmark='swebench'):
    return {
        'schema_version': 1, 'benchmark': benchmark, 'evidence_kind': 'synthetic-fixture',
        'source_commit': '1' * 40, 'runner_commit': '2' * 40,
        'dataset_commit': '3' * 40,
        'selection': [{'id': 'owned-fixture', 'checkpoints': ['final']}],
        'repetitions': 1,
        'settings': {
            'codex_version': '0.147.0', 'codex_sha256': '4' * 64,
            'model': 'gpt-6.1-sol', 'reasoning': 'high', 'auth_mode': 'api-key',
            'native_config': {}, 'prompt_sha256': '5' * 64,
            'environment_digest': 'sha256:' + '6' * 64,
            'harness_digest': 'sha256:' + '7' * 64,
            'gateway_digest': 'sha256:' + '8' * 64,
            'carry_digest': 'sha256:' + '9' * 64,
            'timeout_seconds': 1200, 'retries': 0, 'concurrency': 1,
        },
        'proxy': {},
    }


class ComparisonCommandTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.config = self.root / 'contract.json'
        self.config.write_text(json.dumps(contract()))
        self.env = {**os.environ, 'HOME': str(self.root), 'PYTHONDONTWRITEBYTECODE': '1'}

    def invoke(self, *args):
        return subprocess.run([sys.executable, str(COMMAND), '--contract', str(self.config), *args],
                              env=self.env, text=True, capture_output=True, timeout=10)

    def test_default_is_a_noop_plan_with_direct_disabled_not_forward_only(self):
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)
        self.assertEqual([(a['name'], a['proxy_mode']) for a in plan['arms']],
                         [('direct', 'disabled'), ('compact', 'compact')])
        self.assertFalse(plan['paid_dispatch'])
        self.assertEqual(plan['expected_records'], 2)
        self.assertEqual(len({s['namespace'] for s in plan['slots']}), 2)
        self.assertEqual(list(self.root.iterdir()), [self.config])

    def records(self):
        plan = json.loads(self.invoke().stdout)
        rows = []
        for slot in plan['slots']:
            events = []
            for actor in ('primary', 'shadow') if slot['name'] == 'compact' else ('primary',):
                event = {'actor': actor, 'request_id': slot['namespace'] + actor,
                         'model': 'gpt-6.1-sol', 'service_tier': 'default'}
                events.extend([{**event, 'event': 'started'},
                               {**event, 'event': 'completed', 'latency_ms': 10,
                                'usage': {'input_tokens': 100, 'output_tokens': 10,
                                          'input_tokens_details': {'cached_tokens': 20,
                                                                   'cache_write_tokens': 5}}}])
            rows.append({'contract_sha256': plan['contract_sha256'],
                         'namespace': slot['namespace'], 'checkpoint': 'final',
                         'status': 'evaluated', 'resolved': slot['name'] == 'direct',
                         'agent_seconds': 2 if slot['name'] == 'direct' else 3,
                         'setup_seconds': 1, 'evaluation_seconds': 4,
                         'task_wall_seconds': 7 if slot['name'] == 'direct' else 8,
                         'events': events, 'committed_rewrites': 0,
                         'selected_rewrites': 1 if slot['name'] == 'compact' else 0,
                         'native_compactions': 0, 'quality': {}})
        return rows

    def report(self, rows):
        path = self.root / 'records.jsonl'
        path.write_text(''.join(json.dumps(r) + '\n' for r in rows))
        return self.invoke('--records', str(path))

    def test_report_reconciles_native_primary_and_reviewer_fixed_denominator(self):
        rows = self.records()
        result = self.report(rows)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report.get('evidence_kind'), 'synthetic-fixture')
        direct, compact = report['arms']['direct'], report['arms']['compact']
        self.assertEqual(direct['denominator'], 1)
        self.assertEqual(compact['resolved'], 0)
        self.assertEqual(compact['agent_seconds'], 3)
        self.assertEqual(compact['task_wall_seconds'], 8)
        self.assertEqual(compact['native']['primary']['cache_write_input_tokens'], 5)
        self.assertAlmostEqual(compact['estimated_api_usd'], 2 * direct['estimated_api_usd'])
        self.assertEqual(report['paired']['direct_only_resolved'], 1)
        self.assertEqual(report['paired']['compact_only_resolved'], 0)
        self.assertIn('compact_minus_direct_agent_seconds', report['paired'])
        self.assertEqual(report['paired']['compact_minus_direct_agent_seconds'], 1)
        self.assertAlmostEqual(report['paired']['compact_minus_direct_estimated_api_usd'],
                               direct['estimated_api_usd'])
        self.assertEqual(compact['median_agent_seconds'], 3)
        self.assertEqual(report['timing_basis'], 'summed-checkpoint-intervals-not-cohort-wall')
        self.assertFalse(report['rewrite_efficacy_observed'])
        self.assertEqual(report['cost_basis'], 'modeled-standard-api-usd-not-invoice')

    def test_report_keeps_infra_failure_and_missing_receipt_unavailable_not_zero(self):
        rows = self.records()
        rows[1].update(status='infrastructure-error', resolved=None)
        rows[1]['events'].pop()
        result = self.report(rows)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        arm = report['arms']['compact']
        self.assertEqual(arm['denominator'], 1)
        self.assertEqual(arm['infrastructure_errors'], 1)
        self.assertIsNone(arm['estimated_api_usd'])
        self.assertGreater(arm['observed_api_usd_lower_bound'], 0)
        self.assertFalse(report['comparison_valid'])

    def test_report_rejects_missing_duplicate_and_wrong_source_records(self):
        rows = self.records()
        for records in (rows[:1], rows + [rows[0]],
                        [{**rows[0], 'contract_sha256': '0' * 64}, rows[1]]):
            with self.subTest(records=records):
                self.assertNotEqual(self.report(records).returncode, 0)

    def test_checkpoint_and_arm_namespaces_do_not_flatten_iterative_problems(self):
        value = contract('slopcodebench')
        value['selection'] = [{'id': 'a', 'checkpoints': ['checkpoint_1', 'checkpoint_2']},
                              {'id': 'b', 'checkpoints': ['checkpoint_1']}]
        value['repetitions'] = 2
        self.config.write_text(json.dumps(value))
        result = self.invoke('--include-off')
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)
        self.assertEqual(len(plan['slots']), 12)
        self.assertEqual(len({s['namespace'] for s in plan['slots']}), 12)
        self.assertEqual(plan['expected_records'], 18)
        self.assertEqual(plan['slots'][0]['checkpoints'], ['checkpoint_1', 'checkpoint_2'])
        self.assertEqual(plan['arms'][2], {'name': 'forward-only', 'proxy_mode': 'off'})


if __name__ == '__main__':
    unittest.main()
