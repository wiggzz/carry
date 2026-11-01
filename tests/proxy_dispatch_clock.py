"""Dispatch-time comparison against real HTTP settlement and restart.

Private synthetic native receipts and a deliberately aged modeled point write;
not observed provider residency, a live-state migration, or a paid probe.
"""
import copy
import json
import time
import unittest

import proxy_state_regressions as support


class DispatchClock(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.harness._testMethodName = self._testMethodName
        self.evidence = self.harness.evidence
        self.addCleanup(lambda: self.assertTrue(self.harness.doCleanups(),
                                               "nested fixture cleanup failed"))

    def test_warm_dispatch_crossing_expiry_preserves_frozen_comparison(self):
        inputs = [300000, 300100, 300200, 300300]
        clock = {}

        def primary(_body, n):
            if n == 3:
                clock['arrived'] = int(time.time())
                self.assertLess(clock['arrived'], clock['expires'],
                                'fixture must reach upstream before original expiry')
                # A real held response crosses the existing write's deadline;
                # pricing cannot depend on how long terminal settlement takes.
                time.sleep(max(0, clock['expires'] + 1 - time.time()))
                clock['released'] = int(time.time())
                self.assertGreaterEqual(clock['released'], clock['expires'])
            response = support.completed(response_id=f'dispatch-clock-{n}')
            response['usage'] = {'input_tokens': inputs[n - 1], 'output_tokens': 0,
                                 'input_tokens_details': {'cached_tokens': 0,
                                                          'cache_write_tokens': 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail('unexpected reviewer'))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{'role': 'assistant', 'content': 'owned summarized native history'},
                   {'role': 'user', 'content': 'continue'}]
        wires, snapshots = [], []
        self.evidence.update(clock=clock, snapshots=snapshots, wires=wires,
                             primaries=fixture.primaries, reviews=fixture.reviews)

        def start():
            proxy = support.Proxy(self.harness.state, fixture, mode='audit',
                                  extra=['--request-timeout-secs', '15'],
                                  extra_env={'CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS': '1000000'})
            self.harness.stack.callback(proxy.close)
            return proxy

        def send(proxy, n):
            if n > 1:
                history.append({'role': 'user', 'content': f'append {n}'})
            wires.append(copy.deepcopy(history))
            self.assertEqual(proxy.send(history, model='gpt-6.1-sol', max_output_tokens=1)[0], 200)
            saved = copy.deepcopy(proxy.saved())
            snapshots.append(saved)
            return saved

        proxy = start()
        send(proxy, 1)
        before = send(proxy, 2)
        proxy.close()
        trajectory = before['cost_trajectory']
        self.assertEqual(trajectory['retired'], 280000)
        self.assertEqual(trajectory['summary'], 1000)
        self.assertEqual(trajectory['last_traditional_input'], 21100)
        self.assertIs(trajectory['shared_primary_history'], False)
        # Point-write fixture: replace ONLY the modeled cache event timestamps
        # while stopped, retaining scope, complete current history and all money.
        staged = copy.deepcopy(before)
        cache = staged['cost_trajectory']['receipts']['modeled_cache']
        self.assertTrue(cache['scope'])
        self.assertTrue(cache['writes'])
        self.assertEqual(max(w['end'] for w in cache['writes']), 21100)
        clock['staged'] = int(time.time())
        clock['expires'] = clock['staged'] + 6
        original = {'start': 0, 'end': 21100, 'at': clock['expires'] - 1800,
                    'lifetime': 1800}
        cache['writes'] = [original]
        # This tracer isolates the non-refreshing dispatch clock. Qualified 30m
        # reads now legitimately have separate coverage (tested independently).
        # Declare a frozen synthetic custom policy and stage ONLY a point write,
        # not a fresh read left over from this fixture's earlier setup request.
        cache.pop('reads', None)
        staged['model_resolution']['primary']['scenario_cache_policy']['ttl_seconds'] = 1799
        cache.pop('unresolved', None)
        checkpoint = next(self.harness.state.glob('*.json'))
        checkpoint.write_text(json.dumps(staged))
        self.evidence.update(staged=staged, original_write=original)

        proxy = start()
        self.assertEqual(proxy.saved()['cost_trajectory'], staged['cost_trajectory'],
                         'restart cannot reprice history or refresh original modeled writes')
        held = send(proxy, 3)
        proxy.close()
        proxy = start()
        self.assertEqual(proxy.saved()['cost_trajectory'], held['cost_trajectory'])
        after = send(proxy, 4)
        proxy.close()
        self.assertEqual(len(fixture.primaries), 4)
        self.assertEqual(fixture.reviews, [])
        for index, saved in enumerate(snapshots):
            self.assertEqual(fixture.primaries[index]['input'], wires[index])
            self.assertEqual(fixture.primaries[index]['max_output_tokens'], 1)
            estimate = saved['cost_trajectory']['result']
            paid = sum(inputs[:index + 1]) * 4 / 1e6
            self.assertAlmostEqual(saved['primary']['cost_usd'], paid, places=12)
            self.assertAlmostEqual(estimate['observed_primary_cost'], paid, places=12)
            self.assertAlmostEqual(estimate['carry_cost'], paid, places=12)
            self.assertEqual(estimate['excluded_unpriced_calls'], 0)
            self.assertEqual(estimate['traditional_compactions'], 0)
            self.assertEqual(estimate['initial_compactions'], 1)
            self.assertEqual(estimate['summary_cost'], 0)
            self.assertEqual(saved['cost_trajectory']['retired'], 280000)
            self.assertEqual(saved['cost_trajectory']['summary'], 1000)
            self.assertEqual(saved['cost_trajectory']['last_traditional_input'], 21000 + 100 * index)
        # Complete raw native money/wire/count/history and restart guards before
        # collecting independent clock failures, so one cannot hide the other.
        h = held['cost_trajectory']
        a = after['cost_trajectory']
        b = before['cost_trajectory']
        with self.subTest(boundary='dispatch-price'):
            self.assertAlmostEqual(h['result']['traditional_cost'] - b['result']['traditional_cost'],
                                   (21100 * .1 + 100 * 2.5) / 1e6, places=12,
                                   msg='DISPATCH_CLOCK: warm admission cannot become cold at terminal')
        with self.subTest(boundary='original-write-not-renewed'):
            self.assertIn(original, h['receipts']['modeled_cache']['writes'],
                          'DISPATCH_CLOCK: terminal time cannot rewrite the old prefix')
        with self.subTest(boundary='expired-restarted-continuation'):
            self.assertAlmostEqual(a['result']['traditional_cost'] - h['result']['traditional_cost'],
                                   21300 * 2.5 / 1e6, places=12,
                                   msg='DISPATCH_CLOCK: terminal must not mint a fresh whole-prefix write')


if __name__ == '__main__':
    unittest.main()
