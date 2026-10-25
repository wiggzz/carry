#!/usr/bin/env python3
"""Exercise the loopback cache simulator and its behavioral negative guards.

These tests do NOT execute repaired Rust; hosted CI must run the integrated
fixture against its newly built binary. Inputs and usage here are synthetic.
"""
import copy
import json
import unittest
import urllib.error
import urllib.request

if __package__:
    from . import proxy_reviewer_cache_fixture as fixture
    from .proxy_reviewer_cache_fixture import Provider, boundaries, canonical, grade
else:
    import proxy_reviewer_cache_fixture as fixture
    from proxy_reviewer_cache_fixture import Provider, boundaries, canonical, grade


def body_for(observations, revision, previous):
    records: list[dict] = [dict(role='user', content='Return JSON.'),
               dict(role='user', content=json.dumps({'current_request': {'model': 'fixture'}}))]
    for index in range(observations):
        records.append(dict(role='user', content=json.dumps({'group_id': f'g{index + 1}',
            'member_ids': [index + 1], 'items': [{'role': 'user', 'content': f'synthetic source {index}'}]})))
    records.append(dict(role='user', content=json.dumps({'eligible_group_ids': ['g1'],
        'current_groups': [{'group_id': f'g{index + 1}', 'opinion': 'keep' if revision % 2 else None}
                           for index in range(observations)]})))
    for record in records:
        record['content'] = [{'type': 'input_text', 'text': record['content']}]
    end = len(records) - 1
    records[end - 1]['content'][0]['prompt_cache_breakpoint'] = {'mode': 'explicit'}
    return {'model': 'gpt-6-luna', 'input': records, 'prompt_cache_key': 'synthetic-fixture-only',
            'prompt_cache_options': {'mode': 'explicit'}}


def send(provider, body):
    request = urllib.request.Request(provider.url + '/classifier', data=json.dumps(body).encode(),
                                     headers={'content-type': 'application/json'})
    with urllib.request.urlopen(request, timeout=3) as response:
        return json.load(response)


