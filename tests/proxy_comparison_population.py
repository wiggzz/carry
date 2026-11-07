"""Success-scoped comparisons over real loopback dispatch/settlement/restart.

CARRY_TEST_BINARY=/absolute/carry python3 -B tests/proxy_comparison_population.py -v
Paid ledgers remain all-attempt; no providers, production state or credentials.
"""
import json
import unittest
import proxy_state_regressions as support


class ComparisonPopulation(unittest.TestCase):
    setUp = support.StateRegressions.setUp
    record_evidence = support.StateRegressions.record_evidence
    fixture = support.StateRegressions.fixture

    def test_failed_attempts_do_not_censor_or_price_successful_comparisons(self):
        for failure in ('transport', 'unpriced_terminal', 'priced_terminal'):
            with self.subTest(failure=failure):
                state = self.state / failure
                state.mkdir()

                def primary(_body, n):
                    if n == 2:
                        if failure == 'transport':
                            return None
                        response = support.completed(response_id='failed', input_tokens=9000)
                        response['status'] = 'failed'
                        response['error'] = {'code': 'fixture_failure', 'message': 'synthetic'}
                        if failure == 'unpriced_terminal':
                            del response['usage']
                        return 200, response
                    return 200, support.completed(response_id=f'ok-{n}', input_tokens=1000)

                fixture = self.fixture(primary)
                options = ['--review-every-requests', '100',
                           '--review-min-primary-tokens', '1000000',
                           '--primary-context-tokens', '1000000']
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                history = [{'role': 'user', 'content': 'population tracer'}]
                self.assertEqual(proxy.send(history)[0], 200)
                first = proxy.saved()
                failed_status, _ = proxy.send(history)
                self.assertEqual(failed_status, 502 if failure == 'transport' else 200)
                self.assertEqual(proxy.send(history)[0], 200)
                saved = proxy.saved()
                stats = proxy.request('GET', '/carry/dashboard/stats')[1]
                proxy.close()
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                restarted = proxy.request('GET', '/carry/dashboard/stats')[1]
                self.evidence[failure] = dict(first=first, saved=saved, stats=stats,
                                              restarted=restarted, wires=fixture.primaries)
                # Finish wire, lifecycle and immutable paid-money controls before
                # grading comparison population. Unknown failed spend stays unknown.
                self.assertEqual(len(fixture.primaries), 3)
                self.assertEqual(fixture.reviews, [])
                self.assertTrue(all(b['input'] == history for b in fixture.primaries))
                self.assertEqual(saved['completed_requests'], 2)
                self.assertEqual(saved['failed_primaries'], 1)
                self.assertEqual(saved['primary']['calls'], 3)
                self.assertEqual(saved['primary']['unavailable_cost_calls'],
                                 int(failure != 'priced_terminal'))
                successful_cost = 2 * first['primary']['cost_usd']
                if failure == 'priced_terminal':
                    self.assertGreater(saved['primary']['cost_usd'], successful_cost)
                else:
                    self.assertAlmostEqual(saved['primary']['cost_usd'], successful_cost, places=12)
                self.assertEqual(proxy.saved()['primary'], saved['primary'])
                self.assertEqual(proxy.saved()['shadow'], saved['shadow'])
                self.assertEqual(restarted['sessions'][0]['primary'], stats['sessions'][0]['primary'])
                self.assertEqual(restarted['cost_estimate'], stats['cost_estimate'])
                for snapshot in (stats, restarted):
                    row = snapshot['sessions'][0]['cost_estimate']
                    self.assertEqual(row['coverage']['primary_responses'], 2)
                    self.assertEqual(row['coverage']['failed_calls'], 1)
                    self.assertEqual(row['matched_primary_responses'], 2)
                    self.assertAlmostEqual(row['carry_cost'], successful_cost, places=12)
                    for estimate in (row, snapshot['cost_estimate']):
                        whole = estimate['whole_window']
                        self.assertIsInstance(whole, dict,
                                              'a failed attempt must not censor successful money')
                        self.assertEqual(whole['primary_responses'], 2)
                        self.assertAlmostEqual(whole['carry_cost'], successful_cost, places=12)
                        self.assertAlmostEqual(whole['traditional_cost'], successful_cost, places=12)
                        self.assertAlmostEqual(whole['savings'], 0, places=12)
                    self.assertEqual(row['quantity_evidence']['comparison_scope'],
                                     'matched_completed_primary_window')
    def test_reviewer_failures_are_symmetric_but_invalid_completed_advice_is_paid(self):
        for terminal in ('priced_failed', 'unknown_failed', 'invalid_completed'):
            with self.subTest(terminal=terminal):
                state = self.state / terminal
                state.mkdir()
                response = support.completed(input_tokens=4000)
                if terminal != 'invalid_completed':
                    response['status'] = 'failed'
                    response['error'] = {'code': 'synthetic', 'message': 'fixture'}
                if terminal == 'unknown_failed':
                    del response['usage']
                fixture = self.fixture(lambda _b, _n: (200, support.completed(input_tokens=1000)),
                                       lambda _b, _n: (200, response))
                options = ['--review-replayed-history', '--review-every-requests', '100',
                           '--primary-context-tokens', '1000000']
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                history = [{'role':'user','content':'goal'},
                           {'role':'assistant','content':'reviewable evidence ' * 500},
                           {'role':'user','content':'continue'}]
                self.assertEqual(proxy.send(history)[0], 200)
                saved = proxy.saved()
                before = proxy.request('GET', '/carry/dashboard/stats')[1]
                self.assertEqual(len(fixture.reviews), 1)
                self.assertEqual(saved['shadow']['calls'], 1)
                self.assertEqual(saved['shadow']['unavailable_cost_calls'], int(terminal == 'unknown_failed'))
                self.assertEqual(saved['shadow']['cost_usd'] > 0, terminal != 'unknown_failed')
                estimate = before['sessions'][0]['cost_estimate']
                self.assertEqual(estimate['coverage']['reviewer_responses'], int(terminal == 'invalid_completed'))
                self.assertEqual(estimate['coverage']['failed_calls'], int(terminal != 'invalid_completed'))
                expected = saved['primary']['cost_usd'] + (saved['shadow']['cost_usd'] if terminal == 'invalid_completed' else 0)
                self.assertAlmostEqual(estimate['whole_window']['carry_cost'], expected, places=12)
                self.assertAlmostEqual(estimate['whole_window']['traditional_cost'], saved['primary']['cost_usd'], places=12)
                proxy.close()
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                after = proxy.request('GET', '/carry/dashboard/stats')[1]
                self.evidence[terminal] = dict(saved=saved, before=before, after=after)
                self.assertEqual(after['cost_estimate'], before['cost_estimate'])
                self.assertEqual(proxy.saved()['shadow'], saved['shadow'])
                self.assertEqual(len(fixture.reviews), 1)

    def test_successful_unknown_money_zero_and_empty_are_distinct(self):
        for receipt in ('known_zero', 'missing_usage', 'unknown_rate'):
            with self.subTest(receipt=receipt):
                state = self.state / receipt
                state.mkdir()
                response = support.completed(input_tokens=0)
                response['usage']['output_tokens'] = 0
                if receipt == 'missing_usage':
                    del response['usage']
                fixture = self.fixture(lambda _body, _n: (200, response))
                proxy = support.Proxy(state, fixture, ['--review-every-requests', '100'])
                self.stack.callback(proxy.close)
                empty = proxy.request('GET', '/carry/dashboard/stats')[1]
                self.assertEqual(empty['sessions'], [])
                self.assertIsNone(empty['cost_estimate']['whole_window'])
                fields = {'model': 'fixture-unknown-tariff'} if receipt == 'unknown_rate' else {}
                self.assertEqual(proxy.send([{'role': 'user', 'content': receipt}], **fields)[0], 200)
                saved = proxy.saved()
                stats = proxy.request('GET', '/carry/dashboard/stats')[1]
                self.evidence[receipt] = dict(saved=saved, stats=stats)
                self.assertEqual(saved['completed_requests'], 1)
                self.assertEqual(saved['failed_primaries'], 0)
                self.assertEqual(saved['primary']['calls'], 1)
                self.assertEqual(saved['primary']['cost_usd'], 0)
                self.assertEqual(fixture.reviews, [])
                self.assertEqual(saved['primary']['unavailable_cost_calls'], int(receipt != 'known_zero'))
                for estimate in (stats['sessions'][0]['cost_estimate'], stats['cost_estimate']):
                    whole = estimate['whole_window']
                    if receipt == 'known_zero':
                        self.assertEqual(whole['carry_cost'], 0)
                        self.assertEqual(whole['traditional_cost'], 0)
                        self.assertEqual(whole['primary_responses'], 1)
                        self.assertIsNone(whole['savings_percent'])
                    else:
                        self.assertIsNone(whole, 'successful unknown money must stay unknown')

    def test_settled_actual_money_is_not_hypothetical_modality_eligibility(self):
        fixture = self.fixture(lambda _body, _n: (200, support.completed(input_tokens=1000)))
        proxy = support.Proxy(self.state, fixture, ['--review-every-requests', '100',
                              '--primary-context-tokens', '1000000'])
        self.stack.callback(proxy.close)
        history = [{'role': 'user', 'content': [{'type': 'input_audio', 'data': 'synthetic'}]}]
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        stats = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(saved=saved, stats=stats, wires=fixture.primaries)
        self.assertEqual(fixture.primaries[0]['input'], history)
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(saved['primary']['calls'], 1)
        self.assertEqual(saved['primary']['unavailable_cost_calls'], 0)
        self.assertGreater(saved['primary']['cost_usd'], 0)
        estimate = stats['sessions'][0]['cost_estimate']
        self.assertEqual(estimate['matched_primary_responses'], 0,
                         'unsupported hypothetical modality remains unmodeled')
        self.assertEqual(estimate['coverage']['unmodeled_calls'], 1)
        self.assertEqual(estimate['observed_primary_cost'], saved['primary']['cost_usd'],
                         'settlement, not hypothetical support, owns actual money')
        self.assertEqual(estimate['unavailable_primary_cost_calls'], 0)

    def test_native_summary_success_retains_settled_money_without_forecast_support(self):
        fixture = self.fixture(lambda _body, _n: (200, support.completed(
            [{'type':'compaction','encrypted_content':'native-summary-fixture'}], input_tokens=2000)))
        options = ['--review-every-requests', '100', '--review-min-primary-tokens', '1000000']
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        history = [{'role':'user','content':'native summary evidence'}, {'type':'compaction_trigger'}]
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        before = proxy.request('GET', '/carry/dashboard/stats')[1]
        estimate = before['sessions'][0]['cost_estimate']
        self.evidence.update(saved=saved, before=before, wires=fixture.primaries)
        self.assertEqual(saved['primary']['calls'], 1)
        self.assertEqual(saved['primary']['unavailable_cost_calls'], 0)
        self.assertEqual(estimate['coverage']['primary_responses'], 1)
        self.assertEqual(estimate['coverage']['unmodeled_calls'], 1)
        self.assertEqual(estimate['matched_primary_responses'], 0)
        self.assertEqual(estimate['observed_primary_cost'], saved['primary']['cost_usd'])
        self.assertEqual(estimate['whole_window']['carry_cost'], saved['primary']['cost_usd'])
        self.assertEqual(estimate['whole_window']['inferred_primary_responses'], 1)
        self.assertEqual(fixture.reviews, [])
        proxy.close()
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        after = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(saved=saved, before=before, after=after, wires=fixture.primaries)
        self.assertEqual(before['cost_estimate'], after['cost_estimate'])
        self.assertEqual(saved['primary'], proxy.saved()['primary'])
        self.assertEqual(len(fixture.primaries), 1)

    def test_inherited_reasoning_compacts_whole_cohorts_then_continues_after_restart(self):
        def primary(body, n):
            count = sum(item.get('type') == 'reasoning' for item in body['input'])
            return 200, support.completed(response_id=f'cohort-{n}', input_tokens={2:12000, 1:6000, 0:1000}[count])
        def review(_body, n):
            return 200, support.advice_response({'protected':[1], 'removable':[2], 'memories':[]})
        fixture = self.fixture(primary, review)
        options = ['--review-replayed-history', '--review-every-requests', '100', '--payoff-requests', '1', '--classifier-cache-policy', 'disabled',
                   '--primary-context-tokens', '1000000']
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        history = [{'role':'user','content':'protected task'}]
        for identifier in ('a','b'):
            history.extend([{'type':'reasoning','encrypted_content':f'opaque-{identifier}'},
                            {'type':'function_call','call_id':identifier,'name':'inspect','arguments':'{}'},
                            {'type':'function_call_output','call_id':identifier,'output':'evidence'}])
        history.append({'role':'user','content':'continue'})
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(fixture.primaries[0]['input'], history, 'cold opaque input cannot invent a measurement')
        self.assertEqual(proxy.send(history)[0], 200)
        first = proxy.saved()
        self.evidence.update(first=first, wires=fixture.primaries, review_wires=fixture.reviews)
        self.assertEqual(fixture.primaries[1]['input'], [history[0], {'role':'user','content':'[Earlier conversation omitted by Carry; retained messages remain in original order.]'}, *history[4:]], 'the first whole reviewed tool cohort is omitted with the disclosed gap marker')
        self.assertIsNone(first['last_plan']['candidates'][0]['primary_retained_required_tokens'], 'economic allocations are not capacity proof')
        proxy.close()
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        second = proxy.saved()
        self.assertEqual(fixture.primaries[2]['input'], fixture.primaries[1]['input'], 'filtered native evidence survives restart without resurrecting the omitted cohort or dropping unreviewed source')
        self.assertEqual(proxy.send(history)[0], 200)
        stats = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(first=first, second=second, stats=stats, wires=fixture.primaries, review_wires=fixture.reviews)
        self.assertEqual(fixture.primaries[3]['input'], fixture.primaries[2]['input'])
        self.assertEqual(stats['sessions'][0]['primary']['input_tokens'], 30000)
        self.assertEqual(stats['sessions'][0]['cost_estimate']['coverage']['primary_responses'], 4)
        self.assertIn(history[0], fixture.primaries[-1]['input'])

    def test_request_evidence_does_not_persist_arbitrary_reviewer_receipt_payload(self):
        sentinel = 'PRIVATE-REVIEW-RECEIPT-PAYLOAD'
        response = support.advice_response({'protected':[1], 'removable':[2], 'memories':[]})
        response['usage']['input_tokens_details']['provider_payload'] = {'raw_text': sentinel}
        response['usage']['output_tokens_details'] = {'provider_payload': sentinel}
        fixture = self.fixture(lambda _body, _n: (200, support.completed(input_tokens=1000)),
                               lambda _body, _n: (200, response))
        options = ['--review-replayed-history', '--review-every-requests', '100',
                   '--primary-context-tokens', '1000000']
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        history = [{'role':'user','content':'goal'}, {'role':'assistant','content':'evidence '*500},
                   {'role':'user','content':'continue'}]
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        stats = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(saved=saved, stats=stats)
        self.assertEqual(saved['shadow']['calls'], 1)
        self.assertGreater(saved['shadow']['cost_usd'], 0)
        self.assertNotIn(sentinel, json.dumps(saved['request_evidence']))
        estimate = stats['sessions'][0]['cost_estimate']
        self.assertEqual(estimate['observed_reviewer_cost'], saved['shadow']['cost_usd'])
        self.assertEqual(estimate['coverage']['reviewer_responses'], 1)

    def test_blocking_no_fit_reviewer_receipt_is_numeric_and_restart_stable(self):
        sentinel = 'PRIVATE-BLOCKING-REVIEW-RECEIPT'
        response = support.advice_response({'protected':[], 'removable':[2], 'memories':[]})
        response['usage']['cached_input_tokens'] = 0
        response['usage']['input_tokens_details'].update(
            provider_payload={'raw_text':sentinel}, audio_tokens=0)
        response['usage']['output_tokens_details'] = {'provider_payload':sentinel, 'audio_tokens':0}
        history = [{'role':'user','content':'goal'},
                   {'role':'assistant','content':'bulky output ' * 12000},
                   {'role':'user','content':'current task'}]
        fixture = self.fixture(lambda body, n: (200, support.completed(response_id=f'privacy-{n}',
            input_tokens=60000 if history[1] in body['input'] else 100)),
            lambda _body, _n: (200, response))
        options = ['--review-every-requests','100','--classifier-cache-policy','disabled']
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(fixture.reviews, [])
        proxy.close()
        options += ['--primary-context-tokens','50000','--primary-output-reserve-tokens','1',
                    '--context-margin-percent','0']
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        before = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(saved=saved, before=before, wires=fixture.primaries, reviews=fixture.reviews)
        self.assertEqual(len(fixture.reviews), 1)
        self.assertEqual(saved['shadow']['calls'], 1)
        self.assertGreater(saved['shadow']['cost_usd'], 0)
        self.assertNotIn(history[1], fixture.primaries[-1]['input'], 'blocking review actually applies')
        self.assertNotIn(sentinel, json.dumps(saved['request_evidence']))
        reviewer = next(o['Reviewer'] for o in saved['request_evidence']['observations'] if 'Reviewer' in o)
        self.assertEqual(reviewer['usage']['cached_input_tokens'], 0)
        self.assertEqual(reviewer['usage']['input_tokens_details'], {'cached_tokens':0, 'audio_tokens':0})
        proxy.close()
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        after = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(after=after)
        self.assertEqual(before['cost_estimate'], after['cost_estimate'])
        self.assertEqual(saved['shadow'], proxy.saved()['shadow'])
        self.assertEqual(len(fixture.reviews), 1)
        self.assertNotIn(sentinel, json.dumps(proxy.saved()['request_evidence']))

    def test_stats_derive_comparison_from_evidence_not_saved_predictions(self):
        fixture = self.fixture(lambda _body, _n: (200, support.completed(input_tokens=1000)))
        options = ['--review-every-requests', '100', '--primary-context-tokens', '1000000']
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send([{'role': 'user', 'content': 'evidence replay'}])[0], 200)
        before = proxy.request('GET', '/carry/dashboard/stats')[1]
        paid = proxy.saved()['primary']
        proxy.close()
        index_path = self.state / 'history.index'
        index = json.loads(index_path.read_text())
        mirror = next(self.state.glob('*.json'))
        staged = json.loads(mirror.read_text())
        for saved in (staged, *index['snapshots'].values()):
            owner = saved.get('request_evidence', saved.get('cost_trajectory'))
            # Obsolete predictions are hostile diagnostic data, not settlement.
            owner['result'] = dict(owner.get('result', {}), traditional_cost=12345,
                                   carry_cost=9000, summary_cost=100)
        mirror.write_text(json.dumps(staged))
        index_path.write_text(json.dumps(index))
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        after = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(before=before, after=after, paid=paid)
        self.assertEqual(after['sessions'][0]['primary'], paid)
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(after['sessions'][0]['cost_estimate'], before['sessions'][0]['cost_estimate'],
                         'stats must replay shared request evidence, not saved prediction totals')
        self.assertEqual(after['cost_estimate'], before['cost_estimate'])

    def test_absent_success_scope_is_not_reconstructed_from_all_attempt_money(self):
        for missing in ('totals', 'trajectory'):
            with self.subTest(missing=missing):
                state = self.state / missing
                state.mkdir()
                fixture = self.fixture(lambda _body, _n: (200, support.completed(input_tokens=1000)))
                options = ['--review-every-requests', '100', '--primary-context-tokens', '1000000']
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                history = [{'role': 'user', 'content': 'legacy scope tracer'}]
                self.assertEqual(proxy.send(history)[0], 200)
                frozen = proxy.saved()
                proxy.close()
                # Stage genuinely absent success-scoped fields in both authoritative
                # shared snapshot and diagnostic mirror, with the fixture process stopped.
                index_path = state / 'history.index'
                index = json.loads(index_path.read_text())
                mirror = next(state.glob('*.json'))
                staged = json.loads(mirror.read_text())
                for saved in (staged, *index['snapshots'].values()):
                    if missing == 'trajectory':
                        saved.pop('request_evidence', None)
                    else:
                        for observation in saved['request_evidence']['observations']:
                            if 'Primary' in observation:
                                observation['Primary']['facts']['cost'] = None
                mirror.write_text(json.dumps(staged))
                index_path.write_text(json.dumps(index))
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                stats = proxy.request('GET', '/carry/dashboard/stats')[1]
                self.evidence[missing] = dict(frozen=frozen, staged=staged, stats=stats)
                self.assertEqual(stats['sessions'][0]['primary']['cost_usd'], frozen['primary']['cost_usd'])
                self.assertEqual(stats['sessions'][0]['primary']['calls'], 1)
                self.assertEqual(len(fixture.primaries), 1)
                self.assertEqual(fixture.reviews, [])
                self.assertIsNone(stats['sessions'][0]['cost_estimate']['whole_window'],
                                  'missing population evidence is unknown, not all-attempt fallback')
                self.assertIsNone(stats['cost_estimate']['whole_window'])

if __name__ == '__main__':
    unittest.main()
