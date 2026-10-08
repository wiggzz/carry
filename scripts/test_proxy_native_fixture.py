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
                'group_id':'g7','eligible':True,'items':first['output']+[
                    {'type':'function_call_output','call_id':'call_fixture_1_a','output':'DROP_COHORT_PAYLOAD'}]})}]})
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
