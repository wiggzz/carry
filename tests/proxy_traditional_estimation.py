"""Source-bound traditional accounting: real CLI/HTTP, synthetic receipts only.

CARRY_TEST_BINARY=/absolute/carry python3 -B tests/proxy_traditional_estimation.py -v
All traffic is loopback. No production traces, provider credentials or payments.
"""
import json
import os
from pathlib import Path
import unittest
import proxy_state_regressions as support


class TraditionalEstimation(unittest.TestCase):
    setUp = support.StateRegressions.setUp
    def record_evidence(self):
        support.StateRegressions.record_evidence(self)
        # Reconcile old subset assertions explicitly: they remain strict evidence
        # controls, but every frozen known-money HTTP window also has this NEW
        # full denominator. Includes invalid partial attribution, unknown output,
        # successful gaps, paid summary/cache continuity and their restarts.
        def check(value):
            if isinstance(value, dict):
                if isinstance(value.get('sessions'), list) and 'cost_estimate' in value:
                    windows = []
                    incomplete = bool(value.get('unavailable_sessions'))
                    for session in value['sessions']:
                        p, r = session['primary'], session['shadow']
                        whole = session['cost_estimate']['whole_window']
                        unknown = p['unavailable_cost_calls'] or r['unavailable_cost_calls']
                        if unknown:
                            self.assertIsNone(whole)
                            incomplete = True
                        elif p['calls'] or r['calls']:
                            self.assertIsInstance(whole, dict)
                            self.assertEqual(whole['primary_responses'], p['calls'])
                            self.assertAlmostEqual(whole['carry_cost'], p['cost_usd'] + r['cost_usd'], places=12)
                            self.assertAlmostEqual(whole['savings'], whole['traditional_cost'] - whole['carry_cost'], places=12)
                            if whole['traditional_cost']:
                                self.assertAlmostEqual(whole['savings_percent'], 100 * whole['savings'] / whole['traditional_cost'], places=10)
                            else:
                                self.assertIsNone(whole['savings_percent'])
                            windows.append(whole)
                    total = value['cost_estimate']['whole_window']
                    if incomplete:
                        self.assertIsNone(total)
                    elif windows:
                        for field in ['carry_cost','traditional_cost','savings']:
                            self.assertAlmostEqual(total[field], sum(w[field] for w in windows), places=12)
                for child in value.values(): check(child)
            elif isinstance(value, list):
                for child in value: check(child)
        check(self.evidence)
    fixture = support.StateRegressions.fixture
    proxy = support.StateRegressions.proxy

    def snapshot(self, proxy):
        # Join immutable checkpoint evidence with its real HTTP projection for
        # assertions/captures only. No persisted prediction state is recreated.
        saved = proxy.saved()
        status, stats = proxy.request('GET', '/carry/dashboard/stats')
        self.assertEqual(status, 200)
        self.assertEqual(len(stats['sessions']), 1)
        saved['estimate'] = stats['sessions'][0]['cost_estimate']
        return saved

    def test_reviewer_before_first_primary_mixed_poll_and_process_restart(self):
        import threading
        import time
        held, release = threading.Event(), threading.Event()
        self.stack.callback(release.set)
        def primary(_body, n):
            if n == 1:
                held.set()
                if not release.wait(20):
                    raise AssertionError('held first primary was not released')
            return 200, support.completed(response_id=f'first-primary-{n}', input_tokens=1000)
        fixture = self.fixture(primary)
        options = ['--review-replayed-history', '--review-every-requests', '100',
                   '--primary-context-tokens', '1024', '--primary-output-reserve-tokens', '1',
                   '--context-margin-percent', '0', '--request-timeout-secs', '25']
        proxy = self.proxy(fixture, options, wait_reviews=False)
        self.stack.callback(release.set)
        replies = []
        errors = []
        def send_first():
            try:
                replies.append(proxy.send([{'role':'user','content':'reviewer-only goal'},
                    {'role':'assistant','content':'reviewable source ' * 500},
                    {'role':'user','content':'continue'}]))
            except Exception as error:
                errors.append(type(error).__name__)
        worker = threading.Thread(target=send_first, daemon=True)
        worker.start()
        self.assertTrue(held.wait(8), 'actual first primary must reach held loopback transport')
        deadline = time.monotonic() + 5
        while True:
            before = proxy.request('GET', '/carry/dashboard/stats')[1]
            if before['sessions'] and before['sessions'][0]['shadow']['calls']:
                break
            self.assertLess(time.monotonic(), deadline, 'real reviewer must settle before first primary')
            time.sleep(.02)
        self.evidence['before_first_primary_stats'] = before
        row = before['sessions'][0]
        self.assertEqual(row['primary']['calls'], 0)
        self.assertEqual(row['primary']['cost_usd'], 0)
        self.assertTrue(row['processing'], 'pending primary activity remains visible without invented usage')
        self.assertEqual(row['shadow']['calls'], 1)
        reviewer_cost = row['shadow']['cost_usd']
        self.assertGreater(reviewer_cost, 0)
        only = row['cost_estimate']['whole_window']
        self.assertIsInstance(only, dict, 'settled reviewer-only expense must remain numeric')
        self.assertEqual(only['carry_cost'], reviewer_cost)
        self.assertEqual(only['traditional_cost'], 0)
        self.assertEqual(only['savings'], -reviewer_cost)
        self.assertIsNone(only['savings_percent'])
        self.assertEqual(before['cost_estimate']['whole_window']['carry_cost'], reviewer_cost)
        self.assertIsNone(before['cost_estimate']['whole_window']['savings_percent'])
        # A disjoint priced owner must not be censored by the held primary.
        import http.client
        connection = http.client.HTTPConnection(*proxy.address, timeout=5)
        connection.request('POST', '/v1/responses',
            body=support.wire({'model':'gpt-6-luna','input':[{'role':'user','content':'separate priced goal'}]}),
            headers={'Content-Type':'application/json','x-carry-session':'separate-priced-owner'})
        response = connection.getresponse()
        self.assertEqual(response.status, 200)
        response.read()
        connection.close()
        mixed = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence['mixed_stats'] = mixed
        self.assertEqual(len(mixed['sessions']), 2)
        total = mixed['cost_estimate']['whole_window']
        self.assertIsInstance(total, dict)
        self.assertGreater(total['traditional_cost'], 0)
        self.assertAlmostEqual(total['carry_cost'], sum(s[l]['cost_usd'] for s in mixed['sessions'] for l in ['primary','shadow']), places=12)
        for field in ['carry_cost','traditional_cost','savings']:
            self.assertAlmostEqual(total[field], sum(s['cost_estimate']['whole_window'][field] for s in mixed['sessions']), places=12)
        self.assertAlmostEqual(total['savings_percent'], 100 * total['savings'] / total['traditional_cost'], places=12)
        for _ in range(3):
            self.assertEqual(proxy.request('GET', '/carry/dashboard/stats')[1]['cost_estimate']['whole_window'], total)
        # Kill only this fixture process while its synthetic primary has no receipt.
        # No checkpoint money is edited; the real settled reviewer survives reload.
        proxy.close()
        release.set()
        worker.join(5)
        self.assertFalse(worker.is_alive())
        self.evidence.update(aborted_primary_replies=replies, aborted_primary_transport_errors=errors,
                             lifecycle='real reviewer settled; first primary held without receipt; fixture process killed')
        proxy = self.proxy(fixture, options, wait_reviews=False)
        restarted = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence['restarted_stats'] = restarted
        self.assertEqual(restarted['cost_estimate']['whole_window'], total)
        reviewer_only = next(s for s in restarted['sessions'] if s['primary']['calls'] == 0)
        self.assertEqual(reviewer_only['cost_estimate']['whole_window'], only)
        for _ in range(3):
            self.assertEqual(proxy.request('GET', '/carry/dashboard/stats')[1]['cost_estimate']['whole_window'], total)
        target = os.environ.get('CARRY_TRADITIONAL_STATS_PATH')
        if target:
            Path(target).write_text(json.dumps(restarted))
        html = os.environ.get('CARRY_TRADITIONAL_HTML_PATH')
        if html:
            import http.client
            connection = http.client.HTTPConnection(*proxy.address, timeout=5)
            connection.request('GET', '/carry/dashboard')
            response = connection.getresponse()
            self.assertEqual(response.status, 200)
            Path(html).write_bytes(response.read())
            connection.close()
        # Counterfactual uncertainty is not unknown actual money: an unpriced
        # completed third owner must still invalidate the full aggregate.
        connection = http.client.HTTPConnection(*proxy.address, timeout=5)
        connection.request('POST', '/v1/responses',
            body=support.wire({'model':'fixture-unknown-tariff','input':[{'role':'user','content':'unpriced control'}]}),
            headers={'Content-Type':'application/json','x-carry-session':'unpriced-owner'})
        response = connection.getresponse()
        self.assertEqual(response.status, 200)
        response.read()
        connection.close()
        unknown = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence['unknown_money_stats'] = unknown
        unpriced = next(s for s in unknown['sessions'] if s['primary']['unavailable_cost_calls'])
        self.assertIsNone(unpriced['cost_estimate']['whole_window'])
        self.assertIsNone(unknown['cost_estimate']['whole_window'])

    def test_whole_window_opaque_reviewer_expense_is_subtracted_once(self):
        output = [{'type':'reasoning','summary':[],'encrypted_content':'unresolved-opaque'}]
        fixture = self.fixture(lambda _b,n:(200,support.completed(output, f'reviewer-whole-{n}', input_tokens=1000)))
        proxy = self.proxy(fixture, ['--primary-context-tokens','1000000','--min-payback-percent','100'])
        history = [{'role':'user','content':'goal'}, {'role':'assistant','content':'source once ' + 's' * 8192}, {'role':'user','content':'continue'}]
        for _ in range(3): self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        saved = self.snapshot(proxy)
        stats = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(checkpoint=saved, stats=stats)
        self.assertGreater(saved['shadow']['cost_usd'], 0)
        e = stats['sessions'][0]['cost_estimate']
        w = e['whole_window']
        self.assertGreater(w['inferred_primary_responses'], 0)
        self.assertAlmostEqual(w['carry_cost'], saved['primary']['cost_usd'] + saved['shadow']['cost_usd'], places=12)
        self.assertAlmostEqual(w['savings'], w['traditional_cost'] - saved['primary']['cost_usd'] - saved['shadow']['cost_usd'], places=12)
        proxy.close()
        proxy = self.proxy(fixture, ['--primary-context-tokens','1000000','--min-payback-percent','100'])
        restarted = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence['restarted_stats'] = restarted
        self.assertEqual(restarted['sessions'][0]['cost_estimate']['whole_window'], w)

    def test_whole_window_includes_uncalibrated_historical_opaque_cost_after_restart(self):
        output = [{'type':'reasoning','summary':[],'encrypted_content':'no-calibration'}]
        fixture = self.fixture(lambda _b,n:(200,support.completed(output if n == 1 else [], f'whole-{n}', input_tokens=1000)))
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy = self.proxy(fixture, options)
        history = [{'role':'user','content':'goal'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        first_stats = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence['no_calibration_stats'] = first_stats
        first = first_stats['sessions'][0]['cost_estimate']
        self.assertEqual(first['matched_primary_responses'], 0)
        self.assertEqual(first['whole_window']['inferred_primary_responses'], 1)
        self.assertIn('no_reference_neutral', first['whole_window']['assumption'])
        before = self.snapshot(proxy)
        proxy.close()
        # This is a genuine checkpoint from an uncalibrated opaque response,
        # not an edited count or fabricated paid ledger.
        self.evidence['historical_checkpoint'] = before
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history + output, max_output_tokens=1)[0], 200)
        after = self.snapshot(proxy)
        stats = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(after_restart=after, stats=stats)
        estimate = stats['sessions'][0]['cost_estimate']
        whole = estimate.get('whole_window')
        self.assertIsInstance(whole, dict, 'producer must estimate the entire known-cost window, including previously omitted opaque calls')
        actual = after['primary']['cost_usd'] + after['shadow']['cost_usd']
        self.assertAlmostEqual(whole['carry_cost'], actual, places=12)
        self.assertEqual(whole['primary_responses'], 2)
        self.assertEqual(whole['inferred_primary_responses'], 1)
        self.assertGreater(whole['traditional_cost'], estimate['traditional_cost'], 'old omitted call must contribute actual modeled dollars, not just a caption')
        self.assertAlmostEqual(whole['traditional_cost'], 2 * estimate['traditional_cost'], places=12)
        self.assertAlmostEqual(whole['savings'], whole['traditional_cost'] - actual, places=12)
        self.assertAlmostEqual(whole['savings_percent'], 100 * whole['savings'] / whole['traditional_cost'], places=12)
        aggregate = stats['cost_estimate']['whole_window']
        for field in ['carry_cost','traditional_cost','savings','savings_percent']:
            self.assertAlmostEqual(aggregate[field], whole[field], places=12)
        target = os.environ.get('CARRY_TRADITIONAL_STATS_PATH')
        if target:
            Path(target).write_text(json.dumps(stats, indent=2) + '\n')
        # Exercise the renderer bytes embedded in this exact archived native
        # executable, not only a same-named source checkout.
        import http.client
        connection = http.client.HTTPConnection(*proxy.address, timeout=10)
        connection.request('GET', '/carry/dashboard')
        response = connection.getresponse()
        delivered = response.read()
        self.assertEqual(response.status, 200)
        connection.close()
        self.assertEqual(delivered, (Path(__file__).resolve().parents[1] / 'src/proxy_dashboard.html').read_bytes())
        html_target = os.environ.get('CARRY_DASHBOARD_HTML_PATH')
        if html_target: Path(html_target).write_bytes(delivered)

    def test_pending_member_remainder_recovers_prior_summary_through_http_restart(self):
        output = {'type':'reasoning','summary':[],'encrypted_content':'member-a'}
        caller = {'type':'reasoning','summary':[],'encrypted_content':'member-b'}
        # 28 local additive section units, checked below against the real native
        # allocator. This is fixture input, not an independent tokenizer model.
        fixture = self.fixture(lambda _b,n:(200,support.completed(
            [output] if n == 1 else [], f'member-{n}', input_tokens=1000 if n == 1 else 6829)))
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy = self.proxy(fixture, options)
        history = [{'role':'user','content':'initial below threshold'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        saved = self.snapshot(proxy)
        proxy.close()
        support.stage_comparison_scenario(saved, {'context_window':4000,'reserve':100,'keep_recent':500,'summary':100})
        frozen = saved['estimate'].copy()
        next(self.state.glob('*.json')).write_text(json.dumps(saved))
        proxy = self.proxy(fixture, options)
        history += [output, caller, {'role':'user','content':'caller text'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        after = self.snapshot(proxy)
        self.evidence.update(staged_pending=saved, after_restart=after,
                             stats=proxy.request('GET','/carry/dashboard/stats')[1])
        allocation = after['primary_input']['blocks'][-1]['allocation']
        self.assertEqual(allocation['residual'], 5801)
        self.assertEqual(allocation['opaque_items'], 2)
        t = after['estimate']
        self.assertEqual(t['traditional_compactions'], 1)
        self.assertEqual(t['initial_compactions'], 0)
        self.assertIsNone(frozen['summary_cost'], 'unmatched historical output is unavailable, not zero')
        self.assertEqual(t['summary_cost'], 0, 'no excluded historical fee backfill')
        self.assertEqual(t['matched_primary_responses'], 1)
        rates = after['model_resolution']['primary']['rates']
        expected = (3528*rates['write'] + rates['output'])/1000000
        self.assertIsNone(frozen['traditional_cost'])
        self.assertAlmostEqual(t['traditional_cost'], expected, places=12)
        self.assertAlmostEqual(t['observed_primary_cost'], after['primary']['cost_usd'], places=12)

    def test_single_gap_checkpoint_cannot_skip_config_only_success_after_http_restart(self):
        # Persisted-state seam: stage ONLY a single-gap capability from actual
        # cursor/configuration bytes. Native tests separately produce the gap
        # via unknown projection. No historical monetary fields are rewritten.
        fixture = self.fixture(lambda _b,n:(200,support.completed(
            [{'type':'message','role':'assistant','content':[{'type':'output_text','text':'word '*1000}]}] if n == 4 else [],
            f'single-gap-{n}', input_tokens=[1000,5000,7000,7000,7000][n-1])))
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy = self.proxy(fixture, options)
        history = [{'role':'user','content':'initial'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        saved = self.snapshot(proxy)
        proxy.close()
        support.stage_comparison_scenario(saved, {'context_window':4000,'reserve':100,'keep_recent':1500,'summary':100})
        next(self.state.glob('*.json')).write_text(json.dumps(saved))
        proxy = self.proxy(fixture, options)
        history.append({'role':'user','content':'paid summary'})
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        history.append({'role':'user','content':'gap input'})
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        saved = self.snapshot(proxy)
        proxy.close()
        self.assertEqual(saved['estimate']['traditional_compactions'], 1)
        # Explicit missing projection fact on a real successful dispatch. A
        # later config-only success cannot recover this discarded chronology.
        gap = saved['request_evidence']['observations'][-1]['Primary']
        gap['restoration'] = None
        gap['edited'] = True
        next(self.state.glob('*.json')).write_text(json.dumps(saved))
        self.evidence['staged_single_gap'] = saved
        proxy = self.proxy(fixture, options)
        frozen = self.snapshot(proxy)['estimate']
        matched = frozen['matched_primary_responses']
        self.assertEqual(proxy.send(history, max_output_tokens=1, temperature=0.5)[0], 200)
        intervening = self.snapshot(proxy); self.evidence['intervening_success'] = intervening
        self.assertIsNone(intervening['request_evidence']['observations'][-2]['Primary']['restoration'])
        proxy.close(); proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        after = self.snapshot(proxy); self.evidence['after_restart'] = after
        t = after['estimate']
        self.assertIsNone(after['request_evidence']['observations'][-3]['Primary']['restoration'],
                          'never resurrect missing projection evidence across successful transitions')
        self.assertEqual(t['matched_primary_responses'], matched)
        for field in ['traditional_cost','summary_cost','carry_cost']:
            self.assertEqual(t[field], frozen[field])
        self.assertEqual(t['initial_compactions'], 0)
        self.assertAlmostEqual(t['observed_primary_cost'], after['primary']['cost_usd'], places=12)

    def test_partial_opaque_dispatch_prices_current_projection_after_restart(self):
        self.partial_case(False)

    def test_current_blocks_calibrate_retained_output_without_repricing_prior_money(self):
        self.partial_case(True)

    def partial_case(self, legacy):
        observations = []
        first_output = [{'type':'reasoning','summary':[],'encrypted_content':'first-opaque'},
                        {'type':'function_call','call_id':'first-call','name':'lookup','arguments':'{}'}]
        second_output = [{'type':'reasoning','summary':[],'encrypted_content':'second-opaque'},
                         {'type':'function_call','call_id':'second-call','name':'lookup','arguments':'{}'}]
        def primary(body, n):
            count = sum(item.get('type') == 'reasoning' for item in body['input'])
            return 200, support.completed(first_output if n == 1 else second_output if n == 2 else [{'type':'reasoning','summary':[],'encrypted_content':'new-output-not-billed-reasoning-units'}] if legacy else [], f'partial-{n}', input_tokens={0:1000, 1:3000, 2:5000}[count])

        def review(body, review_number):
            actionable = support.ledger(body)
            pending_target, removable = False, []
            for item in body['input']:
                if item.get('encrypted_content') == 'first-opaque':
                    pending_target = True
                content = item.get('content', '')
                text = content if isinstance(content, str) else ''.join(
                    p.get('text', '') for p in content if isinstance(p, dict)) if isinstance(content, list) else ''
                if text.startswith('[carry context] '):
                    annotation = json.loads(text[len('[carry context] '):])
                    if review_number >= 2 and pending_target and annotation['group_id'] in actionable['eligible_group_ids']:
                        removable.append(annotation['group_id'])
                    pending_target = False
            observations.append({'ledger':actionable, 'removable':removable})
            return 200, support.advice_response({'protected':[], 'removable':removable, 'memories':[]})

        fixture = self.fixture(primary, review)
        options = ['--primary-context-tokens','1000000']
        proxy = self.proxy(fixture, options)
        base = [{'role':'user','content':'Preserve this initial goal.'}]
        self.assertEqual(proxy.send(base, max_output_tokens=1)[0], 200)
        middle = base + first_output + [{'type':'function_call_output','call_id':'first-call','output':'Obsolete first result.'}, {'role':'user','content':'Second task.'}]
        self.assertEqual(proxy.send(middle, max_output_tokens=1)[0], 200)
        full = middle + second_output + [{'type':'function_call_output','call_id':'second-call','output':'Keep second result.'}, {'role':'user','content':'Continue the current task.'}]
        # Native-source review eligibility is established by a successful
        # primary; allow its normal subsequent review frontier to advance.
        for _ in range(3):
            if any(o['removable'] for o in observations):
                break
            self.assertEqual(proxy.send(full, max_output_tokens=1)[0], 200)
        trained = self.snapshot(proxy)
        training_calls = trained['estimate']['matched_primary_responses']
        self.evidence.update(review_observations=observations, trained=trained)
        self.assertTrue(any(o['removable'] for o in observations), observations)
        # Freeze reviewer cadence after accepting real advice. This exercises
        # ordinary projection economics without inventing a price for a changed
        # opaque REVIEW input (a distinct, unsupported planner estimate).
        proxy.close()
        # Current-format restart preserves the same native blocks and receipts;
        # the second arm adds an uncalibrated output, not an obsolete decoder.
        options += ['--review-every-requests','100','--review-min-new-tokens','1000000']
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(full, max_output_tokens=1)[0], 200)
        projected = self.snapshot(proxy)
        stats = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(review_observations=observations, trained=trained,
                             projected=projected, stats=stats,
                             upstream_inputs=[b['input'] for b in fixture.primaries])
        self.assertFalse(any(i.get('encrypted_content') == 'first-opaque' for i in fixture.primaries[-1]['input']))
        self.assertTrue(any(i.get('encrypted_content') == 'second-opaque' for i in fixture.primaries[-1]['input']))
        observation = projected['request_evidence']['observations'][-1]['Primary']
        self.assertGreater(observation['restoration']['tokens'], 0)
        self.assertFalse(observation['restoration']['measured'])
        self.assertEqual(projected['estimate']['matched_primary_responses'], training_calls + 1)
        self.assertGreaterEqual(projected['estimate']['coverage']['estimated_quantity_calls'], 1)
        prior = trained['request_evidence']['observations']
        self.assertEqual(projected['request_evidence']['observations'][:len(prior)], prior)
        evidence = stats['sessions'][0]['cost_estimate']['quantity_evidence']
        aggregate = stats['cost_estimate']['quantity_evidence']
        self.assertEqual(aggregate['comparison_scope'], 'matched_subset_not_whole_session')
        self.assertEqual(aggregate['total_primary_responses'], stats['sessions'][0]['cost_estimate']['coverage']['primary_responses'])
        self.assertEqual(aggregate['estimated_priced_calls'], evidence['estimated_priced_calls'])
        self.assertEqual(evidence['version'], 1)
        self.assertGreaterEqual(evidence['estimated_priced_calls'], 1)
        self.assertIn('no_calibrated_statistical_interval', evidence['uncertainty'])
        self.assertAlmostEqual(stats['sessions'][0]['cost_estimate']['observed_primary_cost'], projected['primary']['cost_usd'], places=12)
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(full, max_output_tokens=1)[0], 200)
        restarted = self.snapshot(proxy)
        after = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(restarted=restarted, after_restart_stats=after)
        self.assertEqual(restarted['request_evidence']['observations'][-1]['Primary']['restoration'], observation['restoration'], 'repeated omission replaces, never accumulates')
        self.assertEqual(restarted['estimate']['matched_primary_responses'], training_calls + 2)
        self.assertAlmostEqual(after['sessions'][0]['cost_estimate']['observed_primary_cost'], restarted['primary']['cost_usd'], places=12)
        target = os.environ.get('CARRY_TRADITIONAL_STATS_PATH')
        if target:
            Path(target).write_text(json.dumps(after, indent=2) + '\n')

    def test_opaque_continuous_summary_progresses_after_http_restart(self):
        inputs = [1000, 300000, 300100]
        fixture = self.fixture(lambda _b,n:(200,support.completed([],f'summary-{n}',input_tokens=inputs[n-1])))
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy = self.proxy(fixture, options)
        history = [{'role':'user','content':'Retain original goal.'},
                   {'type':'reasoning','summary':[],'encrypted_content':'initial-opaque'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0],200)
        history += [{'role':'user','content':'Large in-window growth.'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0],200)
        before = self.snapshot(proxy)
        self.evidence['before_restart'] = before
        self.assertEqual(before['estimate']['traditional_compactions'],1)
        proxy.close()
        proxy = self.proxy(fixture, options)
        history += [{'role':'user','content':'After restart.'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0],200)
        after = self.snapshot(proxy)
        stats = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(after_restart=after, stats=stats)
        t = after['estimate']
        self.assertEqual(t['matched_primary_responses'],3)
        self.assertEqual(t['initial_compactions'],0)
        self.assertEqual(t['traditional_compactions'],1)
        self.assertEqual(t['summary_cost'],before['estimate']['summary_cost'])
        self.assertGreater(t['summary_cost'],0)
        rates = after['model_resolution']['primary']['rates']
        self.assertAlmostEqual(t['traditional_cost'] - before['estimate']['traditional_cost'],
                               (21100*rates['write'] + rates['output'])/1e6, places=12)
        self.assertAlmostEqual(stats['sessions'][0]['cost_estimate']['observed_primary_cost'],after['primary']['cost_usd'],places=12)
        self.assertEqual(fixture.reviews,[])

    def test_unknown_output_crosses_summary_threshold_before_next_http_request(self):
        opaque={'type':'reasoning','summary':[],'encrypted_content':'unresolved-large-output'}
        fixture=self.fixture(lambda _b,n:(200,support.completed([opaque] if n==1 else [],f'cross-{n}',input_tokens=1000 if n==1 else 300000)))
        options=['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy=self.proxy(fixture,options)
        history=[{'role':'user','content':'below modeled threshold'}]
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        before=self.snapshot(proxy); self.evidence['before_restart']=before
        self.assertEqual(before['estimate']['initial_compactions'],0)
        proxy.close(); proxy=self.proxy(fixture,options)
        history.append(opaque)
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        after=self.snapshot(proxy); stats=proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(after_restart=after,stats=stats)
        t=after['estimate']
        self.assertEqual(t['traditional_compactions'],1)
        self.assertEqual(t['initial_compactions'],0)
        self.assertIsNone(before['estimate']['summary_cost'])
        self.assertEqual(t['summary_cost'], 0, 'excluded historical summary fee stays excluded')
        self.assertEqual(t['matched_primary_responses'],1)
        self.assertLess(t['traditional_cost'],0.1,'second primary uses post-summary input, never 300000')
        self.assertAlmostEqual(stats['sessions'][0]['cost_estimate']['observed_primary_cost'],after['primary']['cost_usd'],places=12)

    def test_unknown_output_preserves_divergent_cache_write_through_http_restart(self):
        opaque={'type':'reasoning','summary':[],'encrypted_content':'cache-unknown-output'}
        inputs=[1000,300000,300100,300200]
        fixture=self.fixture(lambda _b,n:(200,support.completed([opaque] if n==3 else [],f'cache-{n}',input_tokens=inputs[n-1])))
        options=['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy=self.proxy(fixture,options)
        history=[{'role':'user','content':'base'}]
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        history.append({'role':'user','content':'large paid summary growth'})
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        history.append({'role':'user','content':'known divergent request input'})
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        before=self.snapshot(proxy); self.evidence['before_restart']=before
        facts = before['request_evidence']['observations'][-1]['Primary']['facts']
        self.assertEqual(facts['native']['input_tokens'], 300100)
        self.assertIsNone(facts['retained'], 'opaque output remains unmeasured')
        proxy.close(); proxy=self.proxy(fixture,options)
        history.append(opaque)
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        after=self.snapshot(proxy); self.evidence.update(after_restart=after,stats=proxy.request('GET','/carry/dashboard/stats')[1])
        t=after['estimate']; prior=before['estimate']
        self.assertEqual(t['summary_cost'],prior['summary_cost'])
        self.assertEqual(t['matched_primary_responses'],prior['matched_primary_responses']+1)
        rates=after['model_resolution']['primary']['rates']
        self.assertEqual(after['primary']['output_tokens']-before['primary']['output_tokens'],1)
        expected=(21100*rates['cached']+100*rates['write']+rates['output'])/1000000
        self.assertAlmostEqual(t['traditional_cost']-prior['traditional_cost'],expected,places=12)
        self.assertAlmostEqual(t['observed_primary_cost'],after['primary']['cost_usd'],places=12)

    def test_missing_partial_projection_evidence_keeps_paid_summary_through_off_restart(self):
        self.evidence['advice_observations']=[]
        def primary(body,n):
            has_first=any(i.get('encrypted_content')=='initial-first' for i in body['input'])
            return 200,support.completed([],f'gap-{n}',input_tokens=1000 if n==1 else 300000 if has_first else 200000)
        def review(body,n):
            actionable=support.ledger(body); pending=False; removable=[]
            for item in body['input']:
                if item.get('encrypted_content')=='initial-first': pending=True
                content=item.get('content','')
                text=content if isinstance(content,str) else ''.join(p.get('text','') for p in content if isinstance(p,dict)) if isinstance(content,list) else ''
                if text.startswith('[carry context] '):
                    annotation=json.loads(text[len('[carry context] '):])
                    if pending and annotation['group_id'] in actionable['eligible_group_ids']: removable.append(annotation['group_id'])
                    pending=False
            self.evidence['advice_observations'].append({'ledger':actionable,'removable':removable})
            return 200,support.advice_response({'protected':[],'removable':removable,'memories':[]})
        fixture=self.fixture(primary,review)
        options=['--primary-context-tokens','1000000']
        proxy=self.proxy(fixture,options)
        history=[{'role':'user','content':'keep original goal'},
            {'type':'reasoning','summary':[],'encrypted_content':'initial-first'},
            {'type':'function_call','call_id':'old','name':'lookup','arguments':'{}'},
            {'type':'function_call_output','call_id':'old','output':'obsolete'},
            {'role':'user','content':'next task'},
            {'type':'reasoning','summary':[],'encrypted_content':'initial-second'},
            {'type':'function_call','call_id':'new','name':'lookup','arguments':'{}'},
            {'type':'function_call_output','call_id':'new','output':'current'},
            {'role':'user','content':'preserve current task'}]
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        history.append({'role':'user','content':'large in-window growth'})
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        paid=self.snapshot(proxy); self.evidence['paid_summary']=paid
        self.assertEqual(paid['estimate']['traditional_compactions'],1)
        proxy.close()
        options += ['--review-every-requests','100','--review-min-new-tokens','1000000']
        proxy=self.proxy(fixture,options)
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        gap=self.snapshot(proxy)
        self.assertTrue(gap['last_plan']['selected'])
        self.assertFalse(any(i.get('encrypted_content') == 'initial-first' for i in fixture.primaries[-1]['input']))
        self.assertEqual(gap['estimate']['summary_cost'],paid['estimate']['summary_cost'])
        proxy.close()
        # Current evidence-loss seam: discard ONLY the projected quantity for
        # this real selected/paid dispatch. No predictions, ledgers, native
        # receipts, chronology or paid-summary state are rewritten. Missing
        # evidence must not be inferred on restart from a later canonical wire.
        last = gap['request_evidence']['observations'][-1]['Primary']
        self.assertGreater(last['restoration']['tokens'], 0)
        last['restoration'] = None
        next(self.state.glob('*.json')).write_text(json.dumps(gap))
        self.evidence['missing_projection_checkpoint']=gap
        proxy=support.Proxy(self.state,fixture,['--primary-context-tokens','1000000','--review-every-requests','100','--review-min-new-tokens','1000000'],mode='off'); self.addCleanup(proxy.close)
        frozen = self.snapshot(proxy)['estimate']
        self.assertEqual(frozen['summary_cost'], paid['estimate']['summary_cost'])
        for suffix in ['off-mode recovery','later growth']:
            history.append({'role':'user','content':suffix})
            self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        after=self.snapshot(proxy);self.evidence.update(after_off_restart=after,stats=proxy.request('GET','/carry/dashboard/stats')[1])
        self.assertEqual(after['estimate']['initial_compactions'],0)
        self.assertEqual(after['estimate']['traditional_compactions'], paid['estimate']['traditional_compactions'])
        self.assertEqual(after['estimate']['summary_cost'],paid['estimate']['summary_cost'])
        self.assertEqual(after['estimate']['matched_primary_responses'], frozen['matched_primary_responses'])
        old = gap['request_evidence']['observations']
        self.assertEqual(after['request_evidence']['observations'][:len(old)], old)
        self.assertGreater(after['primary']['cost_usd'],gap['primary']['cost_usd'])

    def test_unknown_retained_output_keeps_known_free_initialization_once(self):
        opaque = {'type':'reasoning','summary':[],'encrypted_content':'unknown-initial-output'}
        fixture = self.fixture(lambda _b,n:(200,support.completed([opaque] if n == 1 else [],f'unknown-{n}',input_tokens=300000 if n == 1 else 300100)))
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy = self.proxy(fixture, options)
        history = [{'role':'user','content':'Already-large pre-window source.'}]
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        first = self.snapshot(proxy)
        self.evidence['before_restart'] = first
        self.assertEqual(first['estimate']['initial_compactions'],1,
                         'known native input permits one free pre-window initialization even when retained output is unknown')
        proxy.close()
        proxy = self.proxy(fixture, options)
        history += [opaque,{'role':'user','content':'Continue after unknown output.'}]
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        last = self.snapshot(proxy)
        self.evidence.update(after_restart=last, stats=proxy.request('GET','/carry/dashboard/stats')[1])
        self.assertEqual(last['estimate']['initial_compactions'],1)
        self.assertEqual(last['estimate']['traditional_compactions'],0)
        self.assertEqual(last['estimate']['summary_cost'],0)
        rates = last['model_resolution']['primary']['rates']
        self.assertAlmostEqual(last['estimate']['traditional_cost'],
                               (21000*rates['cached'] + 100*rates['write'] + rates['output'])/1e6, places=12)


if __name__ == '__main__':
    unittest.main()
