"""All-outcome client receipts; real credential-free loopback, no model calls.
Run with CARRY_TEST_BINARY pointing at the exact candidate executable.
"""
import http.client
import http.server
import json
import threading
import time
import unittest
import proxy_state_regressions as s


class ReceiptAccounting(s.FixtureCase):
    options = ['--review-every-requests', '100', '--review-min-primary-tokens', '1000000',
               '--primary-context-tokens', '1000000']

    def receipts(self, saved):
        return [o.get('Primary', {}).get('facts', {}).get('receipt') or
                o.get('Reviewer', {}).get('receipt')
                for o in saved['request_evidence']['observations']
                if 'Primary' in o or 'Reviewer' in o]

    def test_failed_and_incomplete_receipts_are_paid_and_compared_without_output_transition(self):
        for terminal in (getattr(self, 'terminal_status', 'failed'),):
            state = self.state / terminal
            state.mkdir()
            def primary(body, n):
                v = s.completed(response_id=f'resp-{n}', input_tokens=2000)
                if n == 2:
                    v['status'] = terminal
                    v['error'] = {'code': 'fixture_failure', 'message': 'PRIVATE_PROSE_NEVER_STORE'}
                return 200, v
            fixture = self.fixture(primary)
            proxy = s.Proxy(state, fixture, self.options)
            self.stack.callback(proxy.close)
            history = [{'role': 'user', 'content': 'same unsummarized prompt'}]
            for _ in range(3):
                self.assertEqual(proxy.send(history)[0], 200)
            saved = proxy.saved()
            stats = proxy.request('GET', '/carry/dashboard/stats')[1]
            self.evidence[terminal] = dict(saved=saved, stats=stats, wires=fixture.primaries)
            self.assertEqual(saved['primary']['calls'], 3)
            self.assertEqual(saved['completed_requests'], 2)
            self.assertEqual(saved['failed_primaries'], 1)
            row = stats['sessions'][0]['cost_estimate']
            self.assertAlmostEqual(row['observed_primary_cost'], saved['primary']['cost_usd'], places=12,
                                   msg='known provider money cannot disappear on failure')
            self.assertAlmostEqual(row['whole_window']['carry_cost'], saved['primary']['cost_usd'], places=12)
            self.assertAlmostEqual(row['whole_window']['traditional_cost'], saved['primary']['cost_usd'], places=12)
            self.assertEqual(row['traditional_compactions'], 0)
            records = self.receipts(saved)
            self.assertEqual(len(records), 3)
            self.assertEqual(records[1]['provider_status'], terminal)
            self.assertEqual(records[1]['error'], {'code': 'fixture_failure'})
            self.assertNotIn('PRIVATE_PROSE_NEVER_STORE', json.dumps(records))
            for record in records:
                self.assertEqual(record['forecast']['version'], 1)
                self.assertIsNotNone(record['forecast']['input_tokens'])
                delta = 2000 - record['forecast']['input_tokens']
                self.assertEqual(record['residual'], {'negative': delta < 0, 'absolute_tokens': abs(delta)})
                self.assertEqual(record['usage']['input_tokens'], 2000)
                self.assertTrue(record['request_id'])
            proxy.close()
            restarted = s.Proxy(state, fixture, self.options)
            self.stack.callback(restarted.close)
            self.assertEqual(restarted.saved()['primary'], saved['primary'])
            self.assertEqual(restarted.request('GET', '/carry/dashboard/stats')[1]['cost_estimate'], stats['cost_estimate'])

    def test_incomplete_receipt_has_same_all_outcome_contract(self):
        self.terminal_status = 'incomplete'
        self.test_failed_and_incomplete_receipts_are_paid_and_compared_without_output_transition()

    def test_failed_reviewer_uses_identical_receipt_owner_and_dispatch_frozen_price(self):
        def review(body, n):
            v = s.completed(input_tokens=4000)
            v['status'] = 'failed'
            return 200, v
        fixture = self.fixture(lambda b, n: (200, s.completed(input_tokens=1000)), review)
        proxy = self.proxy(fixture, ['--review-replayed-history', '--review-every-requests', '100',
                                     '--primary-context-tokens', '1000000'])
        history = [{'role': 'user', 'content': 'goal'},
                   {'role': 'assistant', 'content': 'eligible evidence ' * 500},
                   {'role': 'user', 'content': 'continue'}]
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        stats = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(saved=saved, stats=stats)
        row = stats['sessions'][0]['cost_estimate']
        self.assertAlmostEqual(row['observed_reviewer_cost'] or 0, saved['shadow']['cost_usd'], places=12)
        self.assertAlmostEqual(row['whole_window']['carry_cost'], saved['primary']['cost_usd'] + saved['shadow']['cost_usd'], places=12)
        records = self.receipts(saved)
        reviewer = next(r for r in records if r and r['role'] == 'reviewer')
        primary = next(r for r in records if r and r['role'] == 'primary')
        self.assertEqual(set(reviewer), set(primary))
        self.assertEqual(reviewer['provider_status'], 'failed')
        self.assertEqual(reviewer['usage']['input_tokens'], 4000)
        self.assertIsNotNone(reviewer['resolved'])
        self.assertIsNotNone(reviewer['rates'])
        self.assertEqual(reviewer['cost'], saved['shadow']['cost_usd'])
        self.assertEqual(reviewer['forecast']['route'], reviewer['route'])

    def test_downstream_disconnect_drains_already_dispatched_receipt(self):
        arrived, release = threading.Event(), threading.Event()
        after_paid = getattr(self, 'close_after_paid', False)
        fixture = self.fixture(lambda b, n: (200, s.completed(input_tokens=1234)))
        # A real SSE upstream flushes headers and one event, then waits until the
        # downstream caller closes. The receipt is deliberately later than close.
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a): pass
            def do_POST(self):
                self.rfile.read(int(self.headers['Content-Length']))
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                self.wfile.write(b'event: response.created\ndata: {}\n\n')
                v = s.completed(response_id='drained', input_tokens=1234)
                terminal = b'event: response.completed\ndata: ' + s.wire({'type':'response.completed','response':v}) + b'\n\n'
                if after_paid:
                    self.wfile.write(terminal)
                self.wfile.flush()
                arrived.set()
                release.wait(3)
                try:
                    if not after_paid:
                        self.wfile.write(terminal)
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError): pass
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.stack.callback(server.server_close)
        self.stack.callback(server.shutdown)
        self.stack.callback(release.set)
        fixture.url = f'http://127.0.0.1:{server.server_port}'
        proxy = self.proxy(fixture, self.options)
        conn = http.client.HTTPConnection(*proxy.address, timeout=5)
        conn.request('POST', '/v1/responses', body=s.wire({'model':'gpt-6-luna', 'stream':True,
                     'input':[{'role':'user','content':'disconnect fixture'}]}),
                     headers={'Content-Type':'application/json','x-carry-session':'state-regression'})
        response = conn.getresponse()
        self.assertTrue(arrived.wait(2))
        if after_paid:
            for _ in range(8):
                line = response.readline()
                if b'response.completed' in line:
                    break
                self.assertTrue(line, 'upstream closed before paid terminal')
            else:
                self.fail('paid terminal not delivered within fixture event count')
            self.assertIn(b'"usage"', response.readline())
        else:
            response.read(1)
        response.close()
        conn.close()
        time.sleep(0.1)
        release.set()
        saved = self.wait_saved(proxy, lambda v: v.get('primary',{}).get('calls') == 1 and not v.get('primary_pending'))
        self.evidence.update(saved=saved)
        self.assertEqual(saved['primary']['unavailable_cost_calls'], 0,
                         'dispatched provider work must settle after client close')
        self.assertEqual(saved['primary']['input_tokens'], 1234)
        record = self.receipts(saved)[0]
        self.assertTrue(record['downstream_disconnected'])
        self.assertEqual(record['provider_status'], 'completed')

    def test_downstream_close_after_paid_terminal_keeps_the_same_receipt(self):
        self.close_after_paid = True
        self.test_downstream_disconnect_drains_already_dispatched_receipt()


