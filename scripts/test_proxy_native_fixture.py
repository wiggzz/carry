"""Exercise the credential-free native-client provider fixture over HTTP."""
import json
import unittest
import urllib.request
from contextlib import contextmanager
import http.server
import os
from pathlib import Path
import select
import subprocess
import tempfile
import threading


class ShadowContractProvider:
    """Real HTTP wire capture; no model, credentials or synthesized proxy state."""
    def __init__(self):
        self.primary = []
        self.shadow = []
        self.advice = lambda review: {'protected': [], 'removable': [], 'memories': []}
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['content-length'])))
                review = self.path == '/review'
                (outer.shadow if review else outer.primary).append(body)
                advice = outer.advice(body) if review else None
                failed = not review and body.get('metadata', {}).get('fail_primary')
                value = {'status': 'failed' if failed else 'completed',
                         'output': [{'type': 'message', 'role': 'assistant', 'content': [
                             {'type': 'output_text', 'text': json.dumps(advice)}]}] if review else [],
                         'usage': {'input_tokens': 100, 'output_tokens': 1,
                                   'input_tokens_details': {'cached_tokens': 0}}}
                raw = json.dumps(value).encode()
                self.send_response(200)
                self.send_header('content-type', 'application/json')
                self.send_header('content-length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.url = 'http://127.0.0.1:' + str(self.server.server_port)

    def __enter__(self):
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


@contextmanager
def contract_proxy(test, provider, state_dir, mode='audit', binary=None):
    binary = Path(binary or os.environ.get('CARRY_TEST_BINARY', 'target/debug/carry')).resolve()
    if not binary.is_file():
        test.skipTest('requires an existing Carry binary; never builds locally')
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('OPENAI_', 'CARRY_PROXY_'))}
    command = [str(binary), 'proxy', '--listen', '127.0.0.1:0',
               '--upstream-url', provider.url + '/v1/responses',
               '--classifier-url', provider.url + '/review', '--mode', mode,
               '--min-payback-percent', '0', '--payoff-requests', '5',
               '--state-dir', state_dir]
    process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, text=True)
    assert process.stdout is not None
    try:
        test.assertTrue(select.select([process.stdout], [], [], 5)[0], 'proxy startup timed out')
        banner = process.stdout.readline().strip()
        test.assertTrue(banner.startswith('CARRY_PROXY_LISTEN '), 'proxy failed to start')
        yield 'http://' + banner.removeprefix('CARRY_PROXY_LISTEN ')
    finally:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)
        process.stdout.close()


def contract_send(url, body):
    request = urllib.request.Request(url + '/v1/responses', data=json.dumps(body).encode(),
        headers={'content-type': 'application/json', 'x-carry-session': 'contract'})
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.load(response)


def wire_data(body):
    result = []
    for record in body['input']:
        try:
            result.append(json.loads(record.get('content', '')))
        except (ValueError, TypeError):
            pass
    return result


def contract_state(directory):
    return json.loads(next(Path(directory).glob('*.json')).read_text())


