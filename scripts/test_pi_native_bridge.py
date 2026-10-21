"""Behavioral regressions for the isolated trusted Pi bridge; no model/build."""
import http.client
import http.server
import json
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
import uuid

from scripts.pi_native_bridge import PiNativeBridge


def terminal(text='', status='completed'):
    return ('event: response.'+status+'\ndata: '+json.dumps({
        'type':'response.'+status, 'response':{'status':status,
        'output':[{'text':text}]}})+'\n\n').encode()


class PiBridgeCompletionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.mapping = Path(self.directory.name)/'mapping.json'
        self.seen = []
        self.payload = terminal()
        self.expires = int(time.time())+300
        outer = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_POST(self):
                body = self.rfile.read(int(self.headers['content-length']))
                outer.seen.append((self.path, dict(self.headers), body))
                if self.path.endswith('/match'):
                    raw = json.dumps({'schema_version':1,
                        'matching':'ordered-prefix-cohort-scope-v1', 'outcome':'matched',
                        'points':[{'id':'SYNTHETIC_OPAQUE_POINT','input_items':1,
                                   'expires_at':outer.expires}]}).encode()
                else: raw = outer.payload
                self.send_response(200)
                self.send_header('content-type','text/event-stream')
                self.send_header('content-length',str(len(raw)))
                self.end_headers()
                # Deliberately fragment field names, JSON and frame delimiters.
                for offset in range(0,len(raw),113):
                    self.wfile.write(raw[offset:offset+113]); self.wfile.flush()
                    if offset == 0 and getattr(outer,'gate',None):
                        if not outer.gate.wait(5): raise TimeoutError('stream release')
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.thread = threading.Thread(target=self.server.serve_forever,daemon=True)
        self.thread.start()
        def close():
            self.server.shutdown(); self.server.server_close(); self.thread.join(3)
        self.addCleanup(close)
        self.target = 'http://127.0.0.1:'+str(self.server.server_port)
        self.bridge = self.open_bridge()
        self.parent, self.child = str(uuid.uuid4()), str(uuid.uuid4())
        self.bridge.resume(self.parent)
        self.body = b' {"model":"synthetic", "input":[{"role":"user","content":"exact original"}]} '
        self.assertEqual(self.post(self.bridge), (200,self.payload))
        tree = {'entries':[{'id':'a','parentId':None}], 'leafId':'a'}
        self.bridge.observe_boundary(self.parent,tree)
        self.bridge.fork(self.parent,self.child,tree)

    def open_bridge(self):
        bridge = PiNativeBridge(self.target,'synthetic',tenant='fixed',session='family',mapping=self.mapping)
        bridge.__enter__()
        self.addCleanup(bridge.__exit__,None,None,None)
        return bridge

    def post(self, bridge):
        connection = http.client.HTTPConnection('127.0.0.1',bridge.server.server_port,timeout=5)
        try:
            connection.request('POST','/v1/responses',self.body,{'authorization':'Bearer synthetic'})
            response = connection.getresponse()
            result = response.status,response.read()
            # HTTP read can finish before the handler's state transition.
            with bridge.lock: pass
            return result
        finally: connection.close()

    def check_completed(self, payload):
        self.payload = payload
        self.assertEqual(self.post(self.bridge),(200,payload), 'response bytes must remain exact')
        with self.bridge.lock:
            self.assertIsNone(self.bridge.row()['intent'], 'completed child must retire creation intent')
            self.assertEqual(self.bridge.last_body,(self.child,self.body))
            self.assertIsNone(json.loads(self.mapping.read_bytes())['natives'][self.child]['intent'])
        for bridge in (self.bridge,self.open_bridge()):
            bridge.resume(self.child)
            before = len(self.seen)
            with patch('scripts.pi_native_bridge.time.time',return_value=self.expires+1):
                self.assertEqual(self.post(bridge),(200,payload))
            self.assertEqual(len(self.seen),before+1)
            headers = {k.lower():v for k,v in self.seen[-1][1].items()}
            self.assertFalse(any(k.startswith('x-carry-fork-') for k in headers))
            self.assertEqual(self.seen[-1][2],self.body)

    def test_large_completed_http_retires_persists_and_resumes_after_expiry(self):
        payload = terminal('X'*70000)
        self.assertGreater(len(payload),65536)
        self.check_completed(payload)

    def test_small_completed_http_control(self):
        self.check_completed(terminal('small'))

    def test_failed_incomplete_malformed_and_spoofed_http_keep_durable_intent(self):
        negatives = [terminal(status='failed'),terminal(status='incomplete'),
            b'data: {"type":"response.completed","response":{"status":"completed"},}\n\n',
            b'data: {"output":{"type":"response.completed","response":{"status":"completed"}}}\n\n',
            terminal('response.completed status completed',status='failed'),
            terminal()[:-2],terminal()+terminal(status='failed')]
        saved = self.mapping.read_bytes()
        for payload in negatives:
            with self.subTest(payload=payload):
                self.payload = payload
                self.assertEqual(self.post(self.bridge),(200,payload))
                with self.bridge.lock:
                    self.assertIsNotNone(self.bridge.row()['intent'])
                    self.assertIsNone(self.bridge.last_body)
                    self.assertEqual(self.mapping.read_bytes(),saved)
        before = len(self.seen)
        with patch('scripts.pi_native_bridge.time.time',return_value=self.expires+1):
            self.assertEqual(self.post(self.bridge)[0],409)
        self.assertEqual(len(self.seen),before,'no failed-intent fresh fallback')

    def test_response_prefix_streams_before_upstream_finishes(self):
        self.payload = terminal('X'*70000)
        self.gate = threading.Event()
        connection = http.client.HTTPConnection('127.0.0.1',self.bridge.server.server_port,timeout=3)
        try:
            connection.request('POST','/v1/responses',self.body,{'authorization':'Bearer synthetic'})
            response = connection.getresponse()
            prefix = response.read1(113)
            self.assertTrue(prefix)
            self.assertEqual(prefix,self.payload[:len(prefix)])
            self.assertFalse(self.gate.is_set())
            self.gate.set()
            self.assertEqual(prefix+response.read(),self.payload)
            with self.bridge.lock: self.assertIsNone(self.bridge.row()['intent'])
        finally:
            self.gate.set(); connection.close()


