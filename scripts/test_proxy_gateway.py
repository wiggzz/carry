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
    def __init__(self, status=200, value=None):
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
                payload=json.dumps(value or {'error':{'message':CANARY}}).encode()
                self.send_response(status); self.send_header('content-length',str(len(payload)))
                self.end_headers(); self.wfile.write(payload)
        self.server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    def __enter__(self):
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True); self.thread.start()
        return self
    def __exit__(self,*args):
        self.server.shutdown(); self.server.server_close(); self.thread.join(3)


class TrustedGatewayTests(unittest.TestCase):
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
