"""Receipt-calibrated occurrence pool; real review/restart and loopback receipts."""
import json
import shutil
import unittest
import proxy_state_regressions as support


class VisiblePool(unittest.TestCase):
    setUp = support.StateRegressions.setUp
    record_evidence = support.StateRegressions.record_evidence
    fixture = support.StateRegressions.fixture

    def test_repeated_visible_initial_pool_prices_after_indexed_restart(self):
        self.check_initial_pool(repeated=True)

    def test_distinct_visible_initial_pool_control(self):
        self.check_initial_pool(repeated=False)

    def check_initial_pool(self, repeated):
        visible = {'role': 'assistant', 'content': 'identical synthetic evidence ' * 30}
        sibling = visible if repeated else {**visible, 'content': visible['content'] + 'distinct'}
        old = {'type': 'reasoning', 'summary': [],
               'encrypted_content': 'old synthetic' * 1000}
        body = [
            {'role': 'user', 'content': 'goal'}, old,
            {'type': 'function_call', 'call_id': 'old', 'name': 'inspect', 'arguments': '{}'},
            {'type': 'function_call_output', 'call_id': 'old', 'output': 'evidence'},
            visible, sibling, {'role': 'user', 'content': 'current'},
        ]
        append = [
            {'type': 'reasoning', 'summary': [], 'encrypted_content': 'fresh synthetic' * 1000},
            {'role': 'assistant', 'content': 'fresh answer'},
            {'role': 'user', 'content': 'continue'},
        ]
        fixture = self.fixture(
            lambda _b, n: (200, support.completed([], f'pool-{n}', input_tokens=10000)),
            lambda _b, _n: (200, support.advice_response(
                {'protected': [], 'removable': [2], 'memories': []})))
        options = ['--classifier-cache-policy', 'disabled', '--payoff-requests', '1',
                   '--review-every-requests', '1', '--primary-context-tokens', '100000',
                   '--context-margin-percent', '0']
        seed_dir = self.state / 'seed'
        seed_dir.mkdir()
        proxy = support.Proxy(seed_dir, fixture, options)
        self.stack.callback(proxy.close)
        for _ in range(2):
            self.assertEqual(proxy.send(body, model='gpt-6-sol')[0], 200)
        seed = proxy.saved()
        advice = proxy.advice()
        self.assertEqual(advice['drops'], {'2': [2, 3, 4]})
        self.assertEqual(len(fixture.reviews), 1)
        self.assertEqual([b for b in fixture.primaries],
                         [{'model': 'gpt-6-sol', 'input': body}] * 2)
        anchor = seed['primary_input']['anchor']
        self.assertEqual(anchor['input'], 10000)
        self.assertEqual(anchor['prompt']['item_count'], len(body))
        ids = [item['identity'] for item in anchor['prompt']['items']]
        self.assertEqual(ids.count(ids[4]), 2 if repeated else 1)
        self.assertEqual(sum(item['opaque'] for item in anchor['prompt']['items']), 1)
        # No append receipt block owns old reasoning or the visible repetitions.
        self.assertTrue(all(not b['items'] for b in seed['primary_input'].get('blocks', [])))
        proxy.close()
        observations = {}
        self.evidence.update(seed=seed, advice=advice, arms=observations, body=body, append=append)
        for name in ['indexed_advice', 'without_advice']:
            directory = self.state / name
            shutil.copytree(seed_dir, directory)
            if name == 'without_advice':
                index_path = directory / 'history.index'
                index = json.loads(index_path.read_text())
                self.assertEqual(len(index['reviews']), 1)
                index['reviews'] = {}
                index_path.write_text(json.dumps(index))
            proxy = support.Proxy(directory, fixture, [
                '--classifier-cache-policy', 'disabled', '--payoff-requests', '1',
                '--review-every-requests', '100', '--primary-context-tokens', '1024',
                '--context-margin-percent', '0'])
            self.stack.callback(proxy.close)
            self.assertEqual(proxy.send(body + append, model='gpt-6-sol')[0], 200)
            saved = proxy.saved()
            plan = saved['last_plan']
            observations[name] = {'saved': saved, 'wire': fixture.primaries[-1]}
            canonical = body + append
            omitted = [body[0], {'role': 'user', 'content':
                '[Earlier conversation omitted by Carry; retained messages remain in original order.]'},
                *body[4:], *append]
            # Complete-wire/native-money/count/capacity guards precede sizing.
            self.assertEqual(fixture.primaries[-1], {'model': 'gpt-6-sol',
                             'input': omitted if plan['selected'] else canonical})
            self.assertEqual(len(fixture.reviews), 1)
            self.assertEqual(saved['completed_requests'], 3)
            self.assertEqual(saved['primary']['calls'], 3)
            self.assertEqual(saved['primary']['input_tokens'], 30000)
            self.assertEqual(saved['primary']['output_tokens'], 3)
            self.assertEqual(saved['primary']['cached_tokens'], 0)
            self.assertEqual(saved['primary']['cache_write_tokens'], 0)
            self.assertEqual(saved['primary']['unavailable_cost_calls'], 0)
            self.assertAlmostEqual(saved['primary']['cost_usd'], 0.06003, places=12)
            self.assertAlmostEqual(saved['primary']['cost_usd'], seed['primary']['cost_usd'] * 1.5, places=12)
            self.assertEqual(saved['shadow'], seed['shadow'])
            self.assertAlmostEqual(saved['shadow']['cost_usd'], 0.0000105, places=12)
            self.assertFalse(saved['primary_input']['reasoning_conflict'])
            self.assertEqual(plan['primary_input_basis'], 'receipt_anchored_reasoning_forecast')
            # Native registration checks the independent neutral occurrence oracle.
            self.assertGreater(plan['primary_estimated_tokens'], 10000)
            self.assertIsNone(plan['primary_receipt_prediction_tokens'])
            self.assertIsNone(plan['primary_required_tokens'])
            self.assertIsNone(plan['primary_fits'])
            self.assertIsNone(saved['last_primary_dispatch']['outgoing_input_tokens'])
            self.assertFalse(plan['forced'])
            if name == 'without_advice':
                self.assertFalse(plan['selected'])
                self.assertEqual(plan['candidates'], [])
            else:
                self.assertEqual(plan['candidates'][0]['omitted_members'], [2, 3, 4])
            proxy.close()
        self.assertEqual(len(fixture.primaries), 4)
        self.assertEqual(fixture.errors, [])
        self.evidence.update(upstream_bodies=fixture.primaries, reviewer_calls=len(fixture.reviews),
                             fixture_errors=fixture.errors, independent_guards='PASS')
        # Independent sizing grader: baseline availability does not price edits.
        plan = observations['indexed_advice']['saved']['last_plan']
        candidate = plan['candidates'][0]
        self.assertIsNotNone(candidate['primary_retained_estimated_tokens'],
                             'repeated visible occurrences must not erase the residual pool forecast')
        self.assertGreater(candidate['primary_retained_estimated_tokens'], plan['primary_estimated_tokens'] // 2)
        self.assertLess(candidate['primary_retained_estimated_tokens'], plan['primary_estimated_tokens'])
        self.assertTrue(candidate['selection_joint']['accepted'])
        self.assertTrue(plan['selected'])


if __name__ == '__main__':
    unittest.main()