class PiTerminalParserTests(unittest.TestCase):
    def parse(self, raw, chunks=1):
        from scripts.pi_terminal_sse import TerminalSSE
        parser = TerminalSSE()
        for offset in range(0,len(raw),chunks): parser.feed(raw[offset:offset+chunks])
        return parser.completed()

    def test_every_byte_split_and_sse_field_frame_forms(self):
        from scripts.pi_terminal_sse import TerminalSSE
        forms = [terminal('é \\" [] {}'),
            terminal('é 😀').replace(b'\\u00e9','é'.encode()).replace(b'\\ud83d\\ude00','😀'.encode()),
            b'data:{"response":{"status":"complet\\u0065d"},"ty\\u0070e":"response.complet\\u0065d"}\n\n',
            b':comment\r\nevent:response.completed\r\ndata: {"response":\r\ndata: {"status":"completed"},\r\ndata: "type":"response.completed"}\r\n\r\n',
            terminal().replace(b'\n',b'\r'),terminal()+b'data: [DONE]\n\n']
        for raw in forms:
            self.assertTrue(self.parse(raw),raw)
            for split in range(len(raw)+1):
                parser = TerminalSSE(); parser.feed(raw[:split]); parser.feed(raw[split:])
                self.assertTrue(parser.completed(),(raw,split))

    def test_full_json_validity_and_selector_spoof_negatives(self):
        values = [b'{"type":"response.completed","response":{"status":"completed"},"x":'+v+b'}'
                  for v in [b'01',b'1.',b'1e',b'+1',b'NaN',b'Infinity',b'truee',b'[1,]',
                            b'{"x":1,}',b'{[]}',b'"bad\\q"',b'"bad\\u000x"',b'"control\x01"']]
        values += [b'{"type":"response.completed","response":{"status":"completed"}} trailing',
            b'{"type":"response.completed","response":{"status":"completed"},"type":"response.completed"}',
            b'{"type":"response.completed","response":{"status":"completed","status":"failed"}}',
            b'{"type":"response.completed","response":{"status":"completed"},"response":{}}',
            b'{"type":"response.completed","response":{"nested":{"status":"completed"}}}',
            b'{"type":"response.completed","response":{"status":"completed!"}}',
            b'{"type":"response.completed!","response":{"status":"completed"}}',
            b'{"type":"response.completed","response":{"status":"completed"},"bad":"\xff"}']
        for value in values:
            with self.subTest(value=value): self.assertFalse(self.parse(b'data: '+value+b'\n\n'))
        self.assertFalse(self.parse(terminal()+b'data: {\n\n'))
        self.assertFalse(self.parse(terminal()+terminal(status='incomplete')))
        self.assertTrue(self.parse(terminal(status='failed')+terminal()))

    def test_stdlib_json_differential_and_long_spoofed_output(self):
        # Execute full-document validity differentials, not source-text checks.
        from scripts.pi_terminal_sse import TerminalSSE
        raw = json.dumps({'type':'response.completed','response':{'status':'completed'},
            'other':{'a':[None,False,True,0,-12,1.5,1e-20,'a"b\\c\né']}}).encode()
        mutations = [raw[:i]+raw[i+1:] for i in range(len(raw))]
        mutations += [raw[:i]+c+raw[i:] for i in range(len(raw)+1)
                      for c in (b':',b',',b'0',b'"',b'\\',b']')]
        for value in mutations:
            try:
                decoded = json.loads(value)
                expected = (decoded.get('type')=='response.completed' and
                    isinstance(decoded.get('response'),dict) and decoded['response'].get('status')=='completed')
            except (ValueError,AttributeError): expected = False
            parser = TerminalSSE(); parser.feed(b'data: '+value+b'\n\n')
            self.assertEqual(parser.completed(),expected,value)
        self.assertFalse(self.parse(terminal('X'*70000+' response.completed completed',status='failed'),4096))
        invalid = terminal('X'*70000).replace(b'X"',b'X\\q"')
        self.assertFalse(self.parse(invalid,4096))

    def test_large_arrays_numbers_strings_and_bounded_depth(self):
        body = {'type':'response.completed','response':{'status':'completed'},
            'discard':[None,True,False,-2,0,1.25,1e20,{'status':'failed','type':'spoof'}]*2000}
        raw = b'data: '+json.dumps(body).encode()+b'\n\n'
        self.assertGreater(len(raw),65536)
        for size in (1,17,65536): self.assertTrue(self.parse(raw,size))
        # Numbers themselves are not accumulated, even beyond json.loads' int limit.
        self.assertTrue(self.parse(b'data: {"type":"response.completed","response":{"status":"completed"},"n":'+b'9'*70000+b'}\n\n',4096))
        self.assertFalse(self.parse(b'data: {"type":"response.completed","response":{"status":"completed"},"x":'+b'['*129+b'0'+b']'*129+b'}\n\n'))

    def test_long_discarded_output_has_constant_memory(self):
        import tracemalloc
        from scripts.pi_terminal_sse import TerminalSSE
        parser = TerminalSSE()
        tracemalloc.start()
        try:
            parser.feed(b'data: {"type":"response.completed","response":{"status":"completed","output":"')
            chunk = b'x'*8192
            for _ in range(192): parser.feed(chunk)
            parser.feed(b'"}}\n\n')
            self.assertTrue(parser.completed())
            _,peak = tracemalloc.get_traced_memory()
            self.assertLess(peak,131072,'observer must not retain a full event/response')
        finally: tracemalloc.stop()


