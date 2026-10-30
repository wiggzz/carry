"""Identical-history receipt parity against the real CLI; loopback/stdlb only.

CARRY_TEST_BINARY=/owned/carry python3 -B tests/proxy_noop_parity.py -v
The Cargo integration selector supplies the freshly built executable. Synthetic
receipts are independent from local tokenization; no provider keys are read.
"""
import copy
import http.client
import json
import os
from pathlib import Path
import tempfile
import unittest

import proxy_state_regressions as support


class NoOpParity(unittest.TestCase):
    def record(self, value):
        if directory := os.environ.get('CARRY_TEST_EVIDENCE_DIR'):
            output = Path(directory)
            output.mkdir(parents=True, exist_ok=True)
            (output / (self._testMethodName + '.json')).write_text(json.dumps(value, indent=2) + '\n')

    def stats(self, proxy):
        status, stats = proxy.request('GET', '/carry/dashboard/stats')
        self.assertEqual(status, 200)
        self.assertEqual(len(stats['sessions']), 1)
        return stats['sessions'][0]

    def lifecycle(self, *, mode, options, write, cached_pattern, sse=False):
        observations = [(62719 + index * 100, cached) for index, cached in enumerate(cached_pattern)]
        snapshots = []
        expected = 0.0
        history = [{'role': 'user', 'content': 'unchanged synthetic native receipt matrix'}]

        def primary(body, n):
            if options is None:
                self.assertNotIn('prompt_cache_options', body)
            else:
                self.assertEqual(body['prompt_cache_options'], options)
            incoming, cached = observations[n - 1]
            details = {'cached_tokens': cached}
            if write != 'missing':
                details['cache_write_tokens'] = incoming - cached if write == 'paid' else 0
            result = support.completed(response_id=f'parity-{n}')
            result['usage'] = {'input_tokens': incoming, 'output_tokens': 100,
                               'input_tokens_details': details}
            if sse:
                self.assertIs(body['stream'], True)
                return 200, (b'event: response.completed\ndata: ' + support.wire(
                    {'type': 'response.completed', 'response': result}) + b'\n\ndata: [DONE]\n\n')
            return 200, result

        def review(*_):
            raise AssertionError('review threshold must suppress the reviewer in the primary parity matrix')

        with tempfile.TemporaryDirectory(prefix='carry-noop-parity-') as directory:
            fixture = support.Fixture(primary, review)
            try:
                for index, (incoming, cached) in enumerate(observations):
                    if index:
                        history.append({'role': 'user', 'content': f'append {index}'})
                    proxy = support.Proxy(Path(directory), fixture, mode=mode,
                                          extra_env={'CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS': '1000000'})
                    try:
                        fields = {} if options is None else {'prompt_cache_options': options}
                        if sse:
                            connection = http.client.HTTPConnection(*proxy.address, timeout=30)
                            try:
                                connection.request('POST', '/v1/responses', body=support.wire({
                                    'model': 'gpt-6.1-sol', 'input': history, 'stream': True, **fields}),
                                    headers={'Content-Type': 'application/json',
                                             'x-carry-session': 'state-regression'})
                                response = connection.getresponse()
                                raw = response.read()
                                self.assertEqual(response.status, 200)
                                self.assertIn(b'"response.completed"', raw)
                                self.assertIn(f'parity-{index + 1}'.encode(), raw)
                            finally:
                                connection.close()
                        else:
                            self.assertEqual(proxy.send(history, model='gpt-6.1-sol', **fields)[0], 200)
                        row = self.stats(proxy)
                        estimate = row['cost_estimate']
                        saved = proxy.saved()
                        written = incoming - cached if write == 'paid' else 0
                        expected += ((incoming - cached - written) * 2 + cached * .1
                                     + written * 2.5 + 100 * 10) / 1e6
                        self.assertEqual(fixture.primaries[-1]['input'], history)
                        self.assertEqual(saved['compactions'], 0)
                        self.assertEqual(estimate.get('initial_compactions', 0), 0)
                        self.assertEqual(estimate['traditional_compactions'], 0)
                        self.assertEqual(estimate['summary_cost'], 0)
                        self.assertEqual(estimate['reviewer_cost'], 0)
                        self.assertEqual(estimate['excluded_unpriced_calls'], 0)
                        self.assertEqual(estimate['coverage']['unmodeled_calls'], 0)
                        self.assertAlmostEqual(row['primary']['cost_usd'], expected, places=12)
                        self.assertAlmostEqual(estimate['observed_primary_cost'], expected, places=12)
                        self.assertAlmostEqual(estimate['carry_cost'], expected, places=12)
                        snapshots.append({'estimate': copy.deepcopy(estimate), 'expected_primary': expected})
                    finally:
                        proxy.close()
                self.assertEqual(fixture.reviews, [])
            finally:
                fixture.close()
                self.record({'synthetic_receipts': True, 'mode': mode, 'options': options, 'write': write,
                             'sse': sse, 'snapshots': snapshots, 'requests': fixture.primaries,
                             'reviews': fixture.reviews, 'fixture_errors': fixture.errors})
                self.assertEqual(fixture.errors, [])
        # Complete all three observed-money/wire controls before checking parity,
        # so a cold failure cannot conceal the warm/miss/restart failures.
        for index, snapshot in enumerate(snapshots):
            with self.subTest(primary=index + 1):
                self.assertAlmostEqual(snapshot['estimate']['traditional_cost'], snapshot['expected_primary'],
                                       places=12, msg='NO_OP_RECEIPT_PARITY')
                self.assertAlmostEqual(snapshot['estimate']['savings'], 0, places=12)

    def test_reviewer_only_expense_is_additional_after_restart(self):
        def primary(_body, n):
            result = support.completed(response_id=f'review-parity-{n}')
            result['usage'] = {'input_tokens': 62719, 'output_tokens': 100,
                               'input_tokens_details': {'cached_tokens': 0}}
            return 200, result

        def review(body, _n):
            result = support.advice_response({'protected': support.ledger(body)['eligible_group_ids'],
                                              'removable': [], 'memories': []})
            result['usage'] = {'input_tokens': 1000, 'output_tokens': 100,
                               'input_tokens_details': {'cached_tokens': 0}}
            return 200, result

        with tempfile.TemporaryDirectory(prefix='carry-noop-review-') as directory:
            fixture = support.Fixture(primary, review)
            try:
                proxy = support.Proxy(Path(directory), fixture)
                try:
                    history = [{'role': 'user', 'content': 'preserve task'},
                               {'role': 'assistant', 'content': 'unaltered evidence'},
                               {'role': 'user', 'content': 'continue'}]
                    for _ in range(2):
                        self.assertEqual(proxy.send(history, model='gpt-6.1-sol')[0], 200)
                        self.assertEqual(fixture.primaries[-1]['input'], history)
                    row = self.stats(proxy)
                    saved = proxy.saved()
                    self.assertEqual(saved['compactions'], 0)
                    self.assertGreater(len(fixture.reviews), 0)
                    estimate = row['cost_estimate']
                    expected = 2 * (62719 * 2 + 100 * 10) / 1e6
                    self.assertAlmostEqual(estimate['observed_primary_cost'], expected, places=12)
                    self.assertGreater(estimate['observed_reviewer_cost'], 0)
                    self.assertAlmostEqual(estimate['observed_reviewer_cost'], row['shadow']['cost_usd'], places=12)
                    self.assertAlmostEqual(estimate['carry_cost'], expected + row['shadow']['cost_usd'], places=12)
                finally:
                    proxy.close()
                proxy = support.Proxy(Path(directory), fixture)
                try:
                    self.assertEqual(self.stats(proxy)['cost_estimate'], estimate)
                finally:
                    proxy.close()
                self.record({'estimate': estimate, 'expected_primary': expected, 'saved': saved,
                             'requests': fixture.primaries, 'reviews': fixture.reviews})
                self.assertEqual(estimate['traditional_compactions'], 0)
                self.assertEqual(estimate.get('initial_compactions', 0), 0)
                self.assertAlmostEqual(estimate['traditional_cost'], expected, places=12,
                                       msg='NO_OP_REVIEWER_OVERHEAD')
                self.assertAlmostEqual(estimate['savings'], -row['shadow']['cost_usd'], places=12)
            finally:
                fixture.close()
                self.assertEqual(fixture.errors, [])

    def failure_retry(self, failure):
        def primary(_body, n):
            if n == 2:
                return None if failure == 'transport' else (429, {'error': 'synthetic failure'})
            result = support.completed(response_id=f'retry-parity-{n}')
            result['usage'] = {'input_tokens': 62719 if n == 1 else 62819, 'output_tokens': 100,
                               'input_tokens_details': {'cached_tokens': 0}}
            return 200, result

        with tempfile.TemporaryDirectory(prefix='carry-noop-retry-') as directory:
            fixture = support.Fixture(primary, lambda *_: self.fail('no review'))
            history = [{'role': 'user', 'content': 'retry only committed history'}]
            try:
                proxy = support.Proxy(Path(directory), fixture, mode='off')
                try:
                    self.assertEqual(proxy.send(history, model='gpt-6.1-sol')[0], 200)
                    before = self.stats(proxy)['cost_estimate']
                    history.append({'role': 'user', 'content': 'retry this append'})
                    self.assertEqual(proxy.send(history, model='gpt-6.1-sol')[0],
                                     502 if failure == 'transport' else 429)
                    failed = self.stats(proxy)['cost_estimate']
                    for key in ('observed_primary_cost', 'carry_cost', 'traditional_cost', 'summary_cost'):
                        self.assertEqual(failed[key], before[key])
                    self.assertEqual(proxy.saved()['completed_requests'], 1)
                    self.assertEqual(proxy.saved()['failed_primaries'], 1)
                finally:
                    proxy.close()
                proxy = support.Proxy(Path(directory), fixture, mode='off')
                try:
                    self.assertEqual(proxy.send(history, model='gpt-6.1-sol')[0], 200)
                    estimate = self.stats(proxy)['cost_estimate']
                    self.assertEqual(proxy.saved()['completed_requests'], 2)
                    self.assertEqual(fixture.primaries[1]['input'], history)
                    self.assertEqual(fixture.primaries[2]['input'], history)
                    expected = ((62719 + 62819) * 2 + 200 * 10) / 1e6
                    self.assertAlmostEqual(estimate['observed_primary_cost'], expected, places=12)
                    self.assertAlmostEqual(estimate['carry_cost'], expected, places=12)
                    self.assertEqual(estimate['traditional_compactions'], 0)
                    self.assertEqual(estimate.get('initial_compactions', 0), 0)
                    self.record({'before': before, 'failed': failed, 'estimate': estimate,
                                 'requests': fixture.primaries, 'expected_primary': expected})
                    self.assertAlmostEqual(estimate['traditional_cost'], expected, places=12,
                                           msg='NO_OP_RETRY_PARITY')
                finally:
                    proxy.close()
            finally:
                fixture.close()
                self.assertEqual(fixture.errors, [])

    def test_http_failure_retry_preserves_primary_parity(self):
        self.failure_retry('http')

    def test_transport_failure_retry_preserves_primary_parity(self):
        self.failure_retry('transport')

    def unknown_receipt(self, kind):
        def primary(_body, _n):
            result = support.completed()
            details = {} if kind == 'missing' else {'cached_tokens': 10, 'cache_write_tokens': 2000}
            result['usage'] = {'input_tokens': 2000, 'output_tokens': 0,
                               'input_tokens_details': details}
            if kind == 'tariff':
                result['usage']['input_tokens_details'] = {'cached_tokens': 10}
            return 200, result

        with tempfile.TemporaryDirectory(prefix='carry-noop-unknown-') as directory:
            fixture = support.Fixture(primary, lambda *_: self.fail('no review'))
            try:
                proxy = support.Proxy(Path(directory), fixture, mode='off')
                try:
                    model = 'unknown-parity-fixture' if kind == 'tariff' else 'gpt-6.1-sol'
                    self.assertEqual(proxy.send([{'role': 'user', 'content': 'unknown is not zero'}], model=model)[0], 200)
                    row = self.stats(proxy)
                    estimate = row['cost_estimate']
                    self.record({'row': row, 'saved': proxy.saved(), 'kind': kind})
                    self.assertEqual(row['primary']['calls'], 1)
                    self.assertEqual(row['primary']['input_tokens'], 2000)
                    self.assertEqual(estimate['unavailable_primary_cost_calls'], 1)
                    self.assertEqual(estimate['matched_primary_responses'], 0)
                    for field in ('observed_primary_cost', 'traditional_cost', 'carry_cost', 'savings'):
                        self.assertIsNone(estimate[field], field)
                    if kind == 'tariff':
                        self.assertEqual(row['primary']['cache_observation'],
                                         {'calls': 1, 'input_tokens': 2000, 'cached_tokens': 10})
                finally:
                    proxy.close()
            finally:
                fixture.close()
                self.assertEqual(fixture.errors, [])

    def test_missing_cache_partition_remains_unknown(self):
        self.unknown_receipt('missing')

    def test_impossible_cache_partitions_remain_unknown(self):
        self.unknown_receipt('conflict')

    def test_unknown_tariff_preserves_native_cache_observation(self):
        self.unknown_receipt('tariff')


