"""Real native planner negative proof matrix; synthetic loopback, not residency.

All provider/wire/advice/money controls finish before independent economic
assertions. Unknown gateway routing cannot be certified by a model slug or
an aggregate receipt, even when it reports a complete write.
"""
import copy
import http.server
import json
import os
import pathlib
import subprocess
import threading
import unittest

from proxy_quantity_policy import Cells


def report_policy_rows(test, binary, directory, events):
    """Replay the native trace and a scenario-policy-only diagnostic control."""
    rows = []
    for label, declared_minimum in [('native', None), ('model-policy-control', 1024)]:
        session = directory / label / 'session'
        session.mkdir(parents=True)
        replay_events = copy.deepcopy(events)
        for event in replay_events:
            if event['event'] in ('run_started', 'session_resumed'):
                test.assertIsNone(event['data']['prompt_cache_capabilities'])
                test.assertIsNone(event['data']['implicit_cache_minimum_prefix_tokens'])
                if declared_minimum is not None:
                    event['data']['implicit_cache_minimum_prefix_tokens'] = declared_minimum
        # Remove only the diagnostic metadata override: all paid receipts,
        # native route capability and exact request/projection bytes are equal.
        restored = copy.deepcopy(replay_events)
        for event in restored:
            if event['event'] in ('run_started', 'session_resumed'):
                event['data']['implicit_cache_minimum_prefix_tokens'] = None
        test.assertEqual(restored, events)
        (session / 'trace.jsonl').write_text(''.join(json.dumps(event) + '\n' for event in replay_events))
        report = directory / (label + '.html')
        process = subprocess.run([binary, 'report', 'cost', '--sessions', str(session.parent),
                                  '--output', str(report)],
                                 env={'PATH': '', 'HOME': str(directory), 'TMPDIR': str(directory)},
                                 capture_output=True, timeout=15)
        (directory / (label + '.stdout')).write_bytes(process.stdout)
        (directory / (label + '.stderr')).write_bytes(process.stderr)
        test.assertEqual(process.returncode, 0, process.stderr)
        cells = Cells()
        cells.feed(report.read_text())
        test.assertEqual(len(cells.cells), 10, cells.cells)
        test.assertEqual(cells.cells[2], '3/3')
        test.assertEqual(cells.cells[3], '$0.0301')
        test.assertIn('0 unpriced', cells.cells[9])
        rows.append(cells.cells)
    (directory / 'REPORT_POLICY.json').write_text(json.dumps(rows, indent=2) + '\n')
    return rows


