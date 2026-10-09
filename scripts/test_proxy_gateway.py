"""Actual production Node handler; fixed-host fixture transport, no provider calls."""
import http.server
import json
import os
from pathlib import Path
import select
import subprocess
import threading
import tempfile
import time
import unittest
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
CANARY = 'PRIVATE_CONTENT_CANARY_7f809e'


from scripts.proxy_gateway_fixture_support import Gateway

class Provider:
    def __init__(self, status=200, value=None, response_bytes=None):
        self.calls = []
        outer = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_POST(self):
                # Streaming gateway requests have chunked transfer encoding.
                if self.headers.get('transfer-encoding') == 'chunked':
                    chunks=[]
                    while True:
                        size=int(self.rfile.readline().strip(),16)
                        if not size:
                            self.rfile.readline(); break
                        chunks.append(self.rfile.read(size)); self.rfile.read(2)
                    raw=b''.join(chunks)
                else:
                    raw=self.rfile.read(int(self.headers.get('content-length','0')))
                outer.calls.append({'body':raw,'headers':dict(self.headers),'path':self.path})
                payload = response_bytes if response_bytes is not None else json.dumps(value or {'error':{'message':CANARY}}).encode()
                self.send_response(status); self.send_header('content-length',str(len(payload)))
                self.end_headers(); self.wfile.write(payload)
        self.server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    def __enter__(self):
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True); self.thread.start()
        return self
    def __exit__(self,*args):
        self.server.shutdown(); self.server.server_close(); self.thread.join(3)


