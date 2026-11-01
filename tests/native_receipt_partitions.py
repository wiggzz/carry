"""Native JSON/SSE receipt partitions and real report replay, loopback only.

Raw provider quantities are metering authority; normalized display counters must
not convert missing/conflicting receipt fields into known zero-cost partitions.
This fixture does not assert provider residency, expiry or reusable cache proof.
"""
import copy
import http.server
import json
import os
import pathlib
import subprocess
import tempfile
import threading
import unittest

from proxy_quantity_policy import Cells


class NativeReceipts(unittest.TestCase):
    def test_native_and_report_use_validated_raw_partitions(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(os.environ.get('CARRY_TEST_EVIDENCE_DIR', temporary.name))
        root.mkdir(parents=True, exist_ok=True)
        binary = os.environ['CARRY_TEST_BINARY']
        base = {'input_tokens': 2000, 'output_tokens': 4, 'total_tokens': 2004,
                'input_tokens_details': {'cached_tokens': 700, 'cache_write_tokens': 300}}
        alternate = {'input_tokens': 2000, 'output_tokens': 4, 'total_tokens': 2004,
                     'cached_input_tokens': 700, 'cache_creation_input_tokens': 300}
        agreeing = copy.deepcopy(base)
        agreeing.update(cached_input_tokens=700, cache_creation_input_tokens=300)
        cases = [('nested', base, (700, 300), 0.00293),
                 ('alternate', alternate, (700, 300), 0.00293),
                 ('agreeing', agreeing, (700, 300), 0.00293)]
        for name, field, value, counts, cost in [
                ('absent-write', 'input_tokens_details', {'cached_tokens': 700}, (700, 0), 0.00278),
                ('explicit-zero', 'input_tokens_details', {'cached_tokens': 0, 'cache_write_tokens': 0}, (0, 0), 0.00404),
                ('missing-read', 'input_tokens_details', {'cache_write_tokens': 300}, (0, 300), None),
                ('conflicting-read', 'cached_input_tokens', 701, (0, 300), None),
                ('conflicting-write', 'cache_creation_input_tokens', 301, (700, 0), None),
                ('malformed-write', 'cache_creation_input_tokens', '300', (700, 0), None),
                ('malformed-read', 'cached_input_tokens', '700', (0, 300), None),
                ('overlap', 'input_tokens_details', {'cached_tokens': 1900, 'cache_write_tokens': 300}, (1900, 300), None),
                ('read-overflow', 'input_tokens_details', {'cached_tokens': 2001}, (0, 0), None),
                ('partition-overflow', 'input_tokens_details', {'cached_tokens': 700, 'cache_write_tokens': 18446744073709551615}, (700, 0), None),
                ('missing-output', 'output_tokens', None, (700, 300), None),
                ('malformed-input', 'input_tokens', '2000', (0, 0), None)]:
            usage = copy.deepcopy(base)
            if value is None:
                usage.pop(field)
            else:
                usage[field] = value
            cases.append((name, usage, counts, cost))
        cases.extend([
            ('missing-entire-usage', None, (0, 0), None),
            ('null-entire-usage', None, (0, 0), None),
            ('known-zero', {'input_tokens': 0, 'output_tokens': 0,
                            'input_tokens_details': {'cached_tokens': 0, 'cache_write_tokens': 0}},
             (0, 0), 0.0),
        ])
        captures = []
        for streaming in (False, True):
            for name, usage, counts, cost in cases:
                directory = root / f'{int(streaming)}-{name}'
                directory.mkdir()
                for child in ('home', 'workspace', 'sessions'):
                    (directory / child).mkdir()
                raw = {'id': f'native-{int(streaming)}-{name}', 'status': 'completed',
                       'output': [{'type': 'function_call', 'call_id': 'call-1', 'name': 'finish',
                                   'arguments': json.dumps({'answer': 'done', 'context': {
                                       'protected': [], 'removable': [], 'remember': []}})}],
                       'usage': usage}
                if name == 'missing-entire-usage':
                    raw.pop('usage')
                requests, errors = [], []

                class Handler(http.server.BaseHTTPRequestHandler):
                    def log_message(self, *_):
                        pass

                    def do_POST(self):
                        try:
                            requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                            body = ((f'data: {json.dumps({"type": "response.completed", "response": raw})}\n\n').encode()
                                    if streaming else json.dumps(raw).encode())
                            self.send_response(200)
                            self.send_header('Content-Type', 'text/event-stream' if streaming else 'application/json')
                            self.send_header('Content-Length', str(len(body)))
                            self.end_headers()
                            self.wfile.write(body)
                        except Exception as error:
                            errors.append(repr(error))

                server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
                worker = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.02}, daemon=True)
                worker.start()
                session = directory / 'sessions' / 'native'
                try:
                    env = {'PATH': '/usr/bin:/bin', 'HOME': str(directory / 'home'),
                           'CARRY_HOME': str(directory / 'home'), 'TMPDIR': str(directory),
                           'OPENAI_API_KEY': 'synthetic-only',
                           'OPENAI_BASE_URL': f'http://127.0.0.1:{server.server_port}/v1'}
                    process = subprocess.run([binary, '--cwd', str(directory / 'workspace'),
                                              '--session-dir', str(session), '--model', 'gpt-6-sol',
                                              '--compaction-policy', 'disabled', '-p', 'finish'],
                                             env=env, capture_output=True, timeout=15)
                    (directory / 'stdout.txt').write_bytes(process.stdout)
                    (directory / 'stderr.txt').write_bytes(process.stderr)
                    self.assertEqual(process.returncode, 0, process.stderr)
                finally:
                    server.shutdown()
                    server.server_close()
                    worker.join(timeout=2)
                self.assertEqual(errors, [])
                self.assertFalse(worker.is_alive())
                events = [json.loads(line) for line in (session / 'trace.jsonl').read_text().splitlines()]
                responses = [event['data'] for event in events if event['event'] == 'model_response']
                result = json.loads((session / 'result.json').read_text())
                self.assertEqual(len(requests), 1)
                self.assertEqual(len(responses), 1)
                self.assertEqual(responses[0]['raw'], raw)
                self.assertTrue(result['completed'])
                logged_requests = [copy.deepcopy(event['data']['request']) for event in events
                                   if event['event'] == 'model_request']
                self.assertEqual(len(logged_requests), 1)
                # Native transport adds its stream flag after request logging.
                logged_requests[0]['stream'] = True
                self.assertEqual(logged_requests, requests)
                self.assertFalse(any(event['event'] == 'shell_started' for event in events))
                self.assertEqual(result['compactions'], 0)
                expected_usage = usage or {}
                self.assertEqual(result['usage']['input_tokens'], expected_usage.get('input_tokens', 0) if isinstance(expected_usage.get('input_tokens', 0), int) else 0)
                self.assertEqual(result['usage']['output_tokens'], expected_usage.get('output_tokens', 0))
                # Grade all complete wires/raw receipts/call/finish controls
                # before independent parser/price assertions below.
                report = directory / 'cost.html'
                replay = subprocess.run([binary, 'report', 'cost', '--sessions', str(session.parent),
                                         '--output', str(report)],
                                        env={'PATH': '', 'HOME': str(directory / 'home'), 'TMPDIR': str(directory)},
                                        capture_output=True, timeout=15)
                (directory / 'report.stdout').write_bytes(replay.stdout)
                (directory / 'report.stderr').write_bytes(replay.stderr)
                self.assertEqual(replay.returncode, 0, replay.stderr)
                cells = Cells()
                cells.feed(report.read_text())
                self.assertEqual(len(cells.cells), 10, cells.cells)
                self.assertEqual(cells.cells[2], '1/1')
                captures.append({'streaming': streaming, 'name': name, 'requests': requests,
                                 'raw': raw, 'response': responses[0], 'result': result,
                                 'report': cells.cells, 'expected_counts': counts, 'expected_cost': cost})
                if name == 'missing-entire-usage':
                    # A supported normalized trace can retain raw output for
                    # projection, but must explicitly select its usage contract.
                    normalized = directory / 'normalized' / 'session'
                    normalized.mkdir(parents=True)
                    normalized_events = copy.deepcopy(events)
                    for event in normalized_events:
                        if event['event'] == 'model_response':
                            event['data']['usage_source'] = 'normalized'
                            event['data']['usage'] = {'input_tokens': 2000, 'output_tokens': 4,
                                                     'cached_input_tokens': 0, 'cache_write_input_tokens': 0}
                    (normalized / 'trace.jsonl').write_text(''.join(json.dumps(event) + '\n' for event in normalized_events))
                    normalized_report = directory / 'normalized.html'
                    replay = subprocess.run([binary, 'report', 'cost', '--sessions', str(normalized.parent),
                                             '--output', str(normalized_report)],
                                            env={'PATH': '', 'HOME': str(directory / 'home'), 'TMPDIR': str(directory)},
                                            capture_output=True, timeout=15)
                    self.assertEqual(replay.returncode, 0, replay.stderr)
                    normalized_cells = Cells()
                    normalized_cells.feed(normalized_report.read_text())
                    self.assertEqual(normalized_cells.cells[2], '1/1')
                    self.assertEqual(normalized_cells.cells[3], '$0.0040')
                    self.assertIn('0 unpriced', normalized_cells.cells[9])
        (root / 'RESULT.json').write_text(json.dumps(captures, indent=2) + '\n')
        self.assertEqual(len(captures), len(cases) * 2)
        for case in captures:
            with self.subTest(transport='SSE' if case['streaming'] else 'JSON', case=case['name'], gate='quantities'):
                usage = case['response']['usage']
                self.assertEqual((usage['cached_input_tokens'], usage['cache_write_input_tokens']),
                                 tuple(case['expected_counts']), 'NATIVE_CACHE_PARSER: shared validated alias quantities')
            with self.subTest(transport=case['streaming'], case=case['name'], gate='native-price'):
                cost = case['response']['estimated_cost_usd']
                if case['expected_cost'] is None:
                    self.assertIsNone(cost, 'NATIVE_RECEIPT_PRICE: absent/conflicting partitions are unknown, not clipped zero')
                else:
                    self.assertAlmostEqual(cost, case['expected_cost'], places=12)
            with self.subTest(transport=case['streaming'], case=case['name'], gate='report-price'):
                row = case['report']
                if case['expected_cost'] is None:
                    self.assertIn('1 unpriced', row[9], 'NATIVE_REPORT_RECEIPT: raw receipt remains authoritative')
                else:
                    self.assertIn('0 unpriced', row[9])
                    self.assertEqual(row[3], f"${case['expected_cost']:.4f}")


if __name__ == '__main__':
    unittest.main()