class NativeTracker(unittest.TestCase):
    def test_unknown_route_and_receipts_cannot_invent_cache_veto(self):
        root = pathlib.Path(os.environ['CARRY_TEST_EVIDENCE_DIR'])
        root.mkdir(parents=True, exist_ok=True)
        binary = os.environ['CARRY_TEST_BINARY']
        seed = 'COLD_REMOVABLE_SEED ' * 400
        retained = 'retained_evidence ' * 400
        base = {'input_tokens': 5000, 'output_tokens': 4,
                'input_tokens_details': {'cached_tokens': 0, 'cache_write_tokens': 0}}
        partial_read = copy.deepcopy(base)
        partial_read['input_tokens_details']['cached_tokens'] = 1000
        partial_write = copy.deepcopy(base)
        partial_write['input_tokens_details']['cache_write_tokens'] = 1000
        whole_write = copy.deepcopy(base)
        whole_write['input_tokens_details']['cache_write_tokens'] = 5000
        cases = [('zero', base, 0, 0, .01004),
                 ('partial-read', partial_read, 1000, 0, .00824),
                 ('partial-write', partial_write, 0, 1000, .01054),
                 ('whole-write-unknown-route', whole_write, 0, 5000, .01254),
                 ('missing', None, 0, 0, None)]
        captures = []
        for streaming in (False, True):
            for name, usage, read, written, cost in cases:
                for protected in (False, True):
                    directory = root / f'{int(streaming)}-{name}-{int(protected)}'
                    directory.mkdir()
                    for child in ('home', 'workspace'):
                        (directory / child).mkdir()
                    requests, responses, errors = [], [], []

                    class Handler(http.server.BaseHTTPRequestHandler):
                        def log_message(self, *_):
                            pass

                        def do_POST(self):
                            try:
                                requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                                n = len(requests)
                                args = {'context': {'protected': ([1, 2, 3] if protected else [2, 3]) if n == 2 else [],
                                                    'removable': [1] if n == 2 and not protected else [],
                                                    'remember': []}}
                                if n == 1:
                                    args['command'] = "printf '%s' '" + retained + "'"
                                elif n == 2:
                                    args['command'] = 'true'
                                else:
                                    args['answer'] = 'done'
                                raw = {'id': f'tracker-{n}', 'status': 'completed', 'output': [
                                    {'type': 'function_call', 'call_id': f'call-{n}',
                                     'name': 'shell' if n < 3 else 'finish', 'arguments': json.dumps(args)}]}
                                if usage is not None:
                                    raw['usage'] = usage
                                responses.append(raw)
                                wire = ((f'data: {json.dumps({"type": "response.completed", "response": raw})}\n\n').encode()
                                        if streaming else json.dumps(raw).encode())
                                self.send_response(200)
                                self.send_header('Content-Type', 'text/event-stream' if streaming else 'application/json')
                                self.send_header('Content-Length', str(len(wire)))
                                self.end_headers()
                                self.wfile.write(wire)
                            except Exception as error:
                                errors.append(repr(error))

                    server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
                    worker = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': .02}, daemon=True)
                    worker.start()
                    try:
                        env = {'PATH': '/usr/bin:/bin', 'HOME': str(directory / 'home'),
                               'CARRY_HOME': str(directory / 'home'), 'TMPDIR': str(directory),
                               'OPENAI_API_KEY': 'synthetic-only',
                               'OPENAI_BASE_URL': f'http://127.0.0.1:{server.server_port}/v1'}
                        process = subprocess.run([binary, '--cwd', str(directory / 'workspace'),
                                                  '--session-dir', str(directory / 'run'), '--model', 'gpt-6-sol',
                                                  '--compaction-policy', 'economic', '--compaction-payoff-requests', '1',
                                                  '--compaction-min-payback-percent', '0', '-p', seed],
                                                 env=env, capture_output=True, timeout=30)
                        (directory / 'stdout.txt').write_bytes(process.stdout)
                        (directory / 'stderr.txt').write_bytes(process.stderr)
                    finally:
                        server.shutdown()
                        server.server_close()
                        worker.join(timeout=2)
                    self.assertEqual(errors, [])
                    self.assertFalse(worker.is_alive())
                    self.assertEqual(process.returncode, 0, process.stderr)
                    result = json.loads((directory / 'run/result.json').read_text())
                    events = [json.loads(line) for line in (directory / 'run/trace.jsonl').read_text().splitlines()]
                    self.assertEqual(len(requests), 3)
                    self.assertEqual(len(responses), 3)
                    self.assertTrue(result['completed'])
                    self.assertEqual(result['usage']['input_tokens'], 15000 if usage else 0)
                    self.assertEqual(result['usage']['output_tokens'], 12 if usage else 0)
                    self.assertEqual(result['usage']['cached_input_tokens'], read * 3)
                    self.assertEqual(result['usage']['cache_write_input_tokens'], written * 3)
                    observed = [e['data'] for e in events if e['event'] == 'model_response']
                    self.assertEqual([item['raw'] for item in observed], responses)
                    logged = [copy.deepcopy(e['data']['request']) for e in events if e['event'] == 'model_request']
                    for body in logged:
                        body['stream'] = True
                    self.assertEqual(logged, requests)
                    for response in observed:
                        if cost is None:
                            self.assertIsNone(response['estimated_cost_usd'])
                        else:
                            self.assertAlmostEqual(response['estimated_cost_usd'], cost, places=12)
                    signals = [e['data']['signals'] for e in events if e['event'] == 'context_signals']
                    self.assertEqual(signals[1]['drop'], [] if protected else [1])
                    self.assertEqual(signals[1]['keep'], [1, 2, 3] if protected else [2, 3])
                    self.assertEqual(signals[1]['ignored'], [])
                    self.assertIn(seed, json.dumps(requests[1]))
                    self.assertIn(retained, json.dumps(requests[2]))
                    # Whole native tool call/result cohorts and the protected
                    # source survive exactly; removal targets only user seed1.
                    for call in responses[:2]:
                        self.assertIn(call['output'][0], requests[2]['input'])
                    self.assertEqual([i['call_id'] for i in requests[2]['input']
                                      if i.get('type') == 'function_call_output'], ['call-1', 'call-2'])
                    capture = {'name': name, 'streaming': streaming, 'protected': protected,
                               'requests': requests, 'responses': responses, 'result': result, 'events': events,
                               'retains_seed': seed in json.dumps(requests[2])}
                    (directory / 'RESULT.json').write_text(json.dumps(capture, indent=2) + '\n')
                    captures.append(capture)
        (root / 'RESULT.json').write_text(json.dumps(captures, indent=2) + '\n')
        self.assertEqual(len(captures), len(cases) * 4)
        for case in captures:
            with self.subTest(name=case['name'], streaming=case['streaming'], protected=case['protected']):
                self.assertEqual(case['retains_seed'], case['protected'],
                                 'NATIVE_CACHE_PROOF: unknown-route receipts/local estimates cannot invent cache-veto authority')
                self.assertEqual(case['result']['compactions'], 0 if case['protected'] else 1)
        report_captures = []
        for case in captures:
            if case['name'] != 'zero' or case['protected']:
                continue
            directory = root / ('report-policy-' + str(int(case['streaming'])))
            directory.mkdir()
            rows = report_policy_rows(self, binary, directory, case['events'])
            # The trace has a real native Carry rewrite and no native cache
            # entitlement. Its API-equivalent forecast is a separate contract.
            self.assertEqual(rows[1][4:7], ['$0.0301', '$0.0218', '$-0.0083'])
            report_captures.append((case['streaming'], rows))
        self.assertEqual(len(report_captures), 2)
        for streaming, rows in report_captures:
            with self.subTest(streaming=streaming, gate='native-report-scenario'):
                self.assertEqual(rows[0][4:7], rows[1][4:7],
                    'NATIVE_REPORT_POLICY: absent native entitlement cannot erase an independent known API-equivalent scenario')


if __name__ == '__main__':
    unittest.main()