def install_cases():
    policies = {'omitted': None, 'empty': {}, 'implicit': {'mode': 'implicit', 'ttl': '30m'},
                'explicit': {'mode': 'explicit', 'ttl': '30m'}}
    for mode in ('off', 'audit', 'compact'):
        for name, options in policies.items():
            for write in ('zero', 'missing'):
                cached_pattern = [0, 0, 0] if name == 'explicit' else [0, 60000, 0]
                def case(self, mode=mode, options=options, write=write, cached_pattern=cached_pattern):
                    self.lifecycle(mode=mode, options=options, write=write, cached_pattern=cached_pattern)
                setattr(NoOpParity, f'test_{mode}_{name}_{write}_cold_hit_miss_restart', case)
        def paid(self, mode=mode):
            self.lifecycle(mode=mode, options={'mode': 'implicit', 'ttl': '30m'},
                           write='paid', cached_pattern=[0, 60000, 0])
        setattr(NoOpParity, f'test_{mode}_genuinely_paid_write_control', paid)
        def streamed(self, mode=mode):
            self.lifecycle(mode=mode, options=None, write='missing', cached_pattern=[0, 60000, 0], sse=True)
        setattr(NoOpParity, f'test_{mode}_sse_cold_hit_miss_restart', streamed)


install_cases()
if __name__ == '__main__':
    unittest.main(verbosity=2)