class FailureTelemetry(s.FixtureCase):
    options = ReceiptAccounting.options

    def timings(self, proxy):
        records = []
        directory = proxy.state / 'diagnostics'
        if directory.is_dir():
            for path in sorted(directory.glob('*.jsonl'))[-4:]:
                for line in path.read_bytes()[-65536:].splitlines():
                    try:
                        value = json.loads(line)
                    except ValueError:
                        continue
                    if value.get('event') == 'phase_timing' and value.get('data', {}).get('stage') is None:
                        records.append(value)
        return records

    def terminal_timing(self, proxy):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            records = [r for r in self.timings(proxy) if r['data'].get('outcome')]
            if records:
                return records[-1]
            time.sleep(.025)
        self.fail('bounded sink did not produce terminal timing')

    def test_http_failure_with_usage_is_separate_from_paid_settlement_and_private(self):
        value = s.completed(input_tokens=1200)
        value['status'] = 'failed'
        value['error'] = {'code':'fixture_failure', 'message':'PRIVATE_ERROR_PROSE'}
        fixture = self.fixture(lambda body, n: (429, value))
        proxy = self.proxy(fixture, self.options)
        status, _ = proxy.send([{'role':'user','content':'PRIVATE_PROMPT_PROSE'}])
        self.assertEqual(status, 429)
        saved = proxy.saved()
        record = self.terminal_timing(proxy)
        self.evidence.update(saved=saved, diagnostics=self.timings(proxy))
        data = record['data']
        self.assertEqual(data['schema_version'], 2)
        self.assertTrue(data['actual_dispatch'])
        self.assertEqual(data['attempts'][0]['http_status'], 429)
        self.assertEqual(data['receipt']['provider_status'], 'failed')
        self.assertTrue(data['receipt']['usage_present'])
        self.assertTrue(data['receipt']['pricing_available'])
        self.assertEqual(data['receipt']['usage_status'], 'numeric')
        self.assertFalse(data['downstream_disconnected'])
        self.assertIsNone(data['explicit_local_cancellation'])
        self.assertEqual(data['failure_phase'], 'upstream_body')
        self.assertEqual(saved['primary']['unavailable_cost_calls'], 0)
        dumped = json.dumps(self.timings(proxy))
        for private in ('PRIVATE_ERROR_PROSE', 'PRIVATE_PROMPT_PROSE', fixture.url):
            self.assertNotIn(private, dumped)
        self.assertLess(len(dumped), 65536)
        proxy.close()
        restarted = s.Proxy(proxy.state, fixture, self.options)
        self.stack.callback(restarted.close)
        self.assertEqual(restarted.saved()['primary'], saved['primary'])

    def test_missing_usage_is_not_free_or_a_cancel(self):
        value = s.completed(input_tokens=1200)
        value.pop('usage')
        proxy = self.proxy(self.fixture(lambda body, n: (200, value)), self.options)
        self.assertEqual(proxy.send([{'role':'user','content':'continue'}])[0], 200)
        saved = proxy.saved()
        record = self.terminal_timing(proxy)
        self.evidence.update(saved=saved, diagnostics=self.timings(proxy))
        data = record['data']
        self.assertEqual(data['schema_version'], 2)
        self.assertEqual(data['receipt']['provider_status'], 'completed')
        self.assertFalse(data['receipt']['usage_present'])
        self.assertFalse(data['receipt']['pricing_available'])
        self.assertEqual(data['receipt']['usage_status'], 'missing')
        self.assertIsNone(data['explicit_local_cancellation'])
        self.assertEqual(saved['primary']['unavailable_cost_calls'], 1)
        self.assertEqual(saved['primary']['calls'], 1)

    def test_unauthorized_receipt_survives_refresh_failure_without_phantom_retry(self):
        import base64
        home = self.state / 'synthetic-auth'
        home.mkdir()
        account = json.dumps({'https://api.openai.com/auth':{'chatgpt_account_id':'fixture-account'}}).encode()
        token = 'e30.' + base64.urlsafe_b64encode(account).decode().rstrip('=') + '.fixture'
        auth = home / 'auth.json'
        auth.write_text(json.dumps({'version':1, 'access_token':token,
                                    'refresh_token':'fixture-only', 'expires_at_ms':4102444800000}))
        def primary(body, n):
            auth.unlink()  # refresh must stop locally before its fixed external endpoint
            value = s.completed(input_tokens=1200)
            value['status'] = 'failed'
            return 401, value
        fixture = self.fixture(primary)
        proxy = self.proxy(fixture, self.options + ['--codex-login', '--codex-home', str(home)])
        self.assertEqual(proxy.send([{'role':'user','content':'offline auth rejection'}])[0], 502)
        saved = proxy.saved()
        record = self.terminal_timing(proxy)
        self.evidence.update(saved=saved, diagnostics=self.timings(proxy), wires=fixture.primaries)
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(saved['primary']['calls'], 1, 'auth refresh failure is not another provider call')
        self.assertEqual(saved['primary']['unavailable_cost_calls'], 0)
        self.assertGreater(saved['primary']['cost_usd'], 0)
        data = record['data']
        self.assertEqual(len(data['attempts']), 1)
        self.assertEqual(data['attempts'][0]['http_status'], 401)
        self.assertEqual(data['attempts'][0]['receipt']['provider_status'], 'failed')
        self.assertTrue(data['attempts'][0]['receipt']['pricing_available'])
        self.assertTrue(data['receipt']['attempt_id'].endswith('-auth0'))
        dumped = json.dumps(self.timings(proxy))
        self.assertNotIn(token, dumped)
        self.assertNotIn('fixture-only', dumped)

    def test_sink_failure_does_not_change_money_and_drop_counter_is_visible_after_recovery(self):
        fixture = self.fixture(lambda body, n: (200, s.completed(input_tokens=1200)))
        proxy = self.proxy(fixture, self.options)
        history = [{'role':'user','content':'same prompt'}]
        self.assertEqual(proxy.send(history)[0], 200)
        first = proxy.saved()['primary']
        self.terminal_timing(proxy)
        directory = proxy.state / 'diagnostics'
        parked = proxy.state / 'diagnostics-preserved'
        directory.rename(parked)
        directory.write_text('fixture-only sink obstruction')
        self.assertEqual(proxy.send(history)[0], 200)
        second = proxy.saved()['primary']
        self.assertEqual(second['calls'], first['calls'] + 1)
        self.assertAlmostEqual(second['cost_usd'], first['cost_usd'] * 2, places=12)
        time.sleep(.2)
        directory.unlink()  # only the obstruction made by this fixture
        parked.rename(directory)
        self.assertEqual(proxy.send(history)[0], 200)
        third = proxy.saved()['primary']
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline and not any(r.get('dropped_before',0) > 0 for r in self.timings(proxy)):
            time.sleep(.025)
        diagnostics = self.timings(proxy)
        self.evidence.update(diagnostics=diagnostics, first=first, second=second, third=third)
        self.assertTrue(any(r.get('dropped_before',0) > 0 for r in diagnostics))
        self.assertEqual(third['calls'], first['calls'] + 2)
        self.assertAlmostEqual(third['cost_usd'], first['cost_usd'] * 3, places=12)
        self.assertEqual(third['unavailable_cost_calls'], 0)