class PiMappingModeTests(unittest.TestCase):
    def test_save_hardens_existing_temporary_and_fresh_file(self):
        with tempfile.TemporaryDirectory() as directory:
            for preexisting in (False,True):
                with self.subTest(preexisting=preexisting):
                    mapping = Path(directory)/str(preexisting)/'mapping.json'
                    mapping.parent.mkdir()
                    temporary = mapping.with_suffix('.tmp')
                    if preexisting:
                        temporary.write_text('old temporary')
                        temporary.chmod(0o644)
                    bridge = PiNativeBridge('http://127.0.0.1:1','synthetic',
                        tenant='fixed',session='family',mapping=mapping)
                    try:
                        native = str(uuid.uuid4())
                        bridge.resume(native)
                        self.assertEqual(mapping.stat().st_mode & 0o777,0o600)
                        self.assertIn(native,json.loads(mapping.read_bytes())['natives'])
                        saved = mapping.read_bytes()
                        with patch('scripts.pi_native_bridge.os.fchmod',side_effect=OSError('mode denied')):
                            with self.assertRaises(OSError): bridge.save()
                        self.assertEqual(mapping.read_bytes(),saved,'failed hardening must not publish')
                    finally: bridge.server.server_close()


class PiParentRawProofTests(unittest.TestCase):
    def test_exact_target_snapshots_and_format_only_mutation_rejection(self):
        from scripts.proxy_pi_fork_fixture import ParentCheckpointProof
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root/'exact-parent.json'
            raw = b' {"completed_requests" : 5, "memories" : []}\n\n'
            target.write_bytes(raw)
            (root/'decoy.json').write_text('{}')
            proof = ParentCheckpointProof(root,target,json.loads(raw))
            proof.check('after-child')
            proof.check('after-resume')
            manifest = json.loads((root/'parent-raw-proof.json').read_bytes())
            self.assertEqual(manifest['target'],str(target))
            self.assertEqual(len(manifest['snapshots']),3)
            for row in manifest['snapshots']:
                self.assertEqual((root/row['file']).read_bytes(),raw)
                self.assertEqual(row['sha256'],hashlib.sha256(raw).hexdigest())
                self.assertEqual(row['bytes'],len(raw))
                self.assertTrue(row['raw_equal'] and row['parsed_equal'])
            # Equal parsed JSON must NOT conceal a formatting-only disk rewrite.
            changed = json.dumps(json.loads(raw)).encode()
            target.write_bytes(changed)
            with self.assertRaisesRegex(AssertionError,'parent raw bytes changed'):
                proof.check('mutated')
            self.assertEqual((root/'parent-b-mutated.raw.json').read_bytes(),changed)
            row = json.loads((root/'parent-raw-proof.json').read_bytes())['snapshots'][-1]
            self.assertFalse(row['raw_equal']); self.assertTrue(row['parsed_equal'])
