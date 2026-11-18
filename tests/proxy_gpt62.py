"""Real binary, synthetic subscription SSE/tool continuation and restart."""
import base64
import unittest
import proxy_state_regressions as support


class GPT62Wire(support.FixtureCase):
    def lifecycle(self, model):
        auth = self.state / 'synthetic-auth'
        auth.mkdir()
        payload = base64.urlsafe_b64encode(support.wire({
            'https://api.openai.com/auth': {'chatgpt_account_id': 'fixture-account'}
        })).decode().rstrip('=')
        credential = auth / 'auth.json'
        credential.write_bytes(support.wire({
            'version': 1, 'access_token': f'header.{payload}.signature',
            'refresh_token': 'fixture-refresh', 'expires_at_ms': 4102444800000
        }))
        credential.chmod(0o600)
        call = {'type': 'function_call', 'id': 'fc_gpt62', 'call_id': 'call_gpt62',
                'name': 'synthetic_echo', 'arguments': '{"marker":"tool-marker"}'}
        result = {'type': 'function_call_output', 'call_id': 'call_gpt62',
                  'output': 'tool-marker'}
        user = {'role': 'user', 'content': 'Use synthetic_echo, then report its marker.'}

        def primary(body, number):
            self.assertEqual(body['model'], model)
            self.assertEqual(body['reasoning']['effort'], 'low')
            self.assertFalse(body['store'])
            self.assertTrue(body['stream'])
            self.assertNotIn('max_output_tokens', body)
            self.assertNotIn('prompt_cache_options', body)
            if number == 2:
                self.assertEqual(body['input'], [user, call, result])
            response = support.completed([call] if number == 1 else [{
                'type': 'message', 'role': 'assistant',
                'content': [{'type': 'output_text', 'text': 'tool-marker'}]
            }], response_id=f'resp_gpt62_{number}')
            return 200, b'data: ' + support.wire({
                'type': 'response.completed', 'response': response
            }) + b'\n\n'

        def review(*_):
            raise AssertionError('Tiny fixture must not dispatch a reviewer')

        fixture = self.fixture(primary, review)
        options = ['--codex-login', '--codex-home', str(auth),
                   '--review-every-requests', '100']
        def start():
            proxy = support.Proxy(self.state, fixture, options, mode='audit')
            self.stack.callback(proxy.close)
            return proxy

        proxy = start()
        fields = dict(model=model, reasoning={'effort': 'low'}, max_output_tokens=128,
                      prompt_cache_options={'mode': 'implicit', 'ttl': '30m'})
        status, first = proxy.send([user], **fields)
        self.assertEqual(status, 200)
        self.assertEqual(first['output'], [call])
        proxy.close()
        proxy = start()
        status, second = proxy.send([result], previous_response_id=first['id'], **fields)
        self.assertEqual(status, 200)
        self.assertEqual(second['output'][0]['content'][0]['text'], 'tool-marker')
        self.assertEqual(len(fixture.primaries), 2)
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(fixture.errors, [])
        saved = proxy.saved()
        self.assertEqual(saved['completed_requests'], 2)
        self.assertGreater(saved['primary']['cost_usd'], 0)
        self.evidence.update(model=model, primaries=fixture.primaries, checkpoint=saved)

    def test_sol_tool_restart_continuation(self):
        self.lifecycle('gpt-6.2-sol')

    def test_astra_tool_restart_continuation(self):
        self.lifecycle('gpt-6.2-astra')


if __name__ == '__main__':
    unittest.main()
