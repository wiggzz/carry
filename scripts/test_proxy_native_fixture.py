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
def contract_proxy(test, provider, state_dir, mode='audit', binary=None, classifier_path='/review'):
    binary = Path(binary or os.environ.get('CARRY_TEST_BINARY', 'target/debug/carry')).resolve()
    if not binary.is_file():
        test.skipTest('requires an existing Carry binary; never builds locally')
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('OPENAI_', 'CARRY_PROXY_'))}
    home = Path(state_dir) / 'home'; home.mkdir(parents=True, exist_ok=True)
    env.update(HOME=str(home), CARRY_HOME=str(home/'.carry'), XDG_CONFIG_HOME=str(home/'.config'))
    # Explicit legacy treatment: exercise review mechanics on tiny synthetic inputs.
    env.update(CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS='0', CARRY_PROXY_REVIEW_MIN_NEW_TOKENS='0',
               CARRY_PROXY_REVIEW_BACKOFF_FACTOR='1', CARRY_PROXY_REVIEW_EVERY_REQUESTS='1')
    command = [str(binary), 'proxy', '--verbose', '--listen', '127.0.0.1:0',
               '--upstream-url', provider.url + '/v1/responses',
               '--classifier-url', provider.url + classifier_path, '--mode', mode,
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


def contract_send(url, body, directory):
    request = urllib.request.Request(url + '/v1/responses', data=json.dumps(body).encode(),
        headers={'content-type': 'application/json', 'x-carry-session': 'contract'})
    # Consume the primary first. These receipt/provenance tests deliberately
    # sequence completed reviews; production forwarding must never do this.
    with urllib.request.urlopen(request, timeout=30) as response:
        result = json.load(response)
    contract_settle(directory)
    return result


def contract_settle(directory):
    import time
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            state = contract_state(directory)
            if not state['background_review_pending']:
                return state
        except (StopIteration, FileNotFoundError):
            pass
        time.sleep(.01)
    raise AssertionError('completed primary reviewer receipt did not settle')


def wire_data(body):
    from scripts.proxy_native_fixture import review_records
    return review_records(body)


def contract_state(directory):
    return json.loads(next(Path(directory).glob('*.json')).read_text())


class NativeProviderFixtureTests(unittest.TestCase):
    def test_exact_checkpoint_settlement_ignores_malformed_decoy_and_unrelated_pending(self):
        from scripts.proxy_native_settle import wait_settled
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'malformed.json').write_text('{')
            (root/'unrelated-held.json').write_text(json.dumps({'completed_requests':99,
                'background_review_pending':True}))
            target = root/'exact-owned-lineage.json'
            target.write_text(json.dumps({'completed_requests':3,'background_review_pending':False}))
            # Target is an exact file, never first-file/global scan semantics.
            try:
                wait_settled(target, 3, timeout=.05)
            except TimeoutError:
                self.fail('exact settled checkpoint must not be blocked by unrelated files')
            target.write_text(json.dumps({'completed_requests':3,'background_review_pending':True}))
            (root/'decoy.json').write_text(json.dumps({'completed_requests':99,
                'background_review_pending':False}))
            with self.assertRaises(TimeoutError): wait_settled(target, 3, timeout=.03)
            target.write_text('{')
            with self.assertRaises(TimeoutError): wait_settled(target, 3, timeout=.03)
            from scripts.proxy_native_settle import wait_lineage_settled
            import hashlib
            for identity in [('tenant','family','parent'),('tenant','family','child')]:
                key = hashlib.sha256(json.dumps(identity, separators=(',', ':')).encode()).hexdigest()
                value = {'completed_requests':4,'background_review_pending':identity[-1]=='parent'}
                (root/(key+'.json')).write_text(json.dumps(value))
            self.assertEqual(wait_lineage_settled(root,'tenant','family','child',4,.05),
                {'completed_requests':4,'background_review_pending':False})
            with self.assertRaises(TimeoutError):
                wait_lineage_settled(root,'tenant','family','parent',4,.03)
            with self.assertRaises(TimeoutError):
                wait_lineage_settled(root,'other','family','child',4,.03)

    def test_native_tool_settlement_fails_closed_for_unusable_or_pending_checkpoint(self):
        from scripts.proxy_native_settle import wait_settled
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / 'state.json'
            for text in (None, '{', '{}', json.dumps({'completed_requests': 3,
                                                      'background_review_pending': True})):
                with self.subTest(checkpoint=text):
                    if text is not None:
                        state.write_text(text)
                    with self.assertRaises(TimeoutError):
                        wait_settled(directory, 3, timeout=.03)
            state.write_text(json.dumps({'completed_requests': 3, 'background_review_pending': False}))
            (Path(directory) / 'another.json').write_text(state.read_text())
            with self.assertRaises(TimeoutError):
                wait_settled(directory, 3, timeout=.03)

    def test_compact_native_tool_waits_for_completed_primary_and_reviewer_settlement(self):
        from scripts.proxy_native_fixture import Fixture
        with tempfile.TemporaryDirectory() as directory, Fixture('pi', 'compact') as fixture:
            fixture.state_directory = Path(directory)
            state_file = Path(directory) / 'synthetic.json'
            state_file.write_text(json.dumps({'completed_requests': 3, 'background_review_pending': True}))
            fixture.calls.extend([{'prompt_cache_key': 'fixture'}] * 2)
            request = urllib.request.Request(fixture.url + '/v1/responses',
                data=json.dumps({'prompt_cache_key': 'fixture', 'input': [
                    {'type': 'function_call_output', 'call_id': 'call_fixture_1_a', 'output': 'synthetic'}]}).encode(),
                headers={'content-type': 'application/json'})
            with urllib.request.urlopen(request, timeout=5) as response:
                events = [json.loads(line[6:]) for line in response.read().decode().splitlines()
                          if line.startswith('data: ')]
            completed = next(event['response'] for event in events if event['type'] == 'response.completed')
            command = json.loads(completed['output'][0]['arguments'])['command']
            process = subprocess.Popen(command, shell=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       text=True, start_new_session=True)
            try:
                with self.assertRaises(subprocess.TimeoutExpired,
                        msg='native next turn must wait for reviewer settlement after primary consumption'):
                    process.communicate(timeout=.2)
                state_file.write_text(json.dumps({'completed_requests': 2, 'background_review_pending': False}))
                with self.assertRaises(subprocess.TimeoutExpired,
                        msg='an older settled checkpoint cannot release this native turn'):
                    process.communicate(timeout=.2)
                state_file.write_text(json.dumps({'completed_requests': 3, 'background_review_pending': False}))
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 0, stderr)
                self.assertEqual(stdout, 'SMALL_COHORT_3')
            finally:
                if process.poll() is None:
                    import signal
                    os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=5)
            self.assertEqual(fixture.errors, [])

    def test_observer_preserves_delayed_response_after_client_write_eof(self):
        from scripts.proxy_native_fixture import ClientObserver
        import base64
        import socket
        import time
        request = b'POST /v1/responses HTTP/1.1\r\nHost: fixture\r\nContent-Length: 2\r\n\r\n{}'
        response = b'HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK'
        seen, eof, failures = [], [], []
        with socket.socket() as server:
            server.bind(('127.0.0.1', 0)); server.listen(); server.settimeout(3)
            def target():
                try:
                    for _ in range(2):
                        conn, _ = server.accept()
                        with conn:
                            conn.settimeout(3)
                            raw = b''
                            while len(raw) < len(request):
                                part = conn.recv(4096)
                                if not part: break
                                raw += part
                            seen.append(raw)
                            time.sleep(.15)
                            conn.sendall(response)
                            # Both directions must propagate EOF, not just drain a response.
                            eof.append(conn.recv(1))
                except OSError as error:
                    failures.append(str(error))
            thread = threading.Thread(target=target, daemon=True); thread.start()
            def exchange(port):
                with socket.create_connection(('127.0.0.1', port), timeout=3) as client:
                    client.sendall(request); client.shutdown(socket.SHUT_WR)
                    raw = b''
                    while True:
                        part = client.recv(4096)
                        if not part: return raw
                        raw += part
            try:
                direct = exchange(server.getsockname()[1])
                with tempfile.TemporaryDirectory() as directory, ClientObserver(
                        server.getsockname()[1], Path(directory)/'wire.jsonl') as observer:
                    relayed = exchange(observer.port)
                thread.join(3)
                self.assertFalse(thread.is_alive())
                self.assertEqual(failures, [])
                self.assertEqual(seen, [request, request])
                self.assertEqual(eof, [b'', b''])
                self.assertEqual(direct, response)
                self.assertEqual(relayed, direct)
                self.assertEqual(observer.errors, [])
                self.assertEqual(observer.bodies(), [{}])
                self.assertEqual(base64.b64decode(observer.records[0]['wire_base64']), request)
            finally:
                thread.join(3)

    def test_observer_shutdown_interrupts_stalled_response_reader(self):
        from scripts.proxy_native_fixture import ClientObserver
        import socket
        accepted, stalled = threading.Event(), threading.Event()
        sockets, failures = [], []
        with tempfile.TemporaryDirectory() as directory, socket.socket() as server:
            server.bind(('127.0.0.1', 0)); server.listen(); server.settimeout(3)
            def target():
                try:
                    conn, _ = server.accept()
                    with conn:
                        conn.settimeout(.2)
                        while b'\r\n\r\n' not in conn.recv(4096): pass
                        # Bounded real traffic, more than the loopback buffers can retain.
                        for _ in range(128):
                            conn.sendall(b'x' * 65536)
                except socket.timeout:
                    stalled.set()
                except OSError as error:
                    failures.append(str(error))
            thread = threading.Thread(target=target, daemon=True); thread.start()
            observer = ClientObserver(server.getsockname()[1], Path(directory)/'wire.jsonl')
            get_request = observer.server.get_request
            def accept_small_buffer():
                conn, address = get_request()
                conn.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4096)
                sockets.append(conn); accepted.set()
                return conn, address
            observer.server.get_request = accept_small_buffer
            observer.__enter__()
            closer = threading.Thread(target=observer.__exit__, args=(None, None, None), daemon=True)
            client = socket.socket()
            try:
                client.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
                client.settimeout(3); client.connect(('127.0.0.1', observer.port))
                client.sendall(b'GET / HTTP/1.1\r\nHost: fixture\r\n\r\n')
                self.assertTrue(accepted.wait(3))
                self.assertTrue(stalled.wait(3), 'upstream must really hit response backpressure')
                self.assertEqual(select.select([], sockets, [], .1)[1], [],
                                 'accepted socket must really be blocked by the non-reading client')
                closer.start(); closer.join(3)
                self.assertFalse(closer.is_alive(), 'observer shutdown blocked on accepted-socket send')
                self.assertEqual(observer.errors, [])
            finally:
                client.close()
                if closer.ident is None: closer.start()
                closer.join(3); thread.join(3)
            self.assertFalse(closer.is_alive())
            self.assertFalse(thread.is_alive())
            self.assertEqual(failures, [])

    def test_observer_rejects_incomplete_request_at_client_eof(self):
        from scripts.proxy_native_fixture import ClientObserver
        import socket
        requests = (
            b'POST /v1/responses HTTP/1.1\r\nHost: fixture',
            b'POST /v1/responses HTTP/1.1\r\nContent-Length: 2\r\n\r\n{',
            b'POST /v1/responses HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n',
        )
        for request in requests:
            with self.subTest(request=request), tempfile.TemporaryDirectory() as directory, socket.socket() as server:
                server.bind(('127.0.0.1', 0)); server.listen(); server.settimeout(3)
                def drain():
                    conn, _ = server.accept()
                    with conn:
                        conn.settimeout(3)
                        while conn.recv(4096): pass
                thread = threading.Thread(target=drain, daemon=True); thread.start()
                with ClientObserver(server.getsockname()[1], Path(directory)/'wire.jsonl') as observer:
                    with socket.create_connection(('127.0.0.1', observer.port), timeout=3) as client:
                        client.sendall(request); client.shutdown(socket.SHUT_WR)
                        self.assertEqual(client.recv(4096), b'')
                thread.join(3)
                self.assertFalse(thread.is_alive())
                self.assertEqual(observer.errors, ['incomplete observed HTTP request'])
                self.assertEqual(observer.records, [])
                with self.assertRaises(AssertionError): observer.bodies()

    def test_observer_upstream_write_eof_still_drains_client_request(self):
        from scripts.proxy_native_fixture import ClientObserver
        import socket
        request = b'POST /v1/responses HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}'
        seen, failures = [], []
        with tempfile.TemporaryDirectory() as directory, socket.socket() as server:
            server.bind(('127.0.0.1', 0)); server.listen(); server.settimeout(3)
            def target():
                try:
                    conn, _ = server.accept()
                    with conn:
                        conn.settimeout(3)
                        raw = conn.recv(4096)
                        conn.shutdown(socket.SHUT_WR)
                        while True:
                            part = conn.recv(4096)
                            if not part: break
                            raw += part
                        seen.append(raw)
                except OSError as error:
                    failures.append(str(error))
            thread = threading.Thread(target=target, daemon=True); thread.start()
            with ClientObserver(server.getsockname()[1], Path(directory)/'wire.jsonl') as observer:
                with socket.create_connection(('127.0.0.1', observer.port), timeout=3) as client:
                    client.sendall(request[:-1])
                    self.assertEqual(client.recv(4096), b'')
                    self.assertEqual(observer.errors, [],
                                     'upstream EOF must not reject a still-open client request')
                    client.sendall(request[-1:]); client.shutdown(socket.SHUT_WR)
                thread.join(3)
            self.assertFalse(thread.is_alive())
            self.assertEqual(failures, [])
            self.assertEqual(seen, [request])
            self.assertEqual(observer.errors, [])
            self.assertEqual(observer.bodies(), [{}])

    def test_compact_grading_uses_complete_executed_wire_not_bounded_raw_logs(self):
        from scripts.proxy_native_fixture import Fixture, ClientObserver, compact_evidence
        import copy
        with tempfile.TemporaryDirectory() as directory, Fixture('pi', 'compact') as fixture:
            root = Path(directory)
            trial = root/'pi-compact-trial'; trial.mkdir()
            workspace = root/'pi-compact-workspace'; workspace.mkdir()
            with contract_proxy(self, fixture, str(trial/'state'), mode='compact',
                                classifier_path='/classifier') as url:
                with ClientObserver(int(url.rsplit(':',1)[1]), root/'wire.jsonl') as observer:
                    body = {'model':'gpt-6-luna', 'prompt_cache_key':'synthetic-native',
                            'instructions':'fixture context ' * 5000,
                            'tools':[{'name':'bash'}], 'input':[
                            {'role':'developer','content':'Run the fixture tools.'},
                            {'role':'user','content':'old fixture direction'},
                            {'role':'user','content':'current fixture task'}]}
                    for _ in range(5):
                        req = urllib.request.Request(observer.url+'/v1/responses',
                            data=json.dumps(body).encode(), headers={'content-type':'application/json',
                            'x-carry-session':'contract'})
                        # This large debug-build planner/journal scenario uses
                        # the same finite deadline as contract_send.
                        with urllib.request.urlopen(req, timeout=30) as response:
                            events=[json.loads(line[6:]) for line in response.read().decode().splitlines()
                                    if line.startswith('data: ')]
                        contract_settle(trial/'state')
                        output=events[-1]['response']['output']
                        body['input'].extend(output)
                        for item in output:
                            if item['type']=='function_call':
                                run=subprocess.run(json.loads(item['arguments'])['command'],
                                    shell=True,cwd=workspace,capture_output=True,text=True,check=True)
                                body['input'].append({'type':'function_call_output',
                                    'call_id':item['call_id'],'output':run.stdout})
                    received=observer.bodies()
            self.assertFalse(fixture.errors)
            state_file=next((trial/'state').glob('*.json'))
            raw=[json.loads(line) for line in state_file.with_suffix('.jsonl').read_text().splitlines()]
            self.assertLess(sum(e['event']=='primary_received' for e in raw),5,
                            'control must really exceed bounded production raw logging')
            # Grade even with no operational raw trace at all.
            state_file.with_suffix('.jsonl').unlink()
            self.assertTrue(compact_evidence(trial,fixture,received=received)['compact_verified'])
            original=copy.deepcopy(fixture.calls)
            for mutation in ('missing-in','extra-in','missing-out','extra-out','corrupt-output','cohort'):
                before=copy.deepcopy(received); fixture.calls=copy.deepcopy(original)
                if mutation=='missing-in': before.pop()
                if mutation=='extra-in': before.append(before[-1])
                if mutation=='missing-out': fixture.calls.pop()
                if mutation=='extra-out': fixture.calls.append(fixture.calls[-1])
                if mutation=='corrupt-output':
                    next(i for i in fixture.calls[-1]['input'] if i.get('type')=='function_call_output')['output']='CORRUPT'
                if mutation=='cohort':
                    fixture.calls[3]['input'] += [i for i in before[3]['input']
                        if i.get('call_id')=='call_fixture_1_a']
                with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                    compact_evidence(trial,fixture,received=before)
            fixture.calls=original

    def test_observer_captures_post_gateway_wire_and_operator_headers(self):
        from scripts.proxy_native_fixture import ClientObserver
        from scripts.proxy_gateway_fixture_support import Gateway
        from scripts.test_proxy_gateway import Provider
        import base64
        with tempfile.TemporaryDirectory() as directory, Provider(status=409) as provider:
            with ClientObserver(provider.server.server_port, Path(directory)/'wire.jsonl') as observer:
                gateway=Gateway(observer.port,provider.server.server_port,'strict')
                try:
                    body=b' { "model":"gpt-6-luna", "input":[] } '
                    status, raw=gateway.request(iter((body[:5],body[5:])), {'x-carry-session':'spoof',
                        'x-carry-history-policy':'reset-on-divergence','x-carry-secret':'spoof'})
                    self.assertEqual(status,409)
                    self.assertEqual(provider.calls[0]['body'],body)
                    self.assertEqual(observer.bodies(),[json.loads(body)])
                    wire=base64.b64decode(observer.records[0]['wire_base64'])

                    self.assertIn(b'x-carry-session: fixture-session',wire.lower())
                    self.assertIn(b'x-carry-history-policy: strict',wire.lower())
                    self.assertNotIn(b'spoof',wire)
                finally:
                    gateway.close()

    def test_observer_decodes_chunked_requests_without_changing_the_wire(self):
        from scripts.proxy_native_fixture import ClientObserver
        from scripts.test_proxy_gateway import Provider
        import base64
        with tempfile.TemporaryDirectory() as directory, Provider() as provider:
            with ClientObserver(provider.server.server_port,Path(directory)/'wire.jsonl') as observer:
                body=b'{"input":[], "escaped":"\\u00e9"}'
                request=urllib.request.Request(observer.url+'/v1/responses',
                    data=iter((body[:3],body[3:])), headers={'content-type':'application/json'})
                with urllib.request.urlopen(request,timeout=3) as response: response.read()
                self.assertEqual(provider.calls[0]['body'],body)
                self.assertEqual(observer.bodies(),[json.loads(body)])
                wire=base64.b64decode(observer.records[0]['wire_base64'])
                self.assertIn(b'transfer-encoding: chunked',wire.lower())

    def test_observer_preserves_exact_http_and_streams_before_response_finishes(self):
        from scripts.proxy_native_fixture import ClientObserver
        import socket
        release = threading.Event()
        seen = []
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_POST(self):
                seen.append((self.path, list(self.headers.raw_items()),
                             self.rfile.read(int(self.headers['content-length']))))
                self.send_response(409)
                self.send_header('content-type', 'text/event-stream')
                self.send_header('content-length', '12')
                self.end_headers()
                self.wfile.write(b'data: first\n'); self.wfile.flush()
                release.wait(3)
                self.wfile.write(b'\n')
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory, ClientObserver(
                    server.server_port, Path(directory) / 'wire.jsonl') as observer:
                body = b'{ "input": [], "text": "\\u00e9" }'
                with socket.create_connection(('127.0.0.1', observer.port), timeout=3) as sock:
                    sock.sendall(b'POST /v1/responses?native=1 HTTP/1.1\r\nHost: original\r\n'
                        b'X-Native: one\r\nX-Native: two\r\nContent-Length: '+str(len(body)).encode()+
                        b'\r\nConnection: close\r\n\r\n'+body)
                    raw = b''
                    while b'data: first\n' not in raw: raw += sock.recv(4096)
                    self.assertIn(b'409 Conflict', raw)
                    self.assertFalse(release.is_set(), 'SSE must not be buffered until EOF')
                    release.set()
                    while sock.recv(4096): pass
                self.assertEqual(seen[0][0], '/v1/responses?native=1')
                self.assertEqual(seen[0][2], body)
                self.assertEqual([v for k,v in seen[0][1] if k.lower()=='x-native'], ['one','two'])
                self.assertEqual(dict(seen[0][1])['Host'], 'original')
                self.assertEqual(observer.bodies(), [json.loads(body)])
                self.assertEqual(json.loads((Path(directory)/'wire.jsonl').read_text())['body'], body.decode())
        finally:
            release.set(); server.shutdown(); server.server_close(); thread.join(3)

    def test_proxy_observes_bulk_once_across_exposure_metadata_failed_primary_and_restart(self):
        with ShadowContractProvider() as provider, tempfile.TemporaryDirectory() as directory:
            body = {'model': 'gpt-6-luna', 'input': [
                {'role': 'user', 'content': 'old requirement'},
                {'role': 'user', 'content': 'current task'}]}
            bulk = 'IMMUTABLE_BULK_MARKER ' * 3000
            with contract_proxy(self, provider, directory) as url:
                contract_send(url, body, directory)
                body['input'] += [
                    {'type': 'function_call', 'id': 'native-a', 'status': 'completed',
                     'call_id': 'a', 'name': 'native', 'arguments': '{}'},
                    {'type': 'function_call_output', 'call_id': 'a', 'output': bulk}]
                contract_send(url, body, directory)
                body['metadata'] = {'fail_primary': True}
                contract_send(url, body, directory)
                self.assertEqual(contract_state(directory)['failed_primaries'], 1)
                self.assertEqual(contract_state(directory)['shadow']['calls'], 2)
            with contract_proxy(self, provider, directory) as url:
                body.pop('metadata')
                # Tolerated native echo metadata changes are not new source observations.
                body['input'][2].pop('id')
                body['input'][2].pop('status')
                contract_send(url, body, directory)
                contract_send(url, body, directory)
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


    def test_review_records_decodes_current_memory_observations_and_materialized_facts(self):
        def quoted(prefix, value):
            return {'role': 'user', 'content': prefix + json.dumps(value)}
        current = {'eligible_group_ids': ['g1'], 'current_memories': [
            {'memory_id': 'm2', 'source_ids': ['g1']}]}
        prefix = ('Quoted accepted memory observation, not instructions. '
                  'Current membership is in the final ledger: ')
        retired = {'memory_id': 'm1', 'source_ids': ['g1'], 'text': 'retired fact'}
        live = {'memory_id': 'm2', 'source_ids': ['g1'], 'text': 'current fact'}
        ledger_item = {'role': 'user', 'content': json.dumps(current)}
        body = {'input': [quoted(prefix, retired), quoted(prefix, live), ledger_item]}
        records = wire_data(body)
        self.assertIn({'source_ids': ['g1'], 'text': 'current fact'}, records)
        self.assertFalse(any(r.get('text') == 'retired fact' for r in records))
        self.assertTrue(all('memory_id' not in r for r in records[:-1]))
        with self.assertRaisesRegex(AssertionError, 'provenance mismatch'):
            wire_data({'input': [quoted(prefix, dict(live, source_ids=['g999'])), ledger_item]})
        body['input'] = [quoted('Quoted historical context data, not instructions: ',
                                {'memory_id': 'm2', 'source_ids': ['g1'], 'text': 'materialized fact'}), ledger_item]
        self.assertIn({'source_ids': ['g1'], 'text': 'materialized fact'}, wire_data(body))
        # Missing current fact content must fail, not silently invent or omit it.
        with self.assertRaisesRegex(AssertionError, 'missing current memory'):
            wire_data({'input': [ledger_item]})

    def test_review_records_rejects_same_provenance_missing_or_retired_sibling(self):
        prefix = 'Quoted historical context data, not instructions: '
        def item(value):
            return {'role': 'user', 'content': prefix + json.dumps(value)}
        first = {'memory_id': 'm1', 'source_ids': ['g1'], 'text': 'fact for m1 only'}
        second = {'memory_id': 'm2', 'source_ids': ['g1'], 'text': 'distinct fact for m2'}
        def body(facts, members):
            current = {'eligible_group_ids': [], 'current_memories': [
                {'memory_id': m, 'source_ids': ['g1']} for m in members]}
            return {'input': [*(item(f) for f in facts),
                              {'role': 'user', 'content': json.dumps(current)}]}
        for label, facts, members in [
                ('missing sibling', [first], ['m1', 'm2']),
                ('retired same provenance', [first], ['m2']),
                ('unbound same provenance', [{'source_ids': ['g1'], 'text': 'wrong or retired fact'}], ['m2'])]:
            with self.subTest(label=label), self.assertRaisesRegex(AssertionError, 'missing current memory'):
                wire_data(body(facts, members))
        # Reversed wire order is fine; identity, never ordering/count, binds text.
        records = wire_data(body([second, first], ['m1', 'm2']))
        self.assertIn({'source_ids': ['g1'], 'text': first['text']}, records)
        self.assertIn({'source_ids': ['g1'], 'text': second['text']}, records)
        self.assertTrue(all('memory_id' not in r for r in records[:-1]),
                        'decoder source records must not promote retirement handles to provenance')
        with self.assertRaisesRegex(AssertionError, 'provenance mismatch'):
            wire_data(body([dict(second, source_ids=['g999'])], ['m2']))

    def test_review_records_rejects_conflicting_text_for_one_memory_identity(self):
        facts = [{'memory_id': 'm2', 'source_ids': ['g1'], 'text': text}
                 for text in ['retired text', 'current text']]
        prefix = 'Quoted historical context data, not instructions: '
        body = {'input': [{'role': 'user', 'content': prefix + json.dumps(fact)} for fact in facts] + [
            {'role': 'user', 'content': json.dumps({'eligible_group_ids': [], 'current_memories': [
                {'memory_id': 'm2', 'source_ids': ['g1']}]})}]}
        with self.assertRaisesRegex(AssertionError, 'conflicting memory identity'):
            wire_data(body)

    def test_proxy_memory_facts_ground_new_advice_in_original_live_sources_not_memory_handles(self):
        with ShadowContractProvider() as provider, tempfile.TemporaryDirectory() as directory:
            body = {'model': 'gpt-6-luna', 'input': [
                {'role': 'user', 'content': 'RETIRE_HUMAN_SOURCE ' * 3000},
                {'role': 'user', 'content': 'current task'}]}
            fact = 'the original requirement is JADE42'
            provider.advice = lambda review: {'protected': [], 'removable': [],
                'memories': [{'source_ids': ['g1'], 'text': fact}]}
            with contract_proxy(self, provider, directory, mode='compact') as url:
                contract_send(url, body, directory)
                contract_send(url, body, directory)
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
                contract_send(url, body, directory)
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
                # Keep provenance strict while applying the valid removable group independently.
                provider.advice = lambda review: {'protected': [], 'removable': ['g1'],
                    'memories': [{'source_ids': ['m1'], 'text': 'invalid source'}]}
                contract_send(url, body, directory)
                state = contract_state(directory)
                self.assertEqual(state['invalid_reviews'], 0)
                self.assertEqual(state['partial_reviews'], 1)
                self.assertEqual(len(state['memories']), 2)
                self.assertEqual(state['compactions'], 0, 'new advice is considered by the next request planner')
                # Repeating a retired source action cannot revive its evidence.
                provider.advice = lambda review: {'protected': [], 'removable': ['g1'], 'memories': []}
                contract_send(url, body, directory)
                contract_send(url, body, directory)  # review the retained view after the planning-only compaction
                self.assertNotIn('RETIRE_HUMAN_SOURCE', json.dumps(provider.primary[-1]))
                state = contract_state(directory)
                self.assertTrue(state['history'][0]['removed'])
                self.assertEqual(state['compactions'], 1)
                # Provenance remains visible, but does not re-admit retired IDs for advice.
                provider.advice = lambda review: {'protected': [], 'removable': [],
                    'memories': [{'source_ids': ['g1'], 'text': 'retired source is not newly eligible'}]}
                body['input'].append({'role': 'user', 'content': 'next task'})
                contract_send(url, body, directory)
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
                contract_send(url, body, directory)
                body['input'] += [
                    {'type': 'function_call', 'call_id': 'a', 'name': 'native', 'arguments': 'CALL_A'},
                    {'type': 'function_call', 'call_id': 'b', 'name': 'native', 'arguments': 'CALL_B'},
                    {'type': 'function_call_output', 'call_id': 'a', 'output': 'RESULT_A ' * 500}]
                contract_send(url, body, directory)
                provider.advice = lambda review: {'protected': [], 'removable': ['g1'], 'memories': []}
                body['input'].append({'type': 'function_call_output', 'call_id': 'b',
                                      'output': 'RESULT_B ' * 500})
                contract_send(url, body, directory)
                provider.advice = lambda review: {'protected': [], 'removable': [], 'memories': []}
                contract_send(url, body, directory)
            self.assertEqual(len(provider.shadow), 3)
            self.assertEqual([json.dumps(r).count('RESULT_A') for r in provider.shadow], [0, 500, 500],
                             'incomplete tool cohorts must be withheld until every result arrives')
            self.assertEqual([json.dumps(r).count('RESULT_B') for r in provider.shadow], [0, 500, 500])
            for index, review in enumerate(provider.shadow):
                records = wire_data(review)
                blocks = [r for r in records if 'items' in r and r['group_id'] == 'g3']
                self.assertEqual([r['member_ids'] for r in blocks],
                                 [] if index == 0 else [[3, 4, 5, 6]])
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
                contract_send(url, body, directory)
                body['input'] += [
                    {'type': 'function_call', 'call_id': 'a', 'name': 'native', 'arguments': '{}'},
                    {'type': 'function_call_output', 'call_id': 'a', 'output': 'LEGACY_BULK ' * 500}]
                contract_send(url, body, directory)
                contract_send(url, body, directory)
            self.assertEqual(json.dumps(provider.shadow[-1]).count('LEGACY_BULK'), 1000,
                             'old binary must actually produce the historical duplicated checkpoint')
            with contract_proxy(self, provider, directory) as url:
                contract_send(url, body, directory)
                contract_send(url, body, directory)
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
            # Hold the actual reviewer HTTP result: protocol assertions below
            # must observe completed/persisted work, not a pending background job.
            review_started, review_release = threading.Event(), threading.Event()
            handler = fixture.server.RequestHandlerClass
            original_post = getattr(handler, 'do_POST')
            def held_post(request):
                if request.path == '/classifier':
                    review_started.set()
                    if not review_release.wait(5):
                        raise TimeoutError('owned reviewer was not released')
                return original_post(request)
            setattr(handler, 'do_POST', held_post)
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('OPENAI_', 'CARRY_PROXY_'))}
            home = Path(directory) / 'home'; home.mkdir()
            env.update(HOME=str(home), CARRY_HOME=str(home / '.carry'),
                       XDG_CONFIG_HOME=str(home / '.config'),
                       CARRY_PROXY_UPSTREAM_KEY='synthetic-fixture-key',
                       CARRY_PROXY_CLASSIFIER_KEY='synthetic-fixture-key')
            # This protocol test needs a review on its tiny second request.
            env.update(CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS='0',
                       CARRY_PROXY_REVIEW_MIN_NEW_TOKENS='0',
                       CARRY_PROXY_REVIEW_BACKOFF_FACTOR='1',
                       CARRY_PROXY_REVIEW_EVERY_REQUESTS='1')
            command = [str(binary), 'proxy', '--verbose', '--listen', '127.0.0.1:0',
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
                    from concurrent.futures import ThreadPoolExecutor
                    def send():
                        try:
                            with urllib.request.urlopen(req, timeout=5) as response:
                                return response.read().decode()
                        except urllib.error.HTTPError as error:
                            detail = error.read().decode()
                            error.close()
                            self.fail(f'primary request {turn} failed: {detail}; {fixture.errors}')
                    with ThreadPoolExecutor(max_workers=1) as pool:
                        pending = pool.submit(send)
                        try:
                            if turn == 1:
                                self.assertTrue(review_started.wait(3), 'real reviewer must start')
                                # The legacy flag must not reintroduce a reviewer
                                # dependency: consume this primary while held.
                                pending.result(timeout=2)
                                self.assertFalse(review_release.is_set())
                                self.assertEqual(len(fixture.calls), 2,
                                                 'second primary must precede review completion')
                                self.assertEqual(len(fixture.reviews), 0)
                        finally:
                            if turn == 1:
                                review_release.set()
                        raw = pending.result(timeout=5)
                    self.assertEqual(fixture.calls[-1], body, 'review must not alter primary/tool data')
                    result = [json.loads(line[6:]) for line in raw.splitlines()
                              if line.startswith('data: ')][-1]['response']
                    if turn == 0:
                        body['input'] += result['output'] + [
                            {'type': 'function_call_output', 'call_id': result['output'][0]['call_id'],
                             'output': 'FIXTURE_TOOL_OK'}]
                state_file = next(Path(directory).glob('*.json'))
                state = contract_settle(directory)
                if os.environ.get('CARRY_TEST_EVIDENCE'):
                    events = [json.loads(line) for line in state_file.with_suffix('.jsonl').read_text().splitlines()]
                    Path(os.environ['CARRY_TEST_EVIDENCE']).write_text(json.dumps({
                        'fixture_only': True, 'primary_calls': len(fixture.calls),
                        'classifier_calls': len(fixture.reviews), 'provider_errors': fixture.errors,
                        'invalid_reviews': state['invalid_reviews'],
                        'shadow_failures': [e['data'] for e in events if e['event'] == 'shadow_failed']
                    }, indent=2) + '\n')
                self.assertEqual(len(fixture.reviews), 1, 'must execute a real classifier HTTP request')
                self.assertEqual(state['shadow']['calls'], 1, 'completed review receipt must be persisted')
                self.assertEqual(fixture.reviews[0]['text']['format']['type'], 'json_schema')
                self.assertTrue(fixture.reviews[0]['text']['format']['strict'])
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
            fourth=call('/v1/responses',body)
            self.assertEqual(fourth['output'][0]['type'],'function_call')
            body['input']+=fourth['output']+[{'type':'function_call_output',
                'call_id':fourth['output'][0]['call_id'],'output':'small'}]
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