class TrustedGatewayTests(unittest.TestCase):
    def request_path(self, gateway, body, path, token='fixture-client'):
        request = urllib.request.Request(gateway.url + path, data=body,
            headers={'Authorization': 'Bearer ' + token})
        try:
            with urllib.request.urlopen(request, timeout=3) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            with error:
                return error.code, error.read()

    def test_hosted_tools_are_denied_before_primary_or_shadow_transport(self):
        with Provider() as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port)
            try:
                for tool_type in ('web_search', 'web_search_preview', 'file_search',
                                  'code_interpreter', 'computer', 'computer_use_preview',
                                  'mcp', 'image_generation', 'future_hosted_tool'):
                    for path in ('/v1/responses', '/v1/responses/compact', '/v1/responses?fixture=1'):
                        for token in ('fixture-client', 'fixture-shadow') if path == '/v1/responses' else ('fixture-client',):
                            with self.subTest(tool_type=tool_type, path=path, token=token):
                                body = json.dumps({'model': 'gpt-6-luna', 'input': [],
                                    'tools': [{'type': tool_type, 'name': CANARY}]}).encode()
                                status, raw = self.request_path(gateway, body, path, token)
                                self.assertEqual(status, 400)
                                self.assertNotIn(CANARY.encode(), raw)
                                self.assertEqual(provider.calls, [])
            finally:
                gateway.close()
            self.assertNotIn(CANARY, gateway.stdout + gateway.stderr)

    def test_client_tool_names_arguments_namespaces_and_response_bytes_are_opaque(self):
        tools = [{'type': 'function', 'name': 'web_search',
                  'parameters': {'type': 'object', 'properties': {CANARY: {'type': 'string'}}}},
                 {'type': 'custom', 'name': 'mcp', 'format': {'type': 'text'}},
                 {'type': 'namespace', 'name': 'computer', 'tools': [
                     {'type': 'namespace', 'name': 'image_generation', 'tools': [
                         {'type': 'function', 'name': 'file_search', 'strict': False,
                          'parameters': None}, {'type': 'custom', 'name': CANARY}]}]}]
        # Intentionally noncanonical JSON, escaped Unicode and opaque tool arguments.
        body = ('  {"model":"gpt-6-luna", "tools":' + json.dumps(tools) +
                ', "input":[{"type":"function_call","name":"web_search",'
                '"call_id":"arbitrary", "arguments":"{\\"x\\":\\"\\\\u2603\\"}"}]}  ').encode()
        for reply in (b' { "output": [], "unknown": "\\u2603" } ',
                      b'event: native.opaque\r\ndata: { "arguments": "literal\\nbytes" }\r\n\r\ndata: [DONE]\n\n'):
            with self.subTest(reply=reply), Provider(response_bytes=reply) as provider:
                gateway = Gateway(provider.server.server_port, provider.server.server_port)
                try:
                    for path in ('/v1/responses', '/v1/responses/compact'):
                        status, raw = self.request_path(gateway, body, path)
                        self.assertEqual(status, 200)
                        self.assertEqual(raw, reply)
                        self.assertEqual(provider.calls[-1]['body'], body)
                        self.assertEqual(provider.calls[-1]['path'], path)
                finally:
                    gateway.close()
                self.assertNotIn(CANARY, gateway.stdout + gateway.stderr)

    def test_nested_hosted_and_malformed_tools_fail_closed(self):
        malformed = [None, {}, 'tools', [None], [[]], ['function'], [{}],
            [{'type': 'function'}], [{'type': 'custom', 'name': ''}],
            [{'type': 'function', 'name': 1}], [{'type': 'function', 'name': 'ok', 'parameters': []}],
            [{'type': 'function', 'name': 'ok', 'strict': 'true'}],
            [{'type': 'custom', 'name': 'ok', 'format': []}],
            [{'type': 'namespace', 'name': 'ns'}],
            [{'type': 'namespace', 'name': 'ns', 'tools': None}],
            [{'type': 'namespace', 'name': 'ns', 'tools': [{'type': 'namespace', 'name': 'deep',
                'tools': [{'type': 'web_search'}]}]}],
            [{'type': 'function', 'name': 'ok'}, {'type': 'unknown', 'name': 'ok'}]]
        with Provider() as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port)
            try:
                for tools in malformed:
                    for path in ('/v1/responses', '/v1/responses/compact'):
                        with self.subTest(tools=tools, path=path):
                            status, _ = self.request_path(gateway,
                                json.dumps({'model': 'gpt-6-luna', 'tools': tools}).encode(), path)
                            self.assertEqual(status, 400)
                            self.assertEqual(provider.calls, [])
                for body in (b'{', b'null', b'[]', b'"request"'):
                    with self.subTest(body=body):
                        self.assertEqual(gateway.request(body)[0], 400)
                        self.assertEqual(provider.calls, [])
            finally:
                gateway.close()

    def test_missing_empty_tools_and_empty_namespace_are_allowed(self):
        with Provider() as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port)
            try:
                for extra in ({}, {'tools': []}, {'tools': [{'type': 'namespace', 'name': 'empty', 'tools': []}]}):
                    with self.subTest(extra=extra):
                        body = json.dumps({'model': 'gpt-6-luna', 'input': [], **extra}).encode()
                        self.assertEqual(gateway.request(body)[0], 200)
                        self.assertEqual(provider.calls[-1]['body'], body)
            finally:
                gateway.close()

    def test_requests_above_telemetry_budget_keep_existing_proxy_body_admission(self):
        # The proxy already admits up to 16 MiB; its telemetry budget is not a new input cap.
        body = json.dumps({'model': 'gpt-6-luna', 'input': 'x' * (9 * 1024 * 1024)}).encode()
        with Provider(response_bytes=b'{"output":[]}') as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port)
            try:
                status, raw = gateway.request(body)
                self.assertEqual(status, 200)
                self.assertEqual(len(provider.calls), 1)
                self.assertTrue(provider.calls[0]['body'] == body)
                self.assertEqual(raw, b'{"output":[]}')
            finally:
                gateway.close()

    def test_direct_gateway_waits_for_complete_body_before_any_upstream_request(self):
        script = r'''
const assert = require('node:assert/strict');
const http = require('node:http');
const {serve} = require(process.argv[1]);
const calls = [];
let upstreamRequests = 0;
const reply = Buffer.from('event: opaque\r\ndata: {"arguments":"unchanged\\nbytes"}\r\n\r\n');
const provider = http.createServer((request, response) => {
  const chunks = [];
  request.on('data', chunk => chunks.push(chunk));
  request.on('end', () => { calls.push(Buffer.concat(chunks)); response.end(reply); });
});
const listen = server => new Promise(resolve => server.on('listening', resolve));
async function main() {
  provider.listen(0, '127.0.0.1'); await listen(provider);
  const gateway = serve({port:0, host:'127.0.0.1', request:(options, callback) => {
    upstreamRequests++;
    return http.request({...options, hostname:'127.0.0.1', port:provider.address().port}, callback);
  }});
  await listen(gateway);
  const send = (body, path='/v1/responses', partial=false) => new Promise((resolve, reject) => {
    const request = http.request({hostname:'127.0.0.1', port:gateway.address().port,
      method:'POST', path, headers:{'content-length':Buffer.byteLength(body)}}, response => {
      const chunks = []; response.on('data', chunk => chunks.push(chunk));
      response.on('end', () => resolve([response.statusCode, Buffer.concat(chunks)]));
    });
    request.on('error', reject);
    if (partial) {
      request.write(body.slice(0, 8));
      setTimeout(() => {
        try { assert.equal(upstreamRequests, 0); request.end(body.slice(8)); }
        catch (error) { request.destroy(); reject(error); }
      }, 30);
    } else request.end(body);
  });
  try {
    const denied = '{"model":"gpt-6-luna","tools":[{"type":"web_search"}]}';
    assert.equal((await send(denied, '/v1/responses', true))[0], 400);
    assert.equal((await send(denied, '/v1/responses/compact'))[0], 400);
    assert.equal((await send('{"input":"' + 'x'.repeat(16*1024*1024) + '"}'))[0], 413);
    assert.equal(upstreamRequests, 0); assert.equal(calls.length, 0);
    const body = ' {"tools":[{"type":"function","name":"web_search"}], "input":[]} ';
    const [status, raw] = await send(body);
    assert.equal(status, 200); assert.deepEqual(raw, reply);
    assert.equal(upstreamRequests, 1); assert.deepEqual(calls, [Buffer.from(body)]);
    console.log(JSON.stringify({denied_upstream_requests:0, allowed_upstream_requests:1,
      complete_body_gate:true, request_response_bytes_preserved:true}));
  } finally {
    await new Promise(resolve => gateway.close(resolve));
    await new Promise(resolve => provider.close(resolve));
  }
}
main().catch(error => { console.error(error); process.exitCode=1; });
'''
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('OPENAI_', 'CARRY_', 'BENCHMARK_'))}
        run = subprocess.run(['node', '-e', script, str(ROOT / 'scripts/openai_proxy.js')],
            env=env, text=True, capture_output=True, timeout=20)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        result = json.loads(run.stdout)
        self.assertEqual(result['denied_upstream_requests'], 0)
        self.assertEqual(result['allowed_upstream_requests'], 1)
        self.assertTrue(result['complete_body_gate'])
        self.assertTrue(result['request_response_bytes_preserved'])

    def test_operator_policy_replaces_all_caller_identity_and_policy_headers(self):
        for policy in (None,'strict','reset-on-divergence'):
            with self.subTest(policy=policy), Provider() as provider:
                gateway=Gateway(provider.server.server_port,provider.server.server_port,policy)
                try:
                    body=b' { "input" : [], "model": "gpt-6-luna" } '
                    status,raw=gateway.request(body, {'x-carry-session':'spoof','x-carry-tenant':'spoof',
                        'x-carry-branch':'spoof','x-carry-history-policy':'reset-on-divergence',
                        'x-carry-secret':CANARY})
                    self.assertEqual(status,200)
                    call=provider.calls[0]
                    headers={k.lower():v for k,v in call['headers'].items()}
                    self.assertEqual(call['body'],body)
                    self.assertEqual(headers['x-carry-session'],'fixture-session')
                    self.assertEqual(headers['x-carry-tenant'],'benchmark')
                    self.assertEqual(headers['x-carry-branch'],'main')
                    self.assertEqual(headers.get('x-carry-history-policy'),policy or 'strict')
                    self.assertNotIn('x-carry-secret',headers)
                    gateway.request(body, {'x-carry-history-policy':'strict'}, token='fixture-shadow')
                    self.assertFalse(any(k.lower().startswith('x-carry-') for k in provider.calls[-1]['headers']))
                finally:
                    gateway.close()

    def test_request_and_provider_content_cannot_enter_completed_or_400_telemetry(self):
        usage={'input_tokens':10,'output_tokens':2,'input_tokens_details':{'cached_tokens':0,
            'private':{'message':CANARY}},'output_tokens_details':{'reasoning_tokens':0},'private':CANARY}
        for status in (200,400):
            with self.subTest(status=status), Provider(status, {'model':CANARY*100,
                    'service_tier':{'private':CANARY},'output':[{'type':'function_call','arguments':CANARY}],
                    'usage':usage,'message':CANARY}) as provider:
                gateway=Gateway(provider.server.server_port,provider.server.server_port)
                body=json.dumps({'model':CANARY*100,'service_tier':{'private':CANARY},
                                 'input':[{'content':CANARY}]}).encode()
                try:
                    received,raw=gateway.request(body)
                    self.assertEqual(received,status)
                    self.assertEqual(provider.calls[0]['body'],body)
                finally:
                    gateway.close()
                self.assertNotIn(CANARY,gateway.stdout+gateway.stderr)
                terminal=[e for e in gateway.events if e['event'] in ('completed','censored')]
                self.assertEqual(len(terminal),1)
                self.assertEqual(terminal[0]['model'],'unrecognized')
                self.assertEqual(terminal[0]['service_tier'],'unrecognized')
                self.assertEqual(terminal[0]['event'],'completed' if status==200 else 'censored')
                if status==200:
                    from scripts.proxy_benchmark import summarize_events
                    self.assertIsNone(summarize_events(gateway.events)['estimated_total_cost_usd'])

    def test_invalid_tier_and_unknown_billing_are_not_default_priced(self):
        from scripts.proxy_benchmark import summarize_events
        ordinary={'input_tokens':10,'output_tokens':2,'input_tokens_details':{'cached_tokens':0}}
        for tier,usage in [(CANARY,ordinary),('default',{**ordinary,CANARY:CANARY}),
                           ('default',{**ordinary,'input_tokens_details':{'cached_tokens':0,CANARY:CANARY}})]:
            with self.subTest(tier=tier), Provider(200,{'model':'gpt-6-luna','service_tier':tier,
                    'usage':usage}) as provider:
                gateway=Gateway(provider.server.server_port,provider.server.server_port)
                try:
                    status,_=gateway.request(b'{"model":"gpt-6-luna"}')
                    self.assertEqual(status,200)
                finally: gateway.close()
                self.assertNotIn(CANARY,gateway.stdout+gateway.stderr)
                self.assertIsNone(summarize_events(gateway.events)['estimated_total_cost_usd'])
                self.assertEqual(summarize_events(gateway.events)['primary']['unpriced_requests'],1)

    def test_invalid_operator_policy_fails_before_listen(self):
        with Provider() as provider:
            gateway=None
            try:
                with self.assertRaisesRegex(RuntimeError,'before readiness'):
                    gateway=Gateway(provider.server.server_port,provider.server.server_port,'caller-decides')
            finally:
                if gateway is not None: gateway.close()
            self.assertEqual(provider.calls,[])


if __name__ == '__main__': unittest.main()
