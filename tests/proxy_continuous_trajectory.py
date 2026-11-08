"""Independent native-unit oracle; real HTTP/restart, loopback receipts only."""
import copy
import unittest
import proxy_state_regressions as support


class ContinuousTrajectory(unittest.TestCase):
    setUp = support.StateRegressions.setUp
    record_evidence = support.StateRegressions.record_evidence
    fixture = support.StateRegressions.fixture

    def trajectory(self, *, large, opaque, boundary=False):
        count, step = 50, 30000
        inputs = [(300000 if large else 10000) + i * step
                  - (1000000 if boundary and i >= 35 else 0) for i in range(count)]
        output = [{'type':'reasoning','summary':[], 'encrypted_content':'quantity-unknown'}] if opaque else []
        outputs = []
        def primary(_body, n):
            current = copy.deepcopy(output)
            if opaque:
                current[0]['encrypted_content'] += f'-{n}'
            outputs.append(current)
            result = support.completed(current, f'continuous-{n}', input_tokens=inputs[n-1])
            result['usage']['output_tokens'] = 1000
            return 200, result
        fixture = self.fixture(primary, lambda *_: self.fail('unexpected reviewer'))
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','2000000']
        proxy = support.Proxy(self.state, fixture, options, mode='off')
        self.stack.callback(proxy.close)
        history = [{'role':'assistant','content':'old context'}, {'role':'user','content':'current'}]
        expected_wires, snapshots, oracle = [], [], []
        retired, summary, primaries, summaries, cycles = 0, 0, 0., 0., 0
        canonical, prior_native = inputs[0], inputs[0]
        for i, native in enumerate(inputs):
            # A declared same-owner canonical rebase cannot restore retired
            # source or erase its retained summary. Unknown new growth at that
            # boundary is explicitly estimated as zero, not negative growth.
            if i:
                canonical += max(0, native - prior_native)
            prior_native = native
            if i:
                history += copy.deepcopy(outputs[-1]) + [{'role':'user','content':f'append {i}'}]
            if i == 25:
                before = support.cost_estimate(proxy)
                proxy.close()
                proxy = support.Proxy(self.state, fixture, options, mode='off')
                self.stack.callback(proxy.close)
                self.assertEqual(support.cost_estimate(proxy), before)
            fields = {'model':'gpt-6.1-sol', 'max_output_tokens':1 if i < 20 else 2,
                      'prompt_cache_options':{'mode':'explicit', **({'ttl':'30m'} if i < 30 else {})}}
            if boundary:
                fields['instructions'] = 'new settings' if i >= 15 else 'old settings'
                if i == 35:
                    history[0] = {'role':'assistant','content':'reserialized old context'}
            response = proxy.send(history, **fields)
            self.assertEqual(response[0], 200, response)
            expected_wires.append(copy.deepcopy(history))
            # The fixture's append block has 20 visible/framing units. Native
            # input grows 30,000, so its admitted neutral opaque residual is
            # 29,980 on subsequent responses. First response has no such block;
            # the chronological-only fallback is its 1,000 billed-output proxy.
            retained = (1000 if i == 0 else step - 20) if opaque else 0
            incoming = canonical - retired + summary
            if i == 0 and large:
                retired, summary, incoming = native - 20000, 1000, 21000
            rate = 4 if incoming > 272000 else 2
            primary_charge = (incoming * rate + 1000 * (15 if incoming > 272000 else 10)) / 1e6
            primaries += primary_charge
            summary_charge = 0.
            total = incoming + retained
            if total > 272000 - 16384:
                # Summary tariff uses its own summarized-input tier.
                summary_input = total - 20000
                summary_charge = (summary_input * (4 if summary_input > 272000 else 2) + 1000 * (15 if summary_input > 272000 else 10)) / 1e6
                summaries += summary_charge
                cycles += 1
                retired += total - 20000 - summary
                summary = 1000
            oracle.append({'request':i+1, 'native':native, 'traditional_input':incoming,
                           'primary_charge':primary_charge, 'summary_charge':summary_charge,
                           'paid_cycles':cycles, 'total':primaries+summaries})
            snapshots.append(support.cost_estimate(proxy))
        saved = proxy.saved()
        stats = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(oracle=oracle, estimates=snapshots, stats=stats, checkpoint=saved,
                             primaries=fixture.primaries, large=large, opaque=opaque)
        self.assertEqual(fixture.errors, [])
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(len(fixture.primaries), count)
        self.assertEqual([p['input'] for p in fixture.primaries], expected_wires)
        actual = sum((n * (4 if n > 272000 else 2) + 1000 * (15 if n > 272000 else 10)) / 1e6 for n in inputs)
        self.assertAlmostEqual(saved['primary']['cost_usd'], actual, places=10)
        self.assertEqual(saved['primary']['calls'], count)
        self.assertEqual(saved['shadow']['calls'], 0)
        self.assertGreaterEqual(cycles, 5)
        for i,(estimate, expected) in enumerate(zip(snapshots,oracle)):
            with self.subTest(request=i+1, large=large, opaque=opaque):
                whole = estimate['whole_window']
                self.assertEqual(whole['primary_responses'], i+1)
                self.assertAlmostEqual(whole['traditional_cost'], expected['total'], places=10,
                    msg='CONTINUOUS_TRAJECTORY: every retained primary advances the same summary history, not a matched-expense ratio')
        whole = snapshots[-1]['whole_window']
        self.assertAlmostEqual(whole['carry_cost'], actual, places=10)
        trajectory = whole['trajectory']
        self.assertEqual(trajectory['primary_responses'], count)
        self.assertEqual(trajectory['initial_compactions'], int(large))
        self.assertEqual(trajectory['traditional_compactions'], cycles)
        self.assertAlmostEqual(trajectory['summary_cost'], summaries, places=10)
        self.assertNotIn('reference_cost_ratio', whole)
        self.assertEqual(stats['cost_estimate']['whole_window']['traditional_cost'], whole['traditional_cost'])

    def test_fifty_small_start_settings_and_recognition_changes(self):
        self.trajectory(large=False, opaque=False, boundary=True)

    def test_fifty_large_start_settings_and_recognition_changes(self):
        self.trajectory(large=True, opaque=False, boundary=True)

    def test_fifty_small_start_known_output_acceptance(self):
        self.trajectory(large=False, opaque=False)

    def test_fifty_large_start_known_output_acceptance(self):
        self.trajectory(large=True, opaque=False)

    def test_fifty_small_start_unknown_retention_continuous_estimate(self):
        self.trajectory(large=False, opaque=True)

    def test_fifty_large_start_unknown_retention_continuous_estimate(self):
        self.trajectory(large=True, opaque=True)


if __name__ == '__main__':
    unittest.main()
