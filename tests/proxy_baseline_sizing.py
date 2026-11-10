"""Current-session baseline sizing; real binary, synthetic loopback receipts."""
import unittest
import proxy_state_regressions as support


class BaselineSizing(unittest.TestCase):
    setUp = support.StateRegressions.setUp
    record_evidence = support.StateRegressions.record_evidence
    fixture = support.StateRegressions.fixture
    proxy = support.StateRegressions.proxy

    def test_fresh_reasoning_without_increment_is_unknown_until_native_measurement(self):
        reasoning = {"type": "reasoning", "summary": [],
                     "encrypted_content": "synthetic-opaque/" * 1000}
        answer = {"role": "assistant", "content": "synthetic answer"}
        fixture = self.fixture(lambda _b, n: (200, support.completed(
            [reasoning, answer] if n == 1 else [], f"baseline-{n}", input_tokens=10000)))
        options = ["--review-every-requests", "100", "--review-min-primary-tokens", "1000000",
                   "--primary-context-tokens", "1000000"]
        proxy = self.proxy(fixture, options)
        history = [{"role": "user", "content": "synthetic goal"}]
        self.assertEqual(proxy.send(history)[0], 200)
        seed = proxy.saved()
        self.assertEqual(seed['primary_input']['anchor']['input'], 10000)
        self.assertIsNotNone(seed['last_plan']['primary_estimated_tokens'])
        history += [reasoning, answer, {"role": "user", "content": "continue"}]
        self.assertEqual(proxy.send(history)[0], 200)
        appended = proxy.saved()
        # An unchanged replay has an exact receipt, explaining intermittency.
        self.assertEqual(proxy.send(history)[0], 200)
        replay = proxy.saved()
        self.evidence.update(seed=seed, appended=appended, replay=replay,
                             upstream_inputs=[b['input'] for b in fixture.primaries])
        self.assertEqual([b['input'] for b in fixture.primaries], [history[:1], history, history])
        self.assertEqual(len(fixture.reviews), 0)
        self.assertEqual(appended['completed_requests'], 2)
        self.assertEqual(appended['primary']['input_tokens'], 20000)
        self.assertAlmostEqual(appended['primary']['cost_usd'], 2 * seed['primary']['cost_usd'], places=12)
        self.assertAlmostEqual(replay['primary']['cost_usd'], 3 * seed['primary']['cost_usd'], places=12)
        self.assertEqual(appended['primary_input']['anchor']['input'], 10000)
        self.assertFalse(appended['primary_input']['reasoning_conflict'])
        self.assertEqual(appended['primary_input']['blocks'][-1]['input'], 0,
                         'only the actual native difference enters measurements')
        self.assertEqual(replay['last_plan']['primary_estimated_tokens'], 10000)
        self.assertEqual(replay['last_plan']['primary_input_basis'], 'provider_receipt')
        self.assertIsNone(appended['last_plan']['primary_receipt_prediction_tokens'],
                          'estimated new reasoning is not native receipt prediction')
        self.assertIsNone(appended['last_primary_dispatch']['outgoing_input_tokens'],
                          'forecast cannot mint actual native input authority')
        self.assertIsNone(appended['last_plan']['primary_estimated_tokens'],
                          'ciphertext length alone cannot supply a neutral native increment')
        self.assertIsNone(appended['last_plan']['primary_input_basis'])
        self.assertEqual(appended['last_plan']['reason'], 'input_quantity_unknown')
        self.assertFalse(appended['last_plan']['forced'])
        self.assertIsNone(appended['last_plan']['primary_fits'],
                          'a heuristic does not establish context-capacity proof')
        self.assertIsNone(appended['last_plan']['primary_required_tokens'])
    def test_restart_then_response_id_unmeasured_reasoning_remains_unknown(self):
        reasoning = {"type": "reasoning", "summary": [],
                     "encrypted_content": "synthetic-restart/" * 1000}
        fixture = self.fixture(lambda _b, n: (200, support.completed(
            [reasoning] if n == 1 else [], f"restart-{n}", input_tokens=10000)))
        options = ["--review-every-requests", "100", "--review-min-primary-tokens", "1000000"]
        proxy = self.proxy(fixture, options)
        first = [{"role": "user", "content": "restart goal"}]
        self.assertEqual(proxy.send(first)[0], 200)
        prior = proxy.saved()
        proxy.close()
        proxy = self.proxy(fixture, options)
        tail = [{"role": "user", "content": "continue after restart"}]
        self.assertEqual(proxy.send(tail, previous_response_id="restart-1")[0], 200)
        saved = proxy.saved()
        self.evidence.update(before_restart=prior, after_restart=saved,
                             upstream_inputs=[b['input'] for b in fixture.primaries])
        self.assertEqual(fixture.primaries[-1]['input'], first + [reasoning] + tail)
        self.assertEqual(saved['completed_requests'], 2)
        self.assertEqual(saved['primary']['input_tokens'], 20000)
        self.assertEqual(saved['primary_input']['anchor']['input'], 10000)
        self.assertEqual(fixture.reviews, [])
        self.assertIsNone(saved['last_plan']['primary_receipt_prediction_tokens'])
        self.assertIsNone(saved['last_plan']['primary_input_basis'])
        self.assertIsNone(saved['last_plan']['primary_estimated_tokens'])
        self.assertEqual(saved['last_plan']['reason'], 'input_quantity_unknown')

    def test_measured_native_increment_forecasts_next_reasoning_without_capacity_authority(self):
        outputs = [{'type':'reasoning', 'encrypted_content':f'opaque-{n}' * (n + 1)} for n in range(2)]
        fixture = self.fixture(lambda _b, n: (200, support.completed(
            [outputs[n-1]] if n <= 2 else [], f'increment-{n}', input_tokens=10000 + (n-1)*100)))
        options = ['--review-every-requests','100','--review-min-primary-tokens','1000000']
        proxy = self.proxy(fixture, options)
        history = [{'role':'user','content':'stable goal'}]
        self.assertEqual(proxy.send(history)[0], 200)
        history += [outputs[0], {'role':'user','content':'continue'}]
        self.assertEqual(proxy.send(history)[0], 200)
        proxy.close()
        proxy = self.proxy(fixture, options)
        history += [outputs[1], {'role':'user','content':'continue'}]
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        self.evidence.update(saved=saved, wires=fixture.primaries)
        self.assertEqual(fixture.primaries[-1]['input'], history)
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(saved['last_plan']['primary_input_basis'], 'receipt_anchored_reasoning_forecast')
        self.assertGreater(saved['last_plan']['primary_estimated_tokens'], 10100)
        self.assertIsNone(saved['last_plan']['primary_required_tokens'])
        self.assertIsNone(saved['last_plan']['primary_fits'])
        dispatch = saved['last_primary_dispatch']
        for field in ['canonical_input_tokens', 'continuation_input_tokens', 'outgoing_input_tokens']:
            self.assertEqual(dispatch[field], saved['last_plan']['primary_estimated_tokens'],
                             'dispatch size estimates must reuse the supported planning quantity')
        self.assertEqual(dispatch['receipt_input_tokens'], 10200)
        self.assertEqual(saved['primary']['input_tokens'], 30300)
        self.assertEqual(saved['primary']['calls'], 3)

    def test_unsupported_or_changed_contract_append_remains_unknown(self):
        reasoning = {"type": "reasoning", "summary": [],
                     "encrypted_content": "synthetic-negative/" * 100}
        cases = {
            'image': ({"role": "user", "content": [{"type": "input_image", "image_url": "data:image/png;base64,AAAA"}]}, {}),
            'compaction': ({"type": "compaction", "encrypted_content": "unbounded"}, {}),
            'tools': (reasoning, {'tools': [{'type': 'function', 'name': 'changed', 'parameters': {'type': 'object'}}]}),
            'instructions': (reasoning, {'instructions': 'changed direction'}),
            'settings': (reasoning, {'reasoning': {'effort': 'high'}}),
        }
        observations = {}
        for name, (item, fields) in cases.items():
            with self.subTest(case=name):
                fixture = self.fixture(lambda _b, n: (200, support.completed(
                    [], f"negative-{n}", input_tokens=10000)))
                state = self.state / name
                state.mkdir()
                # This tests unknown sizing, not ordinary admission. Unknown is
                # no longer a primary-floor veto; keep this two-request fixture
                # below cadence instead of relying on an unknown quantity.
                proxy = support.Proxy(state, fixture, [
                    '--review-min-primary-tokens', '1000000',
                    '--review-every-requests', '100'])
                self.stack.callback(proxy.close)
                history = [{"role": "user", "content": "stable negative goal"}]
                self.assertEqual(proxy.send(history)[0], 200)
                history += [item, {"role": "user", "content": "continue"}]
                self.assertEqual(proxy.send(history, **fields)[0], 200)
                saved = proxy.saved()
                observations[name] = saved
                self.assertEqual(fixture.primaries[-1]['input'], history)
                self.assertEqual(saved['primary']['input_tokens'], 20000)
                self.assertEqual(saved['primary_input']['anchor']['input'], 10000)
                self.assertIsNone(saved['last_plan']['primary_estimated_tokens'])
                self.assertIsNone(saved['last_plan']['primary_input_basis'])
                self.assertEqual(saved['last_plan']['reason'], 'input_quantity_unknown')
                self.assertEqual(fixture.reviews, [])
                proxy.close()
        self.evidence.update(negative_cases=observations)


if __name__ == '__main__':
    unittest.main()
