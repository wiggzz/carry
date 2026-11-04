"""HTTPS loopback provider; synthetic data only, never invokes a model."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import ssl
import uuid


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        import os
        assert self.headers.get('Authorization') == 'Bearer '+os.environ['FIXTURE_EXPECTED_KEY']
        payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        # Fail rather than pretend to implement a paid reviewer response.
        assert payload.get('model') == 'gpt-6-luna'
        data = {'id': 'resp_'+uuid.uuid4().hex, 'object': 'response', 'status': 'completed',
                'model': payload['model'],
                'output': [{'id': 'msg_'+uuid.uuid4().hex, 'type': 'message', 'status': 'completed',
                            'role': 'assistant', 'content': [{'type': 'output_text', 'text': 'synthetic fixture', 'annotations': []}]}],
                'usage': {'input_tokens': 30, 'output_tokens': 3, 'total_tokens': 33,
                          'input_tokens_details': {'cached_tokens': 0},
                          'output_tokens_details': {'reasoning_tokens': 0}}}
        raw = json.dumps(data).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers(); self.wfile.write(raw)
        print(json.dumps({'evidence_kind': 'synthetic-fixture', 'request': payload, 'receipt': data}), flush=True)


server = ThreadingHTTPServer(('0.0.0.0', 443), Provider)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain('/opt/provider-cert.pem', '/opt/fixture-key.pem')
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()
