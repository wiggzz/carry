"""Exercise the credential-free native-client provider fixture over HTTP."""
import json
import unittest
import urllib.request


class NativeProviderFixtureTests(unittest.TestCase):
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
