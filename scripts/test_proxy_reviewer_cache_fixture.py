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
        records.append(dict(role='user', content=json.dumps({'group_id': index + 1,
            'member_ids': [index + 1], 'items': [{'role': 'user', 'content': f'synthetic source {index}'}]})))
    records.append(dict(role='user', content=json.dumps({'eligible_group_ids': [1],
        'current_groups': [{'group_id': index + 1, 'opinion': 'keep' if revision % 2 else None}
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
    def test_documented_implicit_message_endings(self):
        tool = {'type': 'function_call_output', 'output': 'synthetic'}
        developer = {'role': 'developer', 'content': 'synthetic'}
        user = {'role': 'user', 'content': 'synthetic'}
        assistant = {'role': 'assistant', 'content': 'synthetic'}
        for items, expected in [
            ([], []), ([assistant], []),
            ([user, tool, tool, assistant], [1, 3]),
            ([developer, developer, user, developer, assistant], [2, 3]),
            ([user, {'role': 'tool', 'content': 'a'}, {'role': 'tool', 'content': 'b'}], [1, 3]),
            ([user, tool, {'type': 'custom_tool_call_output', 'output': 'x'}, user], [1, 3, 4]),
        ]:
            with self.subTest(expected=expected):
                self.assertEqual(fixture.implicit_eligible_endings(items), expected)

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

    def test_implicit_lookup_horizon_and_no_eligible_ending(self):
        for growth, expected in ((20, 2048), (21, 0)):
            with self.subTest(growth=growth), Provider() as provider:
                body = body_for(1, 0, [])
                body.pop('prompt_cache_options')
                body['input'] = canonical(body['input'])
                send(provider, body)
                extended = copy.deepcopy(body)
                extended['input'] += [{'role': 'user', 'content': f'new {n}'} for n in range(growth)]
                extended['input'] += [{'role': 'assistant', 'content': json.dumps({'eligible_group_ids': [1]})}]
                self.assertEqual(send(provider, extended)['usage']['input_tokens_details']['cached_tokens'], expected)
        with Provider() as provider:
            body = body_for(1, 0, [])
            body.pop('prompt_cache_options')
            body['input'] = canonical(body['input'])
            for item in body['input']:
                item['role'] = 'assistant'
            self.assertEqual(send(provider, body)['usage']['input_tokens_details']['cached_tokens'], 0)
            self.assertEqual(provider.cache, set())
            self.assertEqual(provider.errors, [])

    def test_implicit_receipts_require_exact_historical_prefix_and_settings(self):
        body = body_for(1, 0, [])
        body.pop('prompt_cache_options')
        body['input'] = canonical(body['input'])
        with Provider() as provider:
            self.assertEqual(send(provider, body)['usage']['input_tokens_details']['cached_tokens'], 0)
            extended = copy.deepcopy(body)
            extended['input'] += [{'role': 'assistant', 'content': 'synthetic answer'},
                                  {'role': 'user', 'content': json.dumps({'eligible_group_ids': [1]})}]
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
            self.assertEqual(advice[-2], {'protected': [1], 'removable': [], 'memories': []})
            self.assertEqual(advice[-1], {'protected': [], 'removable': [1], 'memories': []})
            self.assertTrue(all(a['protected'] + a['removable'] == [1] for a in advice))
        # The coupled fixture must still protect direction while removing the
        # whole native cohort, rather than alternately dropping direction.
        with Provider('coupled') as provider:
            body = body_for(2, 0, [])
            ledger = json.loads(body['input'][-1]['content'][0]['text'])
            ledger['eligible_group_ids'].append(2)
            body['input'][-1]['content'][0]['text'] = json.dumps(ledger)
            # Insert cohort evidence as valid observation JSON.
            body['input'][3]['content'][0]['text'] = json.dumps({'group_id': 2, 'items': ['DISCARD_NATIVE_BYTES']})
            for _ in range(2):
                response = send(provider, body)
                self.assertEqual(json.loads(response['output'][-1]['content'][0]['text']),
                    {'protected': [1], 'removable': [2], 'memories': []})

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


class CacheRecordSchemaTests(unittest.TestCase):
    """Persisted observations are not automatically reusable write proof."""
    def setUp(self):
        import hashlib
        digest = lambda value: hashlib.sha256(json.dumps(value, sort_keys=True,
            separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()
        self.body = body_for(1, 0, [])
        length = len(self.body['input']) - 1
        self.record = {'route': None, 'credential_sha256': 'fixture',
            'base_sha256': digest({k: v for k, v in self.body.items() if k not in ('input', 'stream')}),
            'input_sha256': digest(fixture.without_markers(self.body['input'][:length])),
            'input_len': length, 'estimated_tokens': 500.0, 'at': 103,
            'ttl_seconds': 1800, 'native_cached_fraction': 0.0}

    def grade(self, record):
        fixture.grade_cache_records([record], [self.body])

    def test_legacy_raw_reads_and_optional_native_quantities_remain_observations(self):
        for fraction in (0.0, .2, 1.0):
            for extent in ({}, {'native_input_tokens': 10000},
                           {'native_input_tokens': 10000, 'native_write_tokens': 0}):
                record = dict(self.record, native_cached_fraction=fraction, **extent)
                before = copy.deepcopy(record)
                with self.subTest(fraction=fraction, extent=extent):
                    self.grade(record)
                    self.assertEqual(record, before)
                    self.assertNotIn('write_window', record)

    def test_partial_full_and_timed_writes_preserve_exact_raw_record(self):
        for written in (1, 10000):
            for timed in (False, True) if written == 10000 else (False,):
                record = dict(self.record, native_input_tokens=10000, native_write_tokens=written)
                if timed:
                    record['write_window'] = {'not_before': 100, 'not_after': 103, 'lifetime': 1800}
                before = copy.deepcopy(record)
                with self.subTest(written=written, timed=timed):
                    self.grade(record)
                    self.assertEqual(record, before)

    def test_schema_guard_rejects_extra_missing_and_invalid_numeric_fields(self):
        required = set(self.record)
        for key in required:
            record = dict(self.record)
            del record[key]
            with self.subTest(missing=key), self.assertRaises(AssertionError):
                self.grade(record)
        for field in ('source', 'warm', 'native_output_tokens'):
            with self.subTest(extra=field), self.assertRaises(AssertionError):
                self.grade(dict(self.record, **{field: 'not part of the bounded evidence schema'}))
        for key in ('input_len', 'at', 'ttl_seconds', 'native_input_tokens', 'native_write_tokens'):
            for invalid in (-1, True, 1.5, '100', None, 2 ** 64):
                record = dict(self.record, native_input_tokens=10000, native_write_tokens=0)
                record[key] = invalid
                with self.subTest(key=key, value=invalid), self.assertRaises(AssertionError):
                    self.grade(record)
        for key in ('estimated_tokens', 'native_cached_fraction'):
            for invalid in (-1, True, '0.2', None, float('nan'), float('inf')):
                with self.subTest(key=key, value=invalid), self.assertRaises(AssertionError):
                    self.grade(dict(self.record, **{key: invalid}))
        for extent in ({'native_write_tokens': 1},
                       {'native_input_tokens': 0},
                       {'native_input_tokens': 10000, 'native_write_tokens': 10001}):
            with self.subTest(extent=extent), self.assertRaises(AssertionError):
                self.grade(dict(self.record, **extent))

    def test_write_window_requires_whole_uncached_extent_and_original_bounds(self):
        record = dict(self.record, native_input_tokens=10000, native_write_tokens=10000,
                      write_window={'not_before': 100, 'not_after': 103, 'lifetime': 1800})
        mutations = [dict(record, native_cached_fraction=.2),
                     dict(record, native_write_tokens=9999)]
        for field in ('native_input_tokens', 'native_write_tokens'):
            changed = copy.deepcopy(record)
            del changed[field]
            mutations.append(changed)
        for field in ('not_before', 'not_after', 'lifetime'):
            for invalid in (-1, True, 1.5, '100', None, 2 ** 64):
                changed = copy.deepcopy(record)
                changed['write_window'][field] = invalid
                mutations.append(changed)
            changed = copy.deepcopy(record)
            del changed['write_window'][field]
            mutations.append(changed)
        for window in ({'not_before': 104, 'not_after': 103, 'lifetime': 1800},
                       {'not_before': 100, 'not_after': 104, 'lifetime': 1800},
                       {'not_before': 100, 'not_after': 103, 'lifetime': 7200},
                       dict(record['write_window'], refreshed_at=103), None, [], 'write'):
            mutations.append(dict(record, write_window=window))
        for index, changed in enumerate(mutations):
            with self.subTest(index=index), self.assertRaises(AssertionError):
                self.grade(changed)

    def test_numeric_storage_count_and_byte_bounds_remain_enforced(self):
        with self.assertRaises(AssertionError):
            fixture.grade_cache_records([self.record] * 129, [self.body])
        oversized = dict(self.record, credential_sha256='x' * 65536)
        with self.assertRaises(AssertionError):
            self.grade(oversized)


if __name__ == '__main__':
    unittest.main()