class SimulatorTests(unittest.TestCase):
    def sequence(self):
        with Provider() as provider:
            bodies = [body_for(min(turn + 1, 7), turn, []) for turn in range(8)]
            for body in bodies:
                send(provider, body)
            receipts = copy.deepcopy(provider.receipts)
        return bodies, receipts

    def test_stable_boundary_and_ledger_only_change(self):
        bodies, receipts = self.sequence()
        grade(bodies, receipts)
        self.assertGreater(receipts[-1]['synthetic_cached_tokens'], 0)

    def test_missing_marker_and_marker_on_ledger_are_rejected(self):
        for corruption in ('missing', 'ledger', 'unchanged', 'receipt'):
            bodies, receipts = self.sequence()
            if corruption == 'missing':
                bodies[-1]['input'][-2]['content'][0].pop('prompt_cache_breakpoint')
            elif corruption == 'ledger':
                bodies[-1]['input'][-1]['content'][0]['prompt_cache_breakpoint'] = {'mode': 'explicit'}
            elif corruption == 'unchanged':
                bodies[-1]['input'][-1] = copy.deepcopy(bodies[-2]['input'][-1])
            else:
                receipts[-1]['synthetic_cached_tokens'] = 0
            with self.subTest(corruption=corruption), self.assertRaises(AssertionError):
                grade(bodies, receipts)

    def test_disposable_output_does_not_replay(self):
        bodies, _ = self.sequence()
        outputs = [[{'id': f'reviewer_{n}'}] for n in range(8)]
        fixture.grade_projection(bodies, outputs)
        bodies[-1]['input'].insert(-1, {'id': 'reviewer_0', 'role': 'assistant', 'content': 'old answer'})
        with self.assertRaisesRegex(AssertionError, 'must not replay'):
            fixture.grade_projection(bodies, outputs)

    def test_shared_evidence_binds_exact_native_wire_prefix_and_settings(self):
        import hashlib
        digest = lambda v: hashlib.sha256(json.dumps(v, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        body = body_for(1, 0, [])
        length = len(body['input']) - 1
        record = {'route': None, 'credential_sha256': 'fixture',
                  'base_sha256': digest({k:v for k,v in body.items() if k not in ('input', 'stream')}),
                  'input_sha256': digest(fixture.without_markers(body['input'][:length])),
                  'input_len': length, 'estimated_tokens': 500.0, 'at': 1,
                  'ttl_seconds': 1800, 'native_cached_fraction': .2}
        fixture.grade_cache_records([record], [body])
        for mutation in ('text', 'role', 'wrapper', 'settings', 'fraction'):
            changed = copy.deepcopy(body)
            entry = dict(record)
            if mutation == 'text': changed['input'][2]['content'][0]['text'] += ' changed'
            elif mutation == 'role': changed['input'][2]['role'] = 'assistant'
            elif mutation == 'wrapper': changed['input'][2]['content'] = changed['input'][2]['content'][0]['text']
            elif mutation == 'settings': changed['model'] = 'other'
            else: entry['native_cached_fraction'] = 2
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                fixture.grade_cache_records([entry], [changed])

    def test_implicit_receipts_require_exact_historical_prefix_and_settings(self):
        body = body_for(1, 0, [])
        body.pop('prompt_cache_options')
        body['input'] = canonical(body['input'])
        with Provider() as provider:
            self.assertEqual(send(provider, body)['usage']['input_tokens_details']['cached_tokens'], 0)
            extended = copy.deepcopy(body)
            extended['input'] += [{'role': 'assistant', 'content': 'synthetic answer'},
                                  {'role': 'user', 'content': json.dumps({'eligible_group_ids': ['g1']})}]
            self.assertEqual(send(provider, extended)['usage']['input_tokens_details']['cached_tokens'], 2048)
            self.assertIn(len(body['input']), provider.receipts[-1]['matched_boundaries'])
            for mutation in ('text', 'role', 'wrapper', 'settings'):
                with self.subTest(mutation=mutation):
                    changed = copy.deepcopy(extended)
                    if mutation == 'settings':
                        changed['reasoning'] = {'effort': 'different-fixture-setting'}
                    elif mutation == 'wrapper':
                        changed['input'][0]['content'] = [{'type': 'input_text', 'text': body['input'][0]['content']}]
                    else:
                        changed['input'][0][mutation if mutation == 'role' else 'content'] = 'changed'
                    self.assertEqual(send(provider, changed)['usage']['input_tokens_details']['cached_tokens'], 0)
            self.assertTrue(all(not boundaries(b) for b in provider.reviews))

    def test_http_advice_explicitly_replaces_same_eligible_opinion_on_ledger_only_reviews(self):
        # Inspect real HTTP responses, not fabricated ledger revisions: Rust
        # apply_advice preserves omitted IDs, so both directions need an update.
        with Provider() as provider:
            body = body_for(1, 0, [])
            responses = [send(provider, body) for _ in range(10)]
            advice = [json.loads(r['output'][-1]['content'][0]['text']) for r in responses]
            self.assertEqual(advice[-2], {'protected': ['g1'], 'removable': [], 'memories': []})
            self.assertEqual(advice[-1], {'protected': [], 'removable': ['g1'], 'memories': []})
            self.assertTrue(all(a['protected'] + a['removable'] == ['g1'] for a in advice))
        # The coupled fixture must still protect direction while removing the
        # whole native cohort, rather than alternately dropping direction.
        with Provider('coupled') as provider:
            body = body_for(2, 0, [])
            ledger = json.loads(body['input'][-1]['content'][0]['text'])
            ledger['eligible_group_ids'].append('g2')
            body['input'][-1]['content'][0]['text'] = json.dumps(ledger)
            # Insert cohort evidence as valid observation JSON.
            body['input'][3]['content'][0]['text'] = json.dumps({'group_id': 'g2', 'items': ['DISCARD_NATIVE_BYTES']})
            for _ in range(2):
                response = send(provider, body)
                self.assertEqual(json.loads(response['output'][-1]['content'][0]['text']),
                    {'protected': ['g1'], 'removable': ['g2'], 'memories': []})

    def test_default_dispatcher_labels_nine_distinct_conditions(self):
        import contextlib
        import io
        from pathlib import Path
        import tempfile
        from unittest.mock import patch
        conditions=[]
        def run(binary, directory, policy, enabled, model, case='rotation'):
            conditions.append((policy,enabled,model,case))
            return {'case':case,'basis':'dispatcher mock only, not binary evidence'}
        with tempfile.TemporaryDirectory() as directory:
            with patch('sys.argv',['fixture','--carry','mock-only','--output',directory]), \
                 patch.object(fixture,'run',side_effect=run), \
                 patch.object(fixture,'reject_unknown',return_value={'case':'explicit-unknown-rejected'}), \
                 contextlib.redirect_stdout(io.StringIO()):
                fixture.main()
            result=json.loads((Path(directory)/'results.json').read_text())
        self.assertEqual(result['case_count'],9)
        self.assertEqual({r['case'] for r in result['cases']},
            {'rotation','coupled','missing-usage','disabled','auto-generic','auto-unknown',
             'implicit-coupled','implicit-missing-usage','explicit-unknown-rejected'})
        self.assertEqual(conditions[-5:],[('disabled',False,'gpt-6-luna','rotation'),
            ('auto',False,'gpt-6-luna','rotation'),('auto',False,'unknown-fixture-model','rotation'),
            ('disabled',False,'gpt-6-luna','coupled'),('disabled',False,'gpt-6-luna','missing-usage')])

    def test_simulator_declares_missing_usage_then_cold_write_then_read_separately(self):
        with Provider('missing-usage') as provider:
            body = body_for(1, 0, [])
            first, second, third = [send(provider, body) for _ in range(3)]
            self.assertNotIn('usage', first)
            self.assertEqual(second['usage']['input_tokens_details']['cached_tokens'], 0)
            self.assertEqual(second['usage']['input_tokens_details']['cache_write_tokens'], 10000)
            self.assertEqual(third['usage']['input_tokens_details']['cached_tokens'], 2048)

    def test_simulator_rejects_more_than_four_explicit_write_slots(self):
        with Provider() as provider:
            body = body_for(5, 0, [])
            for item in body['input'][2:-1]:
                item['content'][0]['prompt_cache_breakpoint'] = {'mode': 'explicit'}
            with self.assertRaises(urllib.error.HTTPError) as error:
                send(provider, body)
            self.assertEqual(error.exception.code, 500)
            error.exception.close()
            self.assertEqual(provider.errors, ['more than four explicit WRITE slots'])


if __name__ == '__main__':
    unittest.main()
