"""Model-free deadline accounting against the real worker (no Docker)."""
import json
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

from scripts import swebench_smoke as worker


class DeadlineAccountingTests(unittest.TestCase):
    def run_slot(self, native_usage, retries=0, pending=False, timed_out=True, result_retries=None,
                 response_model='gpt-5.6-luna', service_tier='default', exit_code=0,
                 launch_timeout=False):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ('repo', 'input', 'output'):
                (root / name).mkdir()
            if isinstance(native_usage, dict) and 'input_tokens' in native_usage:
                native_usage = dict(native_usage, input_tokens_details={'cached_tokens': 0})
            usage = dict.fromkeys(worker.USAGE_KEYS, 0)
            usage.update(input_tokens=100, output_tokens=20, total_tokens=120)
            events = [
                {'event': 'model_request', 'data': {'step': 1}},
                {'event': 'model_response', 'data': {'step': 1, 'usage': usage,
                    'response_retries': retries, 'raw': {'usage': native_usage,
                        'model': response_model, 'service_tier': service_tier}}},
            ]
            if pending:
                events.append({'event': 'model_request', 'data': {'step': 2}})
            output = root / 'output'
            (output / 'trace.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
            (output / 'result.json').write_text(json.dumps({'usage': usage,
                'response_retries': retries if result_retries is None else result_retries}))
            (output / 'final.patch').write_text('')
            def launch(command, **kwargs):
                if command[:2] == ['docker', 'run']:
                    if launch_timeout:
                        raise subprocess.TimeoutExpired(command, 1200)
                    if timed_out or exit_code:
                        raise subprocess.CalledProcessError(124 if timed_out else exit_code, command)
                return subprocess.CompletedProcess(command, 0, stdout='')
            with mock.patch.object(worker.subprocess, 'run', side_effect=launch):
                return worker.run_agent(instance_id='task-1', harness='carry', image='carry:test',
                    repo=root/'repo', harness_bundle=root/'repo', task_input=root/'input', output=output,
                    model='gpt-5.6-luna', reasoning='medium', network='internal', proxy_ip='172.28.0.2',
                    api_base='http://openai-proxy:8080/v1', pricing=worker.pricing_for_model('gpt-5.6-luna'))

    def test_retry_lost_usage_is_unknown_even_before_deadline(self):
        for timed_out in (False, True):
            with self.subTest(timed_out=timed_out):
                record = self.run_slot({'input_tokens': 100, 'output_tokens': 20}, retries=1, timed_out=timed_out)
                self.assertIsNone(record['estimated_cost_usd'])
                self.assertEqual(record['usage']['input_tokens'], 100)
                self.assertEqual(record['response_retries'], 1)

    def test_result_retry_evidence_cannot_be_cleared_by_a_clean_trace(self):
        for launch in ({'timed_out': False}, {'timed_out': True},
                       {'timed_out': False, 'exit_code': 1},
                       {'timed_out': False, 'launch_timeout': True}):
            with self.subTest(launch=launch):
                record = self.run_slot({'input_tokens': 100, 'output_tokens': 20},
                                       result_retries=1, **launch)
                self.assertEqual(record['response_retries'], 1)
                self.assertFalse(record['client_usage_complete'])
                self.assertIsNone(record['estimated_cost_usd'])
                self.assertEqual(record['usage']['input_tokens'], 100)
                self.assertEqual(record['usage']['output_tokens'], 20)
                self.assertEqual(record['status'], 'agent-failed' if (
                    launch.get('timed_out') or launch.get('exit_code') or launch.get('launch_timeout')
                ) else 'agent-completed')

    def test_invalid_result_retry_metrics_cannot_certify_complete_cost(self):
        for retries in (True, -1, '1'):
            for launch in ({'timed_out': False}, {'timed_out': True},
                           {'timed_out': False, 'exit_code': 1}):
                with self.subTest(retries=retries, launch=launch):
                    record = self.run_slot({'input_tokens': 100, 'output_tokens': 20},
                                           result_retries=retries, **launch)
                    self.assertEqual(record['status'], 'agent-failed')
                    self.assertFalse(record['client_usage_complete'])
                    self.assertIsNone(record['estimated_cost_usd'])
                    self.assertEqual(record['usage']['input_tokens'], 100)
                    self.assertEqual(record['usage']['output_tokens'], 20)
                    self.assertTrue(record['error'])

    def test_normalized_zero_is_not_native_usage_provenance(self):
        for native in (None, {}, 'invalid', {'input_tokens': True, 'output_tokens': 20},
                       {'input_tokens': -1, 'output_tokens': 20}):
            with self.subTest(native=native):
                self.assertIsNone(self.run_slot(native)['estimated_cost_usd'])

    def test_unanswered_request_after_valid_receipt_is_unknown(self):
        self.assertIsNone(self.run_slot({'input_tokens': 100, 'output_tokens': 20}, pending=True)['estimated_cost_usd'])

    def test_receipt_model_and_tier_must_support_configured_pricing(self):
        for model, tier in ((None, 'default'), ('unknown-model', 'default'),
                            ('gpt-6-sol', 'default'), ('gpt-5.6-luna', 'priority'),
                            ('gpt-5.6-luna', 'flex')):
            with self.subTest(model=model, tier=tier):
                record = self.run_slot({'input_tokens': 100, 'output_tokens': 20},
                                       response_model=model, service_tier=tier)
                self.assertFalse(record['client_usage_complete'])
                self.assertIsNone(record['estimated_cost_usd'])
                self.assertEqual(record['usage']['input_tokens'], 100)

    def test_valid_native_receipt_is_priced(self):
        for launch in ({'timed_out': False}, {'timed_out': True},
                       {'timed_out': False, 'exit_code': 1}):
            with self.subTest(launch=launch):
                record = self.run_slot({'input_tokens': 100, 'output_tokens': 20}, **launch)
                self.assertTrue(record['client_usage_complete'])
                self.assertIsNotNone(record['estimated_cost_usd'])
                self.assertEqual(record['response_retries'], 0)

    def test_aggregate_unknown_keeps_priced_subtotal(self):
        tasks = [{'instance_id': f'task-{n}'} for n in range(5)]
        records = [{'instance_id': t['instance_id'], 'harness': 'carry', 'status': 'evaluated',
                    'estimated_cost_usd': None if n == 4 else 0.1} for n, t in enumerate(tasks)]
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            worker.finalize(tasks=tasks, records=records, output=output, provenance={}, harnesses=('carry',))
            report = json.loads((output/'report.json').read_text())
            summary = report['harnesses']['carry']
            self.assertIsNone(summary['estimated_cost_usd'])
            self.assertFalse(summary['cost_complete'])
            self.assertEqual(summary['costed_slot_subtotal_usd'], 0.4)
            self.assertIn('priced subtotal', (output/'report.md').read_text())
            self.assertIsNone(report['task_harnesses']['task-4/carry']['estimated_cost_usd'])


if __name__ == '__main__':
    unittest.main()