class ReceiptLifecycle(s.FixtureCase):
    receipts = ReceiptAccounting.receipts

    def test_summary_strategies_are_selectable_read_only_and_survive_restart(self):
        fixture = self.fixture(lambda b, n: (200, s.completed(response_id=f'summary-{n}', input_tokens=4000 if n == 1 else 5000)))
        options = ReceiptAccounting.options
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([{'role':'user','content':'seed'}], max_output_tokens=1)[0], 200)
        proxy.close()
        path = next(self.state.glob('*.json'))
        saved = json.loads(path.read_text())
        s.stage_comparison_scenario(saved, {'context_window':5000,'reserve':100,'keep_recent':200,'summary':50})
        path.write_text(json.dumps(saved))
        proxy = self.proxy(fixture, options)
        delta = [{'role':'assistant','content':'evidence '*3000}, {'role':'user','content':'continue'}]
        self.assertEqual(proxy.send(delta, previous_response_id='summary-1', max_output_tokens=1)[0], 200)
        before = proxy.saved()
        stats = proxy.request('GET', '/carry/dashboard/stats')[1]
        owner = stats['sessions'][0]['id']
        def strategies(client):
            result = {}
            for strategy in ('replacement_cold', 'appended_history', 'conservative_lower'):
                status, estimate = client.request('GET', f'/carry/dashboard/comparison/{owner}?strategy={strategy}')
                self.assertEqual(status, 200)
                self.assertEqual(estimate['summary_strategy'], strategy)
                self.assertEqual(estimate['traditional_compactions'], 1)
                self.assertGreater(estimate['summary_cost'], 0)
                self.assertEqual(estimate['observed_primary_cost'], before['primary']['cost_usd'])
                self.assertIn('not_dedicated_V2', estimate['summary_request_policy']['provenance'])
                result[strategy] = estimate
            self.assertAlmostEqual(result['conservative_lower']['summary_cost'], min(
                result['replacement_cold']['summary_cost'], result['appended_history']['summary_cost']), places=12)
            return result
        estimates = strategies(proxy)
        self.assertEqual(proxy.saved()['primary'], before['primary'])
        self.assertEqual(len(fixture.primaries), 2)
        self.assertEqual(fixture.primaries[-1]['input'][-2:], delta)
        self.assertEqual(fixture.reviews, [])
        proxy.close()
        restarted = self.proxy(fixture, options)
        self.assertEqual(strategies(restarted), estimates)
        self.assertEqual(restarted.saved()['primary'], before['primary'])
        self.evidence.update(before=before, estimates=estimates, wires=fixture.primaries)

    def test_held_timeout_and_early_eof_are_bounded_unknown_receipts_for_both_roles(self):
        for role in ('primary', 'reviewer'):
            for kind in ('timeout', 'early_eof'):
                with self.subTest(role=role, kind=kind):
                    state = self.state / (role + '-' + kind)
                    state.mkdir()
                    arrived, release = threading.Event(), threading.Event()
                    calls = {'primary': 0, 'reviewer': 0}
                    class Handler(http.server.BaseHTTPRequestHandler):
                        def log_message(self, *a): pass
                        def do_POST(self):
                            self.rfile.read(int(self.headers['Content-Length']))
                            actor = 'reviewer' if self.path == '/review' else 'primary'
                            calls[actor] += 1
                            if actor != role:
                                body = s.wire(s.completed(input_tokens=1000))
                                self.send_response(200)
                                self.send_header('Content-Length', str(len(body)))
                                self.end_headers()
                                self.wfile.write(body)
                                return
                            self.send_response(200)
                            self.send_header('Content-Type', 'text/event-stream' if role == 'primary' else 'application/json')
                            # Reviewer body reader must detect an incomplete HTTP entity.
                            if role == 'reviewer':
                                self.send_header('Content-Length', '10000')
                            self.end_headers()
                            self.wfile.write(b'event: response.created\ndata: {}\n\n' if role == 'primary' else b'{')
                            self.wfile.flush()
                            arrived.set()
                            if kind == 'timeout':
                                release.wait(4)
                            self.close_connection = True
                    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
                    server.daemon_threads = True
                    thread = threading.Thread(target=server.serve_forever, daemon=True)
                    thread.start()
                    self.stack.callback(server.server_close)
                    self.stack.callback(server.shutdown)
                    self.stack.callback(release.set)
                    fixture = self.fixture(lambda b, n: (200, s.completed()))
                    fixture.url = f'http://127.0.0.1:{server.server_port}'
                    options = (ReceiptAccounting.options if role == 'primary' else
                               ['--review-replayed-history', '--review-every-requests', '100', '--primary-context-tokens', '1000000'])
                    proxy = s.Proxy(state, fixture, options + ['--request-timeout-secs', '1', '--classifier-timeout-secs', '1'])
                    self.stack.callback(proxy.close)
                    conn = http.client.HTTPConnection(*proxy.address, timeout=4)
                    started = time.monotonic()
                    conn.request('POST', '/v1/responses', body=s.wire({'model':'gpt-6-luna',
                        'input':[{'role':'user','content':'goal'}, {'role':'assistant','content':'evidence '*500},
                                 {'role':'user','content':'continue'}], 'stream':role == 'primary'}),
                        headers={'Content-Type':'application/json', 'x-carry-session':'state-regression'})
                    response = conn.getresponse()
                    try:
                        response.read()
                    except http.client.IncompleteRead:
                        pass  # Delivery failure is independent of the paid ledger.
                    finally:
                        conn.close()
                    self.assertTrue(arrived.wait(2))
                    key = 'primary' if role == 'primary' else 'shadow'
                    saved = self.wait_saved(proxy, lambda v: v.get(key, {}).get('calls') == 1 and
                                            not v.get('primary_pending') and not v.get('background_review_pending'))
                    self.assertLess(time.monotonic() - started, 4)
                    release.set()
                    self.assertEqual(calls[role], 1)
                    self.assertEqual(saved[key]['unavailable_cost_calls'], 1)
                    self.assertEqual(saved[key]['cost_usd'], 0)
                    record = next(r for r in self.receipts(saved) if r and r['role'] == role)
                    self.assertIsNone(record['cost'])
                    self.assertIsNone(record['provider_status'])
                    if kind == 'timeout':
                        self.assertIn(record['transport'], ('upstream_timeout', 'review_deadline_expired'))
                    stats = proxy.request('GET', '/carry/dashboard/stats')[1]
                    self.assertIsNone(stats['sessions'][0]['cost_estimate']['whole_window'])
                    self.evidence[role + '-' + kind] = dict(saved=saved, stats=stats, calls=calls.copy())
                    proxy.close()
                    restarted = s.Proxy(state, fixture, options)
                    self.stack.callback(restarted.close)
                    self.assertEqual(restarted.saved()[key], saved[key])
                    restarted.close()
                    server.shutdown()

    def test_predispatch_forecast_is_frozen_and_calibration_replays_after_process_restart(self):
        arrived, release = threading.Event(), threading.Event()
        def primary(body, n):
            if n == 2:
                arrived.set()
                if not release.wait(4):
                    raise AssertionError('fixture release deadline')
            return 200, s.completed(response_id=f'forecast-{n}', input_tokens=1000 + 250 * n)
        fixture = self.fixture(primary)
        proxy = self.proxy(fixture, ReceiptAccounting.options)
        history = [{'role':'user','content':'fixed forecast input'}]
        self.assertEqual(proxy.send(history)[0], 200)
        results, errors = [], []
        def send():
            try: results.append(proxy.send(history))
            except Exception as error: errors.append(error)
        thread = threading.Thread(target=send, daemon=True)
        thread.start()
        self.stack.callback(release.set)
        self.assertTrue(arrived.wait(2))
        pending = proxy.saved()
        forecast = pending['last_primary_dispatch']['outgoing_input_tokens']
        self.assertTrue(pending['primary_pending'])
        self.assertIsNotNone(forecast)
        self.assertEqual(len(self.receipts(pending)), 1, 'a held call has a forecast, not a fabricated receipt')
        release.set()
        thread.join(4)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(results[0][0], 200)
        saved = proxy.saved()
        receipts = self.receipts(saved)
        self.assertEqual(receipts[-1]['forecast']['input_tokens'], forecast)
        values = [r['usage']['input_tokens'] - r['forecast']['input_tokens'] for r in receipts]
        estimate = proxy.request('GET', '/carry/dashboard/stats')[1]['sessions'][0]['cost_estimate']
        calibration = estimate['calibration']['primary']
        mean = sum(values) / len(values)
        self.assertEqual(calibration['samples'], len(values))
        self.assertAlmostEqual(calibration['bias_tokens'], mean)
        self.assertAlmostEqual(calibration['mean_absolute_error_tokens'], sum(map(abs, values)) / len(values))
        self.assertAlmostEqual(calibration['variance_tokens'], sum((x - mean)**2 for x in values) / len(values))
        proxy.close()
        restarted = self.proxy(fixture, ReceiptAccounting.options)
        after = restarted.request('GET', '/carry/dashboard/stats')[1]['sessions'][0]['cost_estimate']
        self.assertEqual(after['calibration'], estimate['calibration'])
        self.assertEqual(restarted.saved()['primary'], saved['primary'])
        self.assertEqual(len(fixture.primaries), 2)
        self.evidence.update(pending=pending, saved=saved, before=estimate, after=after)


if __name__ == '__main__':
    unittest.main()
