"""Real held HTTP settlement crossing a frozen replay-cache deadline.

Only private synthetic dispatch clocks/policy are aged. Native quantities,
settled money and complete provider wires remain unmodified.
"""
import copy
import json
import time
import unittest

import proxy_state_regressions as support


class DispatchClock(support.FixtureCase):
    def test_warm_dispatch_crossing_expiry_preserves_frozen_comparison(self):
        inputs = [300000, 300100, 300200, 300300]
        clock = {}

        def primary(_body, n):
            if n == 3:
                clock['arrived'] = int(time.time())
                self.assertLess(clock['arrived'], clock['expires'],
                                'fixture must reach upstream before original expiry')
                time.sleep(max(0, clock['expires'] + 1 - time.time()))
                clock['released'] = int(time.time())
                self.assertGreaterEqual(clock['released'], clock['expires'])
            response = support.completed(response_id=f'dispatch-clock-{n}')
            response['usage'] = {'input_tokens': inputs[n - 1], 'output_tokens': 0,
                                 'input_tokens_details': {'cached_tokens': 0,
                                                          'cache_write_tokens': 0}}
            return 200, response

        fixture = self.fixture(primary, lambda *_: self.fail('unexpected reviewer'))
        history = [{'role': 'assistant', 'content': 'owned summarized native history'},
                   {'role': 'user', 'content': 'continue'}]
        wires, snapshots, estimates = [], [], []
        self.evidence.update(clock=clock, snapshots=snapshots, estimates=estimates, wires=wires,
                             primaries=fixture.primaries, reviews=fixture.reviews)

        def start():
            proxy = support.Proxy(self.state, fixture, mode='audit',
                                  extra=['--request-timeout-secs', '15'],
                                  extra_env={'CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS': '1000000'})
            self.stack.callback(proxy.close)
            return proxy

        def estimate(proxy):
            stats = proxy.request('GET', '/carry/dashboard/stats')[1]
            self.assertEqual(len(stats['sessions']), 1)
            return copy.deepcopy(stats['sessions'][0]['cost_estimate'])

        def send(proxy, n):
            if n > 1:
                history.append({'role': 'user', 'content': f'append {n}'})
            wires.append(copy.deepcopy(history))
            self.assertEqual(proxy.send(history, model='gpt-6.1-sol', max_output_tokens=1)[0], 200)
            saved = copy.deepcopy(proxy.saved())
            snapshots.append(saved)
            estimates.append(estimate(proxy))
            return saved

        proxy = start()
        send(proxy, 1)
        before = send(proxy, 2)
        proxy.close()
        self.assertEqual(estimates[-1]['initial_compactions'], 1)
        self.assertEqual(estimates[-1]['traditional_compactions'], 0)
        # Replay now owns cache events: age the actual dispatch facts instead
        # of injecting retired modeled-cache state. A custom 1799s policy has
        # no read-refresh entitlement. Keep native receipts and frozen cost.
        staged = copy.deepcopy(before)
        clock['staged'] = int(time.time())
        clock['expires'] = clock['staged'] + 6
        for observation in staged['request_evidence']['observations']:
            if 'Primary' in observation:
                facts = observation['Primary']['facts']
                facts['at'] = clock['expires'] - 1799
                facts['read_refresh'] = None
                facts['resolved']['scenario_cache_policy']['ttl_seconds'] = 1799
        staged['model_resolution']['primary']['scenario_cache_policy']['ttl_seconds'] = 1799
        checkpoint = next(self.state.glob('*.json'))
        checkpoint.write_text(json.dumps(staged))
        self.evidence['staged'] = staged

        proxy = start()
        self.assertEqual(proxy.saved()['request_evidence'], staged['request_evidence'])
        self.assertEqual(estimate(proxy), estimates[-1], 'uniform clock aging cannot reprice old requests')
        held = send(proxy, 3)
        proxy.close()
        proxy = start()
        self.assertEqual(proxy.saved()['request_evidence'], held['request_evidence'])
        self.assertEqual(estimate(proxy), estimates[-1], 'restart/poll cannot renew expired prefix')
        after = send(proxy, 4)
        proxy.close()
        self.assertEqual(len(fixture.primaries), 4)
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(fixture.errors, [])
        old = staged['request_evidence']['observations']
        for saved in (held, after):
            self.assertEqual(saved['request_evidence']['observations'][:len(old)], old,
                             'terminal settlement cannot rewrite old dispatch events')
        for index, (saved, result) in enumerate(zip(snapshots, estimates)):
            self.assertEqual(fixture.primaries[index]['input'], wires[index])
            self.assertEqual(fixture.primaries[index]['max_output_tokens'], 1)
            paid = sum(inputs[:index + 1]) * 4 / 1e6
            self.assertAlmostEqual(saved['primary']['cost_usd'], paid, places=12)
            self.assertAlmostEqual(result['observed_primary_cost'], paid, places=12)
            self.assertAlmostEqual(result['carry_cost'], paid, places=12)
            self.assertEqual(result['excluded_unpriced_calls'], 0)
            self.assertEqual(result['traditional_compactions'], 0)
            self.assertEqual(result['initial_compactions'], 1)
            self.assertEqual(result['summary_cost'], 0)
        self.assertAlmostEqual(estimates[2]['traditional_cost'] - estimates[1]['traditional_cost'],
                               (21100 * .1 + 100 * 2.5) / 1e6, places=12,
                               msg='warm dispatch cannot become cold at delayed settlement')
        self.assertAlmostEqual(estimates[3]['traditional_cost'] - estimates[2]['traditional_cost'],
                               21300 * 2.5 / 1e6, places=12,
                               msg='terminal cannot mint a fresh whole-prefix write')


if __name__ == '__main__':
    unittest.main()