class NativeProviderFixtureTests(unittest.TestCase):
    def test_proxy_observes_bulk_once_across_exposure_metadata_failed_primary_and_restart(self):
        with ShadowContractProvider() as provider, tempfile.TemporaryDirectory() as directory:
            body = {'model': 'gpt-6-luna', 'input': [
                {'role': 'user', 'content': 'old requirement'},
                {'role': 'user', 'content': 'current task'}]}
            bulk = 'IMMUTABLE_BULK_MARKER ' * 3000
            with contract_proxy(self, provider, directory) as url:
                contract_send(url, body)
                body['input'] += [
                    {'type': 'function_call', 'id': 'native-a', 'status': 'completed',
                     'call_id': 'a', 'name': 'native', 'arguments': '{}'},
                    {'type': 'function_call_output', 'call_id': 'a', 'output': bulk}]
                contract_send(url, body)
                body['metadata'] = {'fail_primary': True}
                contract_send(url, body)
                self.assertEqual(contract_state(directory)['failed_primaries'], 1)
                self.assertEqual(contract_state(directory)['shadow']['calls'], 2)
            with contract_proxy(self, provider, directory) as url:
                body.pop('metadata')
                # Tolerated native echo metadata changes are not new source observations.
                body['input'][2].pop('id')
                body['input'][2].pop('status')
                contract_send(url, body)
                contract_send(url, body)
            self.assertEqual(len(provider.shadow), 3)
            self.assertEqual(provider.primary[-1], body, 'canonical native echo is untouched')
            counts = [json.dumps(r).count('IMMUTABLE_BULK_MARKER') for r in provider.shadow]
            if os.environ.get('CARRY_SHADOW_EVIDENCE'):
                Path(os.environ['CARRY_SHADOW_EVIDENCE']).write_text(json.dumps({
                    'fixture_only': True, 'bulk_mentions_per_review': counts,
                    'review_input_bytes': [len(json.dumps(r['input']).encode()) for r in provider.shadow],
                    'primary_calls': len(provider.primary), 'shadow_calls': len(provider.shadow)
                }, indent=2) + '\n')
            self.assertEqual(counts, [3000, 3000, 3000],
                             'exposure/metadata bookkeeping must not duplicate immutable main source')
            for index, review in enumerate(provider.shadow):
                records = wire_data(review)
                blocks = [r for r in records if 'items' in r]
                self.assertTrue(blocks)
                self.assertTrue(all('eligible' not in r for r in blocks),
                                'immutable observation must not freeze stale eligibility')
                self.assertEqual(records[-1]['eligible_group_ids'],
                                 ['g1'] if index == 0 else ['g1', 'g3'])
            self.assertEqual(contract_state(directory)['invalid_reviews'], 0)


    def test_proxy_memory_facts_ground_new_advice_in_original_live_sources_not_memory_handles(self):
        with ShadowContractProvider() as provider, tempfile.TemporaryDirectory() as directory:
            body = {'model': 'gpt-6-luna', 'input': [
                {'role': 'user', 'content': 'RETIRE_HUMAN_SOURCE ' * 3000},
                {'role': 'user', 'content': 'current task'}]}
            fact = 'the original requirement is JADE42'
            provider.advice = lambda review: {'protected': [], 'removable': [],
                'memories': [{'source_ids': ['g1'], 'text': fact}]}
            with contract_proxy(self, provider, directory, mode='compact') as url:
                contract_send(url, body)
                contract_send(url, body)
                self.assertEqual(contract_state(directory)['invalid_reviews'], 0)
                # A reviewer derives another fact from the shown prior fact's provenance.
                grounding = []
                def grounded(review):
                    facts = [r for r in wire_data(review) if r.get('text') == fact]
                    grounding.append(facts)
                    return {'protected': [], 'removable': [], 'memories': [
                        {'source_ids': facts[0].get('source_ids', ['m1']),
                         'text': 'JADE42 remains the required outcome'}]}
                provider.advice = grounded
                contract_send(url, body)
                state = contract_state(directory)
                if os.environ.get('CARRY_MEMORY_EVIDENCE'):
                    Path(os.environ['CARRY_MEMORY_EVIDENCE']).write_text(json.dumps({
                        'fixture_only': True, 'shown_fact_fields': sorted(grounding[0][0]),
                        'shown_source_ids': grounding[0][0].get('source_ids'),
                        'invalid_reviews': state['invalid_reviews'],
                        'accepted_memory_count': len(state['memories'])
                    }, indent=2) + '\n')
                self.assertEqual(state['invalid_reviews'], 0,
                                 'reviewer must receive the original source IDs for valid grounding')
                self.assertEqual(grounding[0][0]['source_ids'], ['g1'])
                self.assertNotIn('memory_id', grounding[0][0],
                                 'internal memory handle must not masquerade as an actionable source')
                self.assertEqual(len(state['memories']), 2)
                # Keep validation strict: m1 is not a main source; whole mixed batch rejects.
                provider.advice = lambda review: {'protected': [], 'removable': ['g1'],
                    'memories': [{'source_ids': ['m1'], 'text': 'invalid source'}]}
                contract_send(url, body)
                state = contract_state(directory)
                self.assertEqual(state['invalid_reviews'], 1)
                self.assertEqual(len(state['memories']), 2)
                self.assertEqual(state['compactions'], 0)
                # Retire original human source only with an accepted original-ID review.
                provider.advice = lambda review: {'protected': [], 'removable': ['g1'], 'memories': []}
                contract_send(url, body)
                self.assertNotIn('RETIRE_HUMAN_SOURCE', json.dumps(provider.primary[-1]))
                state = contract_state(directory)
                self.assertTrue(state['history'][0]['removed'])
                self.assertEqual(state['compactions'], 1)
                # Provenance remains visible, but does not re-admit retired IDs for advice.
                provider.advice = lambda review: {'protected': [], 'removable': [],
                    'memories': [{'source_ids': ['g1'], 'text': 'retired source is not newly eligible'}]}
                body['input'].append({'role': 'user', 'content': 'next task'})
                contract_send(url, body)
                later = provider.shadow[-1]
                self.assertNotIn('RETIRE_HUMAN_SOURCE', json.dumps(later))
                self.assertEqual([r for r in wire_data(later) if r.get('text') == fact][0]['source_ids'], ['g1'])
                self.assertNotIn('g1', wire_data(later)[-1]['eligible_group_ids'])
                self.assertEqual(contract_state(directory)['invalid_reviews'], 2)
                self.assertEqual(len(contract_state(directory)['memories']), 2)

    def test_proxy_parallel_cohort_extension_observes_only_new_members_and_current_opinion(self):
        with ShadowContractProvider() as provider, tempfile.TemporaryDirectory() as directory:
            body = {'model': 'gpt-6-luna', 'input': [
                {'role': 'user', 'content': 'old requirement'},
                {'role': 'user', 'content': 'current task'}]}
            provider.advice = lambda review: {'protected': ['g1'], 'removable': [], 'memories': []}
            with contract_proxy(self, provider, directory) as url:
                contract_send(url, body)
                body['input'] += [
                    {'type': 'function_call', 'call_id': 'a', 'name': 'native', 'arguments': 'CALL_A'},
                    {'type': 'function_call', 'call_id': 'b', 'name': 'native', 'arguments': 'CALL_B'},
                    {'type': 'function_call_output', 'call_id': 'a', 'output': 'RESULT_A ' * 500}]
                contract_send(url, body)
                provider.advice = lambda review: {'protected': [], 'removable': ['g1'], 'memories': []}
                body['input'].append({'type': 'function_call_output', 'call_id': 'b',
                                      'output': 'RESULT_B ' * 500})
                contract_send(url, body)
                provider.advice = lambda review: {'protected': [], 'removable': [], 'memories': []}
                contract_send(url, body)
            self.assertEqual(len(provider.shadow), 3)
            self.assertEqual([json.dumps(r).count('RESULT_A') for r in provider.shadow], [500] * 3,
                             'partial cohort extension must not re-copy earlier member payloads')
            self.assertEqual([json.dumps(r).count('RESULT_B') for r in provider.shadow], [0, 500, 500])
            for index, review in enumerate(provider.shadow):
                records = wire_data(review)
                blocks = [r for r in records if 'items' in r and r['group_id'] == 'g3']
                self.assertEqual([r['member_ids'] for r in blocks],
                                 [[3, 4, 5]] if index == 0 else [[3, 4, 5], [6]])
                tail = records[-1]
                self.assertEqual(tail['eligible_group_ids'], ['g1'] if index < 2 else ['g1', 'g3'])
                self.assertEqual(tail['current_groups'][0]['opinion'], [None, 'keep', 'drop'][index])
                self.assertFalse(any('opinion' in r for r in records[:-1]),
                                 'superseded opinions must not live in immutable observation history')
            state = contract_state(directory)
            self.assertEqual(state['invalid_reviews'], 0)
            self.assertEqual(len(state['observed']), 6)
            self.assertEqual(provider.primary[-1], body)

    def test_proxy_upgrades_real_legacy_checkpoint_without_duplicate_source_or_memory_aliases(self):
        legacy = os.environ.get('CARRY_LEGACY_BINARY')
        if not legacy:
            self.skipTest('optional old binary needed for real checkpoint upgrade replay')
        with ShadowContractProvider() as provider, tempfile.TemporaryDirectory() as directory:
            body = {'model': 'gpt-6-luna', 'input': [
                {'role': 'user', 'content': 'old requirement'},
                {'role': 'user', 'content': 'current task'}]}
            provider.advice = lambda review: {'protected': ['g1'], 'removable': [],
                'memories': [{'source_ids': ['g1'], 'text': 'legacy derived fact'}]}
            with contract_proxy(self, provider, directory, binary=legacy) as url:
                contract_send(url, body)
                body['input'] += [
                    {'type': 'function_call', 'call_id': 'a', 'name': 'native', 'arguments': '{}'},
                    {'type': 'function_call_output', 'call_id': 'a', 'output': 'LEGACY_BULK ' * 500}]
                contract_send(url, body)
                contract_send(url, body)
            self.assertEqual(json.dumps(provider.shadow[-1]).count('LEGACY_BULK'), 1000,
                             'old binary must actually produce the historical duplicated checkpoint')
            with contract_proxy(self, provider, directory) as url:
                contract_send(url, body)
                contract_send(url, body)
            for review in provider.shadow[-2:]:
                self.assertEqual(json.dumps(review).count('LEGACY_BULK'), 500)
                data = wire_data(review)
                self.assertEqual([r for r in data if r.get('text') == 'legacy derived fact'][0]['source_ids'], ['g1'])
                self.assertTrue(all('memory_id' not in r and 'eligible' not in r and 'opinion' not in r
                                    for r in data[:-1]))
            self.assertEqual(provider.primary[-1], body)
            self.assertEqual(contract_state(directory)['invalid_reviews'], 0)

    def test_classifier_uses_tail_eligibility_for_immutable_payload_blocks(self):
        from scripts.proxy_native_fixture import Fixture
        for eligible, stale in [(True, False), (False, True)]:
            with self.subTest(eligible=eligible), Fixture('pi', 'compact') as fixture:
                block = {'group_id': 'g7', 'member_ids': [7],
                         'items': [{'type': 'function_call_output', 'call_id': 'a',
                                    'output': 'DROP_COHORT_PAYLOAD'}]}
                if stale:
                    block['eligible'] = True
                body = {'input': [{'role': 'user', 'content': json.dumps(block)},
                    {'role': 'user', 'content': json.dumps({'eligible_group_ids': ['g7'] if eligible else []})}]}
                request = urllib.request.Request(fixture.url + '/classifier', data=json.dumps(body).encode(),
                    headers={'content-type': 'application/json'})
                with urllib.request.urlopen(request, timeout=3) as response:
                    advice = json.load(response)
                self.assertEqual(json.loads(advice['output'][0]['content'][0]['text'])['removable'],
                                 ['g7'] if eligible else [])

    def test_json_mode_requires_json_in_actual_input_message_text(self):
        from scripts.proxy_native_fixture import Fixture
        import urllib.error
        rejected = [[], [{'role': 'user', 'content': 'Finish the task.'}],
                    [{'role': 'user', 'content': [{'type': 'input_text', 'text': 'Finish.'}],
                      'metadata': 'JSON'}],
                    [{'type': 'function_call_output', 'call_id': 'a', 'output': 'JSON'}]]
        accepted = [[{'role': 'user', 'content': 'Return JSON.'}],
                    [{'type': 'message', 'role': 'user',
                      'content': [{'type': 'input_text', 'text': 'Return jSoN.'}]}]]
        for input_value in rejected + accepted:
            with self.subTest(input=input_value), Fixture('pi') as fixture:
                body = {'instructions': 'Return JSON.', 'input': input_value,
                        'text': {'format': {'type': 'json_object'}}}
                req = urllib.request.Request(fixture.url + '/classifier',
                    data=json.dumps(body).encode(), headers={'content-type': 'application/json'})
                if input_value in rejected:
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        with urllib.request.urlopen(req, timeout=3) as response:
                            response.read()
                    self.assertEqual(error.exception.code, 400)
                    value = json.loads(error.exception.read())
                    error.exception.close()
                    self.assertEqual(value['error']['param'], 'input')
                    self.assertEqual(value['error']['type'], 'invalid_request_error')
                    self.assertNotIn('code', value['error'])
                else:
                    with urllib.request.urlopen(req, timeout=3) as response:
                        self.assertEqual(response.status, 200)
                        response.read()
                    self.assertFalse(fixture.errors)

    def test_proxy_json_mode_review_succeeds_without_json_in_primary_or_tools(self):
        from scripts.proxy_native_fixture import Fixture
        import os
        import select
        import subprocess
        import tempfile
        from pathlib import Path
        binary = Path(os.environ.get('CARRY_TEST_BINARY', 'target/debug/carry')).resolve()
        if not binary.is_file():
            self.skipTest('requires an existing Carry binary; never builds locally')
        with Fixture('pi') as fixture, tempfile.TemporaryDirectory() as directory:
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('OPENAI_', 'CARRY_PROXY_'))}
            env.update(CARRY_PROXY_UPSTREAM_KEY='synthetic-fixture-key',
                       CARRY_PROXY_CLASSIFIER_KEY='synthetic-fixture-key')
            command = [str(binary), 'proxy', '--listen', '127.0.0.1:0',
                       '--upstream-url', fixture.url + '/v1/responses',
                       '--classifier-url', fixture.url + '/classifier',
                       '--classifier-model', 'gpt-6-luna', '--mode', 'audit',
                       '--state-dir', directory]
            process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL, text=True)
            assert process.stdout is not None
            try:
                self.assertTrue(select.select([process.stdout], [], [], 5)[0], 'proxy startup timed out')
                banner = process.stdout.readline().strip()
                self.assertTrue(banner.startswith('CARRY_PROXY_LISTEN '), 'proxy failed to start')
                url = 'http://' + banner.removeprefix('CARRY_PROXY_LISTEN ')
                body = {'model': 'gpt-6-luna', 'prompt_cache_key': 'plain-conversation',
                        'instructions': 'Finish the task.',
                        'tools': [{'type': 'function', 'name': 'bash',
                                   'description': 'Run a shell command.',
                                   'parameters': {'type': 'object'}}],
                        'input': [{'role': 'user', 'content': 'Finish the task.'},
                                  {'type': 'function_call', 'name': 'bash',
                                   'call_id': 'old', 'arguments': '{"command":"printf old"}'},
                                  {'type': 'function_call_output', 'call_id': 'old', 'output': 'old'}]}
                for turn in range(2):
                    self.assertNotIn('json', json.dumps(body).lower())
                    req = urllib.request.Request(url + '/v1/responses',
                        data=json.dumps(body).encode(), headers={'content-type': 'application/json',
                                                               'x-carry-session': 'plain-conversation'})
                    try:
                        with urllib.request.urlopen(req, timeout=5) as response:
                            raw = response.read().decode()
                    except urllib.error.HTTPError as error:
                        detail = error.read().decode()
                        error.close()
                        self.fail(f'primary request {turn} failed: {detail}; {fixture.errors}')
                    self.assertEqual(fixture.calls[-1], body, 'review must not alter primary/tool data')
                    result = [json.loads(line[6:]) for line in raw.splitlines()
                              if line.startswith('data: ')][-1]['response']
                    if turn == 0:
                        body['input'] += result['output'] + [
                            {'type': 'function_call_output', 'call_id': result['output'][0]['call_id'],
                             'output': 'FIXTURE_TOOL_OK'}]
                state_file = next(Path(directory).glob('*.json'))
                state = json.loads(state_file.read_text())
                if os.environ.get('CARRY_TEST_EVIDENCE'):
                    events = [json.loads(line) for line in state_file.with_suffix('.jsonl').read_text().splitlines()]
                    Path(os.environ['CARRY_TEST_EVIDENCE']).write_text(json.dumps({
                        'fixture_only': True, 'primary_calls': len(fixture.calls),
                        'classifier_calls': len(fixture.reviews), 'provider_errors': fixture.errors,
                        'invalid_reviews': state['invalid_reviews'],
                        'shadow_failures': [e['data'] for e in events if e['event'] == 'shadow_failed']
                    }, indent=2) + '\n')
                self.assertEqual(len(fixture.reviews), 1, 'must execute a real classifier HTTP request')
                self.assertEqual(fixture.reviews[0]['text']['format']['type'], 'json_object')
                self.assertEqual(len(fixture.reviews[0]['prompt_cache_key']), 64)
                self.assertEqual(state['invalid_reviews'], 0,
                                 f'JSON-mode review rejected: {fixture.errors}')
                self.assertFalse(fixture.errors)
            finally:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait(timeout=3)
                process.stdout.close()

    def test_classifier_rejects_present_cache_keys_above_provider_character_limit(self):
        from scripts.proxy_native_fixture import Fixture
        import urllib.error
        for key in ('x' * 65, 'carry-review-' + 'a' * 64, None, 42, True, []):
            with self.subTest(key=key), Fixture('codex', 'compact') as fixture:
                req = urllib.request.Request(fixture.url + '/classifier',
                    data=json.dumps({'input': [], 'prompt_cache_key': key}).encode(),
                    headers={'content-type': 'application/json'})
                with self.assertRaises(urllib.error.HTTPError) as error:
                    with urllib.request.urlopen(req, timeout=3) as response:
                        response.read()
                self.assertEqual(error.exception.code, 400)
                value = json.loads(error.exception.read())
                error.exception.close()
                self.assertEqual(value['error']['param'], 'prompt_cache_key')
                self.assertEqual(value['error']['code'],
                    'string_above_max_length' if isinstance(key, str) else 'invalid_type')
                self.assertEqual(len(fixture.reviews), 1)

    def test_classifier_accepts_optional_cache_key_and_64_character_boundary(self):
        from scripts.proxy_native_fixture import Fixture
        for fields in ({}, {'prompt_cache_key': ''}, {'prompt_cache_key': 'x' * 64},
                       {'prompt_cache_key': 'é' * 64}):
            with self.subTest(fields=fields), Fixture('pi', 'compact') as fixture:
                req = urllib.request.Request(fixture.url + '/classifier',
                    data=json.dumps({'input': [], **fields}).encode(),
                    headers={'content-type': 'application/json'})
                with urllib.request.urlopen(req, timeout=3) as response:
                    self.assertEqual(response.status, 200)
                    self.assertEqual(json.load(response)['status'], 'completed')
                self.assertFalse(fixture.errors)

    def test_provider_emits_realistic_tool_and_final_sse_for_both_clients(self):
        from scripts.proxy_native_fixture import Fixture
        for client, name in [('codex','exec_command'),('pi','bash')]:
            with self.subTest(client=client), Fixture(client) as fixture:
                body = {'model':'gpt-6-luna','prompt_cache_key':'fixture-conversation','input':[{'role':'user','content':'fixture'}],
                        'tools':[{'type':'function','name':name}]}
                def call(body):
                    request = urllib.request.Request(fixture.url+'/v1/responses',
                        data=json.dumps(body).encode(), headers={'content-type':'application/json'})
                    with urllib.request.urlopen(request, timeout=3) as response:
                        raw = response.read().decode()
                    return [json.loads(line[6:]) for line in raw.splitlines() if line.startswith('data: ')]
                events = call(body)
                response = events[-1]['response']
                self.assertEqual(events[-1]['type'], 'response.completed')
                tool = response['output'][0]
                self.assertEqual(tool['name'], name)
                body['input'].extend([tool, {'type':'function_call_output','call_id':tool['call_id'],
                                           'output':'FIXTURE_TOOL_OK'}])
                events = call(body)
                self.assertEqual(events[-1]['response']['output'][0]['content'][0]['text'], 'FIXTURE_COMPLETE')
                self.assertEqual(len(fixture.calls), 2)
                self.assertEqual(events[-1]['response']['usage']['input_tokens'], 1000)


    def test_compact_trajectory_executes_parallel_cohort_and_valid_classifier(self):
        from scripts.proxy_native_fixture import Fixture
        import subprocess
        import tempfile
        from pathlib import Path
        with Fixture('pi') as fixture, tempfile.TemporaryDirectory() as directory:
            fixture.case='compact'
            body={'model':'gpt-6-luna','prompt_cache_key':'fixture-conversation',
                  'tools':[{'name':'bash'}],'input':[{'role':'user','content':'fixture'}]}
            def call(path, body):
                req=urllib.request.Request(fixture.url+path,data=json.dumps(body).encode(),
                    headers={'content-type':'application/json'})
                with urllib.request.urlopen(req,timeout=3) as response:
                    raw=response.read().decode()
                if path == '/classifier': return json.loads(raw)
                return [json.loads(line[6:]) for line in raw.splitlines() if line.startswith('data: ')][-1]['response']
            first=call('/v1/responses',body)
            self.assertEqual(len(first['output']),2)
            for item in first['output']:
                run=subprocess.run(json.loads(item['arguments'])['command'],shell=True,
                    cwd=directory,capture_output=True,text=True,check=True)
                body['input'].extend([item,{'type':'function_call_output',
                    'call_id':item['call_id'],'output':run.stdout}])
            self.assertEqual((Path(directory)/'proxy-fixture.txt').read_text(),'FIXTURE_TOOL_OK')
            second=call('/v1/responses',body)
            advice=call('/classifier',{'input':[{'role':'user','content':json.dumps({
                'group_id':'g7','member_ids':[7,8,9],'items':first['output']+[
                    {'type':'function_call_output','call_id':'call_fixture_1_a','output':'DROP_COHORT_PAYLOAD'}]})},
                {'role':'user','content':json.dumps({'eligible_group_ids':['g7']})}]})
            self.assertEqual(json.loads(advice['output'][0]['content'][0]['text']),
                {'protected':[],'removable':['g7'],'memories':[]})
            self.assertEqual(second['output'][0]['type'],'function_call')
            # Removed whole cohort: the provider must accept the compact projection.
            body['input']=body['input'][:1]+second['output']+[{'type':'function_call_output',
                'call_id':second['output'][0]['call_id'],'output':'small'}]
            third=call('/v1/responses',body)
            self.assertEqual(third['output'][0]['type'],'function_call')
            body['input']+=third['output']+[{'type':'function_call_output',
                'call_id':third['output'][0]['call_id'],'output':'small'}]
            final=call('/v1/responses',body)
            self.assertEqual(final['output'][0]['content'][0]['text'],'FIXTURE_COMPLETE')
            self.assertFalse(fixture.errors)

    def test_native_compaction_routes_return_real_checkpoint_items(self):
        from scripts.proxy_native_fixture import Fixture
        for case in ('v1','v2'):
            with self.subTest(case=case), Fixture('codex',case) as fixture:
                body={'model':'gpt-6-luna','prompt_cache_key':'fixture-conversation',
                      'input':[{'role':'user','content':'fixture'}],'tools':[{'name':'exec_command'}]}
                def call(path):
                    req=urllib.request.Request(fixture.url+path,data=json.dumps(body).encode(),
                        headers={'content-type':'application/json'})
                    with urllib.request.urlopen(req,timeout=3) as response:
                        raw=response.read().decode()
                    if case=='v1' and path.endswith('/compact'): return json.loads(raw)
                    return [json.loads(line[6:]) for line in raw.splitlines() if line.startswith('data: ')][-1]['response']
                first=call('/v1/responses')
                self.assertGreater(first['usage']['total_tokens'],20000)
                body['input'].append({'type':'compaction_trigger'}) if case=='v2' else None
                compact=call('/v1/responses/compact' if case=='v1' else '/v1/responses')
                self.assertEqual(compact['output'][0]['type'],'compaction')
                body['input']=[{'role':'user','content':'continue'}]+compact['output']
                final=call('/v1/responses')
                self.assertEqual(final['output'][0]['content'][0]['text'],'FIXTURE_COMPLETE')
                self.assertFalse(fixture.errors)

    def test_pi_summary_is_cache_isolated_and_returns_native_text_checkpoint(self):
        from scripts.proxy_native_fixture import Fixture
        with Fixture('pi','pi-checkpoint') as fixture:
            body={'model':'gpt-6-luna','input':[{'role':'user','content':'summarize controlled history'}]}
            req=urllib.request.Request(fixture.url+'/v1/responses',data=json.dumps(body).encode(),
                headers={'content-type':'application/json'})
            try:
                with urllib.request.urlopen(req,timeout=3) as response: raw=response.read().decode()
            except urllib.error.HTTPError as error:
                error.close(); self.fail('Pi native summary must be accepted without a coding cache namespace')
            value=[json.loads(line[6:]) for line in raw.splitlines() if line.startswith('data: ')][-1]['response']
            self.assertEqual(value['output'][0]['content'][0]['text'],'FIXTURE_PI_CHECKPOINT')

    def test_provider_rejects_missing_or_changed_native_cache_namespace(self):
        from scripts.proxy_native_fixture import Fixture
        import urllib.error
        with Fixture('pi') as fixture:
            def call(body):
                req=urllib.request.Request(fixture.url+'/v1/responses',data=json.dumps(body).encode(),
                    headers={'content-type':'application/json'})
                return urllib.request.urlopen(req,timeout=3)
            body={'model':'gpt-6-luna','tools':[{'name':'bash'}],'input':[]}
            with self.assertRaises(urllib.error.HTTPError) as error: call(body)
            self.assertEqual(error.exception.code,400); error.exception.close()
            body['prompt_cache_key']='one-conversation'
            with call(body) as response: response.read()
            body['prompt_cache_key']='another-conversation'
            with self.assertRaises(urllib.error.HTTPError) as error: call(body)
            self.assertEqual(error.exception.code,400); error.exception.close()
            self.assertEqual(len(fixture.calls),1)


if __name__ == '__main__':
    unittest.main()
