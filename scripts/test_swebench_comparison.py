"""Synthetic executable SWE paired boundary; no provider, Docker or dataset download."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import swebench_comparison as lane
from scripts import benchmark_comparison as comparison
from scripts.test_benchmark_comparison import contract


def events():
    identity = {'actor': 'primary', 'request_id': 'synthetic-request', 'model': 'gpt-6.1-sol', 'service_tier': 'default'}
    return [{**identity, 'event': 'started'}, {**identity, 'event': 'completed', 'latency_ms': 10,
            'usage': {'input_tokens': 100, 'output_tokens': 10, 'input_tokens_details': {'cached_tokens': 20}}}]


class PairedSWEExecutionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.config = contract()
        self.config.update(benchmark='swebench', selection=[{'id': 'owned__synthetic-1', 'checkpoints': ['final']}])
        self.records = [{'instance_id': 'owned__synthetic-1', 'repo': 'owned/synthetic',
                         'base_commit': 'a'*40, 'problem_statement': 'Owned synthetic task',
                         'patch': 'PRIVATE GOLD', 'test_patch': 'PRIVATE TEST'}]
        dataset = self.root/'dataset.json'; dataset.write_text(json.dumps(self.records))
        self.prepared = {'canonical_dataset': str(dataset), 'source_root': str(Path(__file__).resolve().parents[1]),
                         'config': {}, 'harness_bundle': str(self.root/'bundle'), 'gateway_image': 'sha256:'+'1'*64,
                         'carry_image': 'sha256:'+'2'*64, 'evaluation_timeout_seconds': 30,
                         'tasks': {'owned__synthetic-1': {'agent_image': {'image_id': 'sha256:'+'3'*64}}}}

    def execute(self, *, evaluator_failure=False, receipt_missing=False):
        launches = []
        clones = []
        original_materialize = lane.swe.materialize
        def clone(repo, commit, destination):
            clones.append(str(destination)); destination.mkdir(parents=True)
        def materialize(**kwargs):
            return original_materialize(**kwargs, clone=clone)
        def agent(**kwargs):
            launches.append((os.environ['CARRY_PROXY_MODE'], os.environ['BENCHMARK_TRUSTED_DIRECT'], kwargs))
            public = json.loads((kwargs['task_input']/'task.json').read_text())
            self.assertNotIn('patch', public); self.assertNotIn('test_patch', public)
            if not receipt_missing:
                state = kwargs['output'].with_name('agent-proxy-state'); state.mkdir()
                state.joinpath('benchmark-events.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events()))
            return {'instance_id': kwargs['instance_id'], 'harness': 'codex', 'status': 'agent-completed',
                    'patch': 'SYNTHETIC GENERATED PATCH', 'agent_seconds': 0.01, 'setup_seconds': 0.01}
        def evaluate(**kwargs):
            self.assertFalse(any(k.startswith('OPENAI_') for k in kwargs['environment']))
            prediction = json.loads(kwargs['predictions'].read_text())
            self.assertEqual(prediction['model_patch'], 'SYNTHETIC GENERATED PATCH')
            self.assertEqual(kwargs['max_workers'], 1)
            self.assertIsNotNone(kwargs['process_timeout_seconds'])
            if evaluator_failure:
                raise RuntimeError('synthetic evaluator lost')
            task = kwargs['instance_ids'][0]
            kwargs['output'].joinpath('report.json').write_text(json.dumps({
                'completed_ids': [task], 'resolved_ids': [task], 'unresolved_ids': [],
                'empty_patch_ids': [], 'error_ids': [], 'incomplete_ids': []}))
        config = self.root/'contract.json'; config.write_text(json.dumps(self.config))
        prepared = self.root/'prepared.json'; prepared.write_text(json.dumps(self.prepared))
        with patch.object(lane, 'preflight', side_effect=lambda c,p: comparison.build_plan(c)), \
             patch.object(lane.swe, 'materialize', side_effect=materialize), \
             patch.object(lane.swe, 'run_isolated_agent', side_effect=agent), \
             patch.object(lane.swe, 'run_official_evaluation', side_effect=evaluate), \
             patch.dict(os.environ, {'OPENAI_API_KEY': 'synthetic-secret', 'OPENAI_SECRET_FILE': '/synthetic/private'}), \
             contextlib.redirect_stdout(io.StringIO()):
            lane.main(['--contract', str(config), '--prepared', str(prepared), '--run', '--allow-paid',
                       '--budget-usd', '1', '--output', str(self.root/'paired')])
        report = json.loads((self.root/'paired/report.json').read_text())
        return report, launches, clones

    def test_production_command_dispatches_both_arms_and_grades_patches(self):
        report, launches, clones = self.execute()
        self.assertEqual([(mode, direct) for mode, direct, _ in launches], [('disabled', '1'), ('compact', '0')])
        self.assertEqual(len(set(clones)), 2)
        self.assertEqual(report['paired']['both_resolved'], 1)
        self.assertTrue(report['comparison_valid'])
        self.assertTrue(report['cost_comparison_complete'])
        self.assertEqual(report['evidence_kind'], 'synthetic-fixture')
        self.assertEqual(launches[0][2]['model'], launches[1][2]['model'])
        self.assertNotEqual(launches[0][2]['output'], launches[1][2]['output'])

    def test_evaluator_loss_stops_further_dispatch_with_fixed_denominator(self):
        report, launches, _ = self.execute(evaluator_failure=True)
        self.assertEqual(len(launches), 1)
        self.assertEqual(report['expected_records'], 2)
        self.assertFalse(report['comparison_valid'])
        self.assertEqual(report['arms']['compact']['terminal_statuses'], {'not-run': 1})
        self.assertIsNone(report['arms']['compact']['estimated_api_usd'])

    def test_missing_native_receipts_are_unknown_and_stop_pair(self):
        report, launches, _ = self.execute(receipt_missing=True)
        self.assertEqual(len(launches), 1)
        self.assertFalse(report['cost_comparison_complete'])
        self.assertIsNone(report['arms']['direct']['estimated_api_usd'])

    def test_missing_consent_rejects_before_preflight(self):
        config = self.root/'contract.json'; config.write_text(json.dumps(self.config))
        with patch.object(lane, 'preflight') as check, contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                lane.main(['--contract', str(config), '--run'])
            check.assert_not_called()


if __name__ == '__main__':
    unittest.main()
