"""Shared quantity policy across real HTTP/Session and offline report adapters.

All traces, receipts and state are synthetic and private; no provider calls.
Native request totals and local prompt estimates intentionally remain distinct.
"""
import copy
from history_store import read_index, write_index
import json
import os
import subprocess
import unittest
from html.parser import HTMLParser

import proxy_state_regressions as support
from proxy_legacy_history import OwnerProxy


class Cells(HTMLParser):
    def __init__(self):
        super().__init__()
        self.cells = []
        self.cell = None

    def handle_starttag(self, tag, attrs):
        if tag == 'td':
            self.cell = ''

    def handle_data(self, text):
        if self.cell is not None:
            self.cell += text

    def handle_endtag(self, tag):
        if tag == 'td' and self.cell is not None:
            self.cells.append(self.cell)
            self.cell = None


class QuantityPolicy(support.FixtureCase):
    def test_unknown_money_does_not_suppress_later_summary_in_either_adapter(self):
        results = {}
        self.evidence['arms'] = results
        for ttl in ['30m', '12h']:
            bodies = []
            history = [{'role': 'assistant', 'content': ' evidence' * 10000},
                       {'role': 'user', 'content': 'continue'}]
            for index in range(3):
                if index == 1:
                    history.append({'role': 'assistant', 'content': ' growth' * 25000})
                elif index == 2:
                    history.append({'role': 'user', 'content': ' append' * 100})
                bodies.append({'model': 'gpt-6.1-sol', 'max_output_tokens': 1,
                               'prompt_cache_options': {'mode': 'implicit', 'ttl': ttl},
                               'input': copy.deepcopy(history)})
            # Both quantity domains cross their own declared threshold. Do not
            # cast provider totals into local tokenizer estimates for parity.
            inputs = [10000, 300000, 300100]
            for shared in [False, True]:
                kind = f'{ttl}-{shared}'
                state = self.state / kind
                state.mkdir()

                def primary(_body, n):
                    response = support.completed(response_id=f'quantity-{kind}-{n}')
                    response['usage'] = {'input_tokens': inputs[n - 1], 'output_tokens': 0,
                                         'input_tokens_details': {'cached_tokens': 0,
                                                                  'cache_write_tokens': 1000 if n == 2 else 0}}
                    return 200, response

                fixture = self.fixture(primary, lambda *_: self.fail('unexpected reviewer'))
                self.addCleanup(lambda fixture=fixture: self.assertEqual(fixture.errors, []))
                snapshots, estimates, owners = [], [], []
                previous_files = {}
                for index, body in enumerate(bodies):
                    proxy = OwnerProxy(state, fixture, shared=shared, mode='audit', wait_reviews=False,
                                       extra_env={'CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS': '1000000'})
                    self.stack.callback(proxy.close)
                    for name, saved in previous_files.items():
                        self.assertEqual(read_index(state / name), saved,
                                         'restart must not mutate settled quantities or money')
                    self.assertEqual(proxy.send(**{'input_items': body['input']},
                                                **{k: v for k, v in body.items() if k != 'input'})[0], 200)
                    stats = proxy.request('GET', '/carry/dashboard/stats')[1]
                    self.assertEqual(len(stats['sessions']), 1)
                    estimates.append(copy.deepcopy(stats['sessions'][0]['cost_estimate']))
                    proxy.close()
                    indexed = read_index(state / 'history.index')
                    refs = [ref for ref in indexed['transcripts'].values()
                            if ref['external_id'] == f'quantity-{kind}-{index + 1}']
                    self.assertEqual(len(refs), 1)
                    owner = refs[0]['session']
                    owners.append(owner)
                    saved = (indexed['snapshots'][owner] if shared else
                             json.loads((state / f'{owner}.json').read_text()))
                    snapshots.append(copy.deepcopy(saved))
                    previous_files = {p.name: json.loads(p.read_text()) for p in state.glob('*.json')}
                    previous_files['history.index'] = indexed
                self.assertEqual(len(set(owners)), 1, 'one native history owner across restart')
                self.assertEqual(fixture.primaries, bodies, 'actual complete provider wires must match')
                self.assertEqual(fixture.reviews, [])
                self.assertEqual(len(snapshots), 3)
                known_paid = 0.0
                for index, saved in enumerate(snapshots):
                    if ttl == '30m' or index != 1:
                        known_paid += (inputs[index] * (4 if inputs[index] > 272000 else 2)
                                       + (1000 * 1 if index == 1 else 0)) / 1e6
                    estimate = estimates[index]
                    self.assertAlmostEqual(estimate['observed_primary_cost'], known_paid, places=12)
                    self.assertAlmostEqual(saved['primary']['cost_usd'], known_paid, places=12)
                    self.assertEqual(saved['primary']['calls'], index + 1)
                    self.assertEqual(saved['primary']['unavailable_cost_calls'], int(ttl == '12h' and index >= 1))
                    self.assertEqual(saved['shadow']['calls'], 0)
                    self.assertEqual(saved['compactions'], 0)
                    self.assertEqual(estimate['initial_compactions'], 0)
                    facts = [o['Primary']['facts'] for o in saved['request_evidence']['observations'] if 'Primary' in o]
                    self.assertEqual([f['native']['input_tokens'] for f in facts], inputs[:index + 1])
                    if index:
                        old = snapshots[index - 1]['request_evidence']['observations']
                        self.assertEqual(saved['request_evidence']['observations'][:len(old)], old)
                final = estimates[-1]
                self.assertEqual(final['traditional_compactions'], 1)
                self.assertEqual(final['summary_cost'] > 0, ttl == '30m',
                                 'unknown interval must not backfill modeled summary money')
                results[kind] = {'snapshots': snapshots, 'estimates': estimates, 'owners': owners,
                                 'primaries': fixture.primaries, 'reviews': fixture.reviews}
                fixture.close()

            directory = self.state / f'report-{ttl}'
            session = directory / 'sessions' / f'report-{ttl}'
            session.mkdir(parents=True)
            events = [{'event': 'run_started', 'data': {'model': 'gpt-6.1-sol', 'context_window': 40000,
                       'cache_ttl_seconds': 1800, 'implicit_cache_minimum_prefix_tokens': 1024}}]
            for step, body in enumerate(bodies, 1):
                events.extend([
                    {'event': 'model_request', 'run_id': ttl, 'timestamp_ms': step * 1000,
                     'data': {'step': step, 'route': 'synthetic-policy', 'request': body}},
                    {'event': 'model_response', 'run_id': ttl, 'timestamp_ms': step * 1000 + 1,
                     'data': {"usage_source":"normalized",'step': step, 'usage': {'input_tokens': inputs[step - 1], 'output_tokens': 0,
                              'cached_input_tokens': 0, 'cache_write_input_tokens': 1000 if step == 2 else 0},
                              'raw': {'output': []}}}])
            (session / 'trace.jsonl').write_text(''.join(json.dumps(event) + '\n' for event in events))
            html = directory / 'cost.html'
            process = subprocess.run([os.environ['CARRY_TEST_BINARY'], 'report', 'cost', '--sessions',
                                      str(session.parent), '--output', str(html)],
                                     env={'PATH': '', 'HOME': str(directory), 'TMPDIR': str(directory)},
                                     capture_output=True, text=True, timeout=30)
            (directory / 'stdout.txt').write_text(process.stdout)
            (directory / 'stderr.txt').write_text(process.stderr)
            self.assertEqual(process.returncode, 0, process.stderr)
            parser = Cells()
            parser.feed(html.read_text())
            row = parser.cells
            self.assertEqual(len(row), 10, row)
            self.assertEqual(row[0], f'report-{ttl}')
            self.assertEqual(row[2], '3/3')
            self.assertIn('0 missing usage', row[9])
            self.assertIn('0 missing input / 0 output projections', row[9])
            self.assertIn('0 unmatched', row[9])
            self.assertIn('0 orphan', row[9])
            self.assertIn('0 invalid', row[9])
            self.assertIn(f'{int(ttl == "12h")} unpriced', row[9])
            self.assertEqual(row[3], '$2.4214' if ttl == '30m' else '$1.2204 (partial)')
            results[f'report-{ttl}'] = {'row': row, 'events': events}
        # All paid/native/wire/restart and report match/projection controls above
        # run before the differential semantic grader.
        for ttl in ['30m', '12h']:
            with self.subTest(ttl=ttl):
                self.assertEqual(results[f'report-{ttl}']['row'][7], '1',
                                 'SHARED_POLICY: unknown money cannot suppress a quantity summary')


if __name__ == '__main__':
    unittest.main()
