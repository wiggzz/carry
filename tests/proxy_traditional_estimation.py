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
    record_evidence = support.StateRegressions.record_evidence
    fixture = support.StateRegressions.fixture
    proxy = support.StateRegressions.proxy

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
        proxy.close()
        saved = proxy.saved()
        support.stage_comparison_scenario(saved, {'context_window':4000,'reserve':100,'keep_recent':500,'summary':100})
        frozen = saved['cost_trajectory']['result'].copy()
        next(self.state.glob('*.json')).write_text(json.dumps(saved))
        proxy = self.proxy(fixture, options)
        history += [output, caller, {'role':'user','content':'caller text'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        after = proxy.saved()
        self.evidence.update(staged_pending=saved, after_restart=after,
                             stats=proxy.request('GET','/carry/dashboard/stats')[1])
        allocation = after['primary_input']['blocks'][-1]['allocation']
        self.assertEqual(allocation['residual'], 5801)
        self.assertEqual(allocation['opaque_items'], 2)
        t = after['cost_trajectory']
        self.assertEqual(t['retired'], 3401, 'recover the 2901-unit member BEFORE pricing the next primary')
        self.assertEqual(t['last_traditional_input'], 3528)
        self.assertEqual(t['result']['traditional_compactions'], 1)
        self.assertEqual(t['result']['initial_compactions'], 0)
        self.assertEqual(t['result']['summary_cost'], frozen['summary_cost'], 'no excluded historical fee backfill')
        self.assertEqual(t['priced_primary_responses'], 1)
        rates = after['model_resolution']['primary']['rates']
        expected = (3528*rates['write'] + rates['output'])/1000000
        self.assertAlmostEqual(t['result']['traditional_cost']-frozen['traditional_cost'], expected, places=12)
        self.assertAlmostEqual(t['result']['observed_primary_cost'], after['primary']['cost_usd'], places=12)

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
        proxy.close()
        saved = proxy.saved()
        support.stage_comparison_scenario(saved, {'context_window':4000,'reserve':100,'keep_recent':1500,'summary':100})
        next(self.state.glob('*.json')).write_text(json.dumps(saved))
        proxy = self.proxy(fixture, options)
        history.append({'role':'user','content':'paid summary'})
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        history.append({'role':'user','content':'gap input'})
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        proxy.close()
        saved = proxy.saved(); t = saved['cost_trajectory']
        self.assertEqual(t['retired'], 3500)
        self.assertEqual(t['summary'], 100)
        t['unresolved_projection'] = True
        t['projection_recovery'] = {'configuration':t['receipts']['configuration'],
                                    'cursor':t['history_continuity']['cursor'].copy()}
        next(self.state.glob('*.json')).write_text(json.dumps(saved))
        self.evidence['staged_single_gap'] = saved
        frozen = t['result'].copy(); matched = t['priced_primary_responses']
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history, max_output_tokens=1, temperature=0.5)[0], 200)
        intervening = proxy.saved(); self.evidence['intervening_success'] = intervening
        self.assertTrue(intervening['cost_trajectory']['unresolved_projection'])
        proxy.close(); proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0], 200)
        after = proxy.saved(); self.evidence['after_restart'] = after
        t = after['cost_trajectory']
        self.assertTrue(t['unresolved_projection'], 'never resurrect a stale gap capability across successful response transitions')
        self.assertEqual(t['priced_primary_responses'], matched)
        for field in ['traditional_cost','summary_cost','carry_cost']:
            self.assertEqual(t['result'][field], frozen[field])
        self.assertEqual(t['result']['initial_compactions'], 0)
        self.assertAlmostEqual(t['result']['observed_primary_cost'], after['primary']['cost_usd'], places=12)

    def test_partial_opaque_dispatch_prices_current_projection_after_restart(self):
        self.partial_case(False)

    def test_legacy_blocks_calibrate_retained_output_without_repricing_prior_money(self):
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
        trained = proxy.saved()
        training_calls = trained['cost_trajectory']['priced_primary_responses']
        self.evidence.update(review_observations=observations, trained=trained)
        self.assertTrue(any(o['removable'] for o in observations), observations)
        # Freeze reviewer cadence after accepting real advice. This exercises
        # ordinary projection economics without inventing a price for a changed
        # opaque REVIEW input (a distinct, unsupported planner estimate).
        proxy.close()
        if legacy:
            # Exact old checkpoint representation: retain native quantities and
            # identities, remove ONLY fields that old binaries never wrote.
            saved = proxy.saved()
            for block in saved['primary_input']['blocks']:
                block.pop('allocation', None)
                block.pop('opaque', None)
            saved['cost_trajectory']['receipts'].pop('quantity_policy_version', None)
            saved['cost_trajectory'].pop('estimated_projection', None)
            saved['cost_trajectory']['result'].pop('quantity_policy_version', None)
            saved['cost_trajectory']['result'].pop('legacy_quantity_segment', None)
            for field in ('estimated_quantity_calls','measured_quantity_calls','unavailable_quantity_calls'):
                saved['cost_trajectory']['result']['coverage'].pop(field, None)
            self.assertFalse(saved.get('history_shared', False))
            next(self.state.glob('*.json')).write_text(json.dumps(saved))
        options += ['--review-every-requests','100','--review-min-new-tokens','1000000']
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(full, max_output_tokens=1)[0], 200)
        projected = proxy.saved()
        stats = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(review_observations=observations, trained=trained,
                             projected=projected, stats=stats,
                             upstream_inputs=[b['input'] for b in fixture.primaries])
        self.assertFalse(any(i.get('encrypted_content') == 'first-opaque' for i in fixture.primaries[-1]['input']))
        self.assertTrue(any(i.get('encrypted_content') == 'second-opaque' for i in fixture.primaries[-1]['input']))
        trajectory = projected['cost_trajectory']
        self.assertGreater(trajectory['restored'], 0)
        self.assertTrue(trajectory['projection_known'])
        self.assertEqual(trajectory['priced_primary_responses'], training_calls + 1)
        self.assertGreaterEqual(trajectory['result']['coverage']['estimated_quantity_calls'], 1)
        evidence = stats['sessions'][0]['cost_estimate']['quantity_evidence']
        aggregate = stats['cost_estimate']['quantity_evidence']
        self.assertEqual(aggregate['comparison_scope'], 'matched_subset_not_whole_session')
        self.assertEqual(aggregate['total_primary_responses'], stats['sessions'][0]['cost_estimate']['coverage']['primary_responses'])
        self.assertEqual(aggregate['estimated_priced_calls'], evidence['estimated_priced_calls'])
        if legacy:
            self.assertEqual(aggregate['historical_policy_scope'], 'mixed_prospective_only_no_repricing')
            self.assertEqual(aggregate['legacy_priced_calls'], training_calls)
        self.assertEqual(evidence['version'], 1)
        self.assertGreaterEqual(evidence['estimated_priced_calls'], 1)
        self.assertIn('no_calibrated_statistical_interval', evidence['uncertainty'])
        self.assertAlmostEqual(stats['sessions'][0]['cost_estimate']['observed_primary_cost'], projected['primary']['cost_usd'], places=12)
        if legacy:
            self.assertTrue(any(block.get('allocation') for block in projected['primary_input']['blocks']))
            frozen = trajectory['result']['legacy_quantity_segment']
            self.assertEqual(frozen['carry_cost'], trained['cost_trajectory']['result']['carry_cost'])
            self.assertEqual(frozen['traditional_cost'], trained['cost_trajectory']['result']['traditional_cost'])
            self.assertEqual(frozen['coverage']['estimated_quantity_calls'],0,
                             'absent historical quality facts stay unknown, not inferred')
            self.assertEqual(trajectory['result']['coverage']['estimated_quantity_calls'],1,
                             'only the prospective call receives new estimated coverage')
            return
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(full, max_output_tokens=1)[0], 200)
        restarted = proxy.saved()
        after = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(restarted=restarted, after_restart_stats=after)
        self.assertEqual(restarted['cost_trajectory']['restored'], trajectory['restored'], 'repeated omission replaces, never accumulates')
        self.assertEqual(restarted['cost_trajectory']['priced_primary_responses'], training_calls + 2)
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
        before = proxy.saved()
        self.evidence['before_restart'] = before
        self.assertEqual(before['cost_trajectory']['result']['traditional_compactions'],1)
        proxy.close()
        proxy = self.proxy(fixture, options)
        history += [{'role':'user','content':'After restart.'}]
        self.assertEqual(proxy.send(history, max_output_tokens=1)[0],200)
        after = proxy.saved()
        stats = proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(after_restart=after, stats=stats)
        t = after['cost_trajectory']
        self.assertEqual(t['priced_primary_responses'],3)
        self.assertEqual(t['result']['initial_compactions'],0)
        self.assertEqual(t['result']['traditional_compactions'],1)
        self.assertEqual(t['result']['summary_cost'],before['cost_trajectory']['result']['summary_cost'])
        self.assertGreater(t['result']['summary_cost'],0)
        self.assertEqual(t['last_traditional_input'],21100)
        self.assertAlmostEqual(stats['sessions'][0]['cost_estimate']['observed_primary_cost'],after['primary']['cost_usd'],places=12)
        self.assertEqual(fixture.reviews,[])

    def test_unknown_output_crosses_summary_threshold_before_next_http_request(self):
        opaque={'type':'reasoning','summary':[],'encrypted_content':'unresolved-large-output'}
        fixture=self.fixture(lambda _b,n:(200,support.completed([opaque] if n==1 else [],f'cross-{n}',input_tokens=1000 if n==1 else 300000)))
        options=['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy=self.proxy(fixture,options)
        history=[{'role':'user','content':'below modeled threshold'}]
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        before=proxy.saved(); self.evidence['before_restart']=before
        self.assertEqual(before['cost_trajectory']['result']['initial_compactions'],0)
        proxy.close(); proxy=self.proxy(fixture,options)
        history.append(opaque)
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        after=proxy.saved(); stats=proxy.request('GET','/carry/dashboard/stats')[1]
        self.evidence.update(after_restart=after,stats=stats)
        t=after['cost_trajectory']
        self.assertEqual(t['last_traditional_input'],21000)
        self.assertEqual(t['retired'],280000)
        self.assertEqual(t['result']['traditional_compactions'],1)
        self.assertEqual(t['result']['initial_compactions'],0)
        self.assertEqual(t['result']['summary_cost'],before['cost_trajectory']['result']['summary_cost'],'excluded historical summary fee stays excluded')
        self.assertEqual(t['priced_primary_responses'],1)
        self.assertLess(t['result']['traditional_cost'],0.1,'second primary uses post-summary input, never 300000')
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
        before=proxy.saved(); self.evidence['before_restart']=before
        cache=before['cost_trajectory']['receipts']['modeled_cache']
        self.assertTrue(cache['writes'],'unknown retained output cannot omit the known input cache write')
        self.assertEqual(cache['writes'][-1]['end'],21100)
        proxy.close(); proxy=self.proxy(fixture,options)
        history.append(opaque)
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        after=proxy.saved(); self.evidence.update(after_restart=after,stats=proxy.request('GET','/carry/dashboard/stats')[1])
        t=after['cost_trajectory']; prior=before['cost_trajectory']
        self.assertEqual(t['last_traditional_input'],21200)
        self.assertEqual(t['result']['summary_cost'],prior['result']['summary_cost'])
        self.assertEqual(t['priced_primary_responses'],prior['priced_primary_responses']+1)
        rates=after['model_resolution']['primary']['rates']
        self.assertEqual(after['primary']['output_tokens']-before['primary']['output_tokens'],1)
        expected=(21100*rates['cached']+100*rates['write']+rates['output'])/1000000
        self.assertAlmostEqual(t['result']['traditional_cost']-prior['result']['traditional_cost'],expected,places=12)
        self.assertAlmostEqual(t['result']['observed_primary_cost'],after['primary']['cost_usd'],places=12)

    def test_successful_unknown_partial_projection_keeps_paid_window_through_off_restart(self):
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
        paid=proxy.saved(); self.evidence['paid_summary']=paid
        self.assertEqual(paid['cost_trajectory']['result']['traditional_compactions'],1)
        proxy.close()
        options += ['--review-every-requests','100','--review-min-new-tokens','1000000']
        proxy=self.proxy(fixture,options)
        for _ in range(3):
            self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
            gap=proxy.saved()
            if not gap['cost_trajectory']['projection_known']: break
        self.evidence['successful_gap']=gap
        self.assertFalse(gap['cost_trajectory']['projection_known'],'real reviewer projection reaches quantity-unavailable path')
        self.assertEqual(gap['cost_trajectory']['retired'],paid['cost_trajectory']['retired'])
        self.assertEqual(gap['cost_trajectory']['result']['summary_cost'],paid['cost_trajectory']['result']['summary_cost'])
        proxy.close()
        proxy=support.Proxy(self.state,fixture,['--primary-context-tokens','1000000','--review-every-requests','100','--review-min-new-tokens','1000000'],mode='off'); self.addCleanup(proxy.close)
        for suffix in ['off-mode recovery','later growth']:
            history.append({'role':'user','content':suffix})
            self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        after=proxy.saved();self.evidence.update(after_off_restart=after,stats=proxy.request('GET','/carry/dashboard/stats')[1])
        self.assertEqual(after['cost_trajectory']['result']['initial_compactions'],0)
        self.assertGreaterEqual(after['cost_trajectory']['retired'],paid['cost_trajectory']['retired'])
        self.assertEqual(after['cost_trajectory']['result']['summary_cost'],paid['cost_trajectory']['result']['summary_cost'])
        self.assertEqual(after['cost_trajectory']['priced_primary_responses'],gap['cost_trajectory']['priced_primary_responses'])
        self.assertGreater(after['primary']['cost_usd'],gap['primary']['cost_usd'])

    def test_unknown_retained_output_keeps_known_free_initialization_once(self):
        opaque = {'type':'reasoning','summary':[],'encrypted_content':'unknown-initial-output'}
        fixture = self.fixture(lambda _b,n:(200,support.completed([opaque] if n == 1 else [],f'unknown-{n}',input_tokens=300000 if n == 1 else 300100)))
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','1000000']
        proxy = self.proxy(fixture, options)
        history = [{'role':'user','content':'Already-large pre-window source.'}]
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        first = proxy.saved()
        self.evidence['before_restart'] = first
        self.assertEqual(first['cost_trajectory']['result']['initial_compactions'],1,
                         'known native input permits one free pre-window initialization even when retained output is unknown')
        proxy.close()
        proxy = self.proxy(fixture, options)
        history += [opaque,{'role':'user','content':'Continue after unknown output.'}]
        self.assertEqual(proxy.send(history,max_output_tokens=1)[0],200)
        last = proxy.saved()
        self.evidence.update(after_restart=last, stats=proxy.request('GET','/carry/dashboard/stats')[1])
        self.assertEqual(last['cost_trajectory']['result']['initial_compactions'],1)
        self.assertEqual(last['cost_trajectory']['result']['traditional_compactions'],0)
        self.assertEqual(last['cost_trajectory']['result']['summary_cost'],0)
        self.assertEqual(last['cost_trajectory']['last_traditional_input'],21100)


if __name__ == '__main__':
    unittest.main()
