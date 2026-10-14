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
    for point in sorted(set([p for p in previous if p < end][-3:] + [end])):
        records[point - 1]['content'][0]['prompt_cache_breakpoint'] = {'mode': 'explicit'}
    return {'model': 'gpt-6-luna', 'input': records, 'prompt_cache_key': 'synthetic-fixture-only',
            'prompt_cache_options': {'mode': 'explicit'}}


def send(provider, body):
    request = urllib.request.Request(provider.url + '/classifier', data=json.dumps(body).encode(),
                                     headers={'content-type': 'application/json'})
    with urllib.request.urlopen(request, timeout=3) as response:
        return json.load(response)


class SimulatorTests(unittest.TestCase):
    def test_http_advice_explicitly_replaces_same_eligible_opinion_on_ledger_only_reviews(self):
        # Inspect real HTTP responses, not fabricated ledger revisions: Rust
        # apply_advice preserves omitted IDs, so both directions need an update.
        with Provider() as provider:
            body = body_for(1, 0, [])
            responses = [send(provider, body) for _ in range(10)]
            advice = [json.loads(r['output'][0]['content'][0]['text']) for r in responses]
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
                self.assertEqual(json.loads(response['output'][0]['content'][0]['text']),
                    {'protected': ['g1'], 'removable': ['g2'], 'memories': []})

    def test_default_dispatcher_labels_seven_distinct_conditions(self):
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
        self.assertEqual(result['case_count'],7)
        self.assertEqual({r['case'] for r in result['cases']},
            {'rotation','coupled','missing-usage','disabled','auto-generic','auto-unknown','explicit-unknown-rejected'})
        self.assertEqual(conditions[-3:],[('disabled',False,'gpt-6-luna','rotation'),
            ('auto',False,'gpt-6-luna','rotation'),('auto',False,'unknown-fixture-model','rotation')])

    def sequence(self):
        bodies = []
        prior = []
        with Provider() as provider:
            for turn in range(10):
                body = body_for(min(turn + 1, 8), turn, prior)
                send(provider, body)
                bodies.append(body)
                prior = boundaries(body)
            grade(provider.reviews, provider.receipts)
            self.assertEqual(len(provider.reviews), 10)
            self.assertEqual(provider.receipts[0]['synthetic_cached_tokens'], 0)
            self.assertTrue(all(r['synthetic_cached_tokens'] == 2048 for r in provider.receipts[1:]))
            return bodies, provider.receipts

    def test_simulator_eight_rotations_and_ledger_only_changes_reuse_prior_boundary(self):
        bodies, receipts = self.sequence()
        self.assertEqual(canonical(bodies[-1]['input'][:-1]), canonical(bodies[-2]['input'][:-1]))
        self.assertEqual(max(len(boundaries(b)) for b in bodies), 4)
        # Marker rotation changes raw JSON but not the quoted model text prefix.
        self.assertNotEqual(bodies[3]['input'][:3], bodies[4]['input'][:3])
        self.assertEqual(canonical(bodies[3]['input'][:3]), canonical(bodies[4]['input'][:3]))
        self.assertEqual(fixture.prefix_identity(bodies[3], 3), fixture.prefix_identity(bodies[4], 3))

    def test_grader_rejects_missing_outbound_marker_not_cli_or_source_text(self):
        bodies, receipts = self.sequence()
        for body in bodies:
            for item in body['input']:
                item['content'][0].pop('prompt_cache_breakpoint', None)
        with self.assertRaisesRegex(AssertionError, 'missing outbound stable reviewer cache breakpoint'):
            grade(bodies, receipts)

    def test_grader_rejects_marker_at_mutable_ledger(self):
        bodies, receipts = self.sequence()
        bodies[0]['input'][-1]['content'][0]['prompt_cache_breakpoint'] = {'mode': 'explicit'}
        with self.assertRaisesRegex(AssertionError, 'not ledger'):
            grade(bodies, receipts)

    def test_grader_rejects_previous_boundary_disappearing_after_rotation(self):
        bodies, receipts = self.sequence()
        previous = max(boundaries(bodies[0]))
        bodies[1]['input'][previous - 1]['content'][0].pop('prompt_cache_breakpoint')
        with self.assertRaisesRegex(AssertionError, 'lookup candidate'):
            grade(bodies, receipts)

    def test_simulator_declares_missing_usage_then_cold_write_then_read_separately(self):
        with Provider('missing-usage') as provider:
            body = body_for(1, 0, [])
            first, second, third = [send(provider, body) for _ in range(3)]
            self.assertNotIn('usage', first)
            self.assertEqual(second['usage']['input_tokens_details']['cached_tokens'], 0)
            self.assertEqual(second['usage']['input_tokens_details']['cache_write_tokens'], 10000)
            self.assertEqual(third['usage']['input_tokens_details']['cached_tokens'], 2048)

    def test_simulator_requires_exact_text_and_non_input_settings(self):
        with Provider() as provider:
            body = body_for(1, 0, [])
            send(provider, body)
            identity = fixture.prefix_identity(body, len(body['input']) - 1)
            record = {'format_version': 1, 'base': {'settings': {k: v for k, v in body.items() if k != 'input'}},
                      'input_len': len(body['input']) - 1, 'input_sha256': identity, 'at': 1,
                      'read_estimated_tokens': 0.0, 'read_confirmed_at': None}
            fixture.grade_cache_records([record], body)
            for bad in [dict(record, input_len=len(body['input'])),
                        dict(record, input_sha256='0' * 64)]:
                with self.assertRaises(AssertionError):
                    fixture.grade_cache_records([bad], body)
            role_change = copy.deepcopy(body)
            role_change['input'][2]['role'] = 'assistant'
            self.assertNotEqual(identity, fixture.prefix_identity(role_change, record['input_len']))
            changed = copy.deepcopy(body)
            changed['input'][2]['content'][0]['text'] += ' changed text'
            self.assertNotEqual(identity, fixture.prefix_identity(changed, record['input_len']))
            self.assertEqual(send(provider, changed)['usage']['input_tokens_details']['cached_tokens'], 0)
            changed = copy.deepcopy(body)
            changed['model'] = 'different-synthetic-model'
            self.assertEqual(send(provider, changed)['usage']['input_tokens_details']['cached_tokens'], 0)

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
