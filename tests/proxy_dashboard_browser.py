"""Delivered UI in real Chrome; synthetic HTTP data, no providers/downloads."""
import base64
from datetime import datetime, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from urllib.parse import parse_qs, urlparse
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def ledger(calls, cost, unknown=0):
    return dict(calls=calls, cost_usd=cost, input_tokens=calls * 100,
                output_tokens=calls * 10, unavailable_cost_calls=unknown)


def session_row(n):
    return dict(id=f'{n:064x}', title=f'Synthetic browser history {n} · Δ',
                client={'kind': 'agent'}, primary=ledger(1, .002),
                shadow=ledger(0, 0), completed_requests=1,
                cost_estimate=dict(carry_cost=.001, traditional_cost=.003,
                                   savings=.002, matched_primary_responses=1),
                context=dict(history_items=1, retained_items=1, removed_items=0,
                             removable_items=0, protected_items=1,
                             unreviewed_items=0, memories=0),
                last_primary_dispatch=dict(at=1794326400, model='synthetic'))


class BrowserRetention(unittest.TestCase):
    def test_many_pages_mobile_history_and_browser_working_set(self):
        driver = shutil.which('chromedriver')
        if not driver:
            driver = str(Path(os.environ.get('CHROMEWEBDRIVER', '/nonexistent')) / 'chromedriver')
        if not Path(driver).is_file():
            if os.environ.get('GITHUB_ACTIONS') == 'true':
                self.fail('Hosted browser acceptance requires the preinstalled ChromeDriver')
            self.skipTest('No preinstalled ChromeDriver; no browser downloads')
        with tempfile.TemporaryDirectory(prefix='carry-browser-') as directory:
            self.exercise_browser(Path(directory), driver)

    def exercise_browser(self, directory, driver):
        html = (ROOT / 'src/proxy_dashboard.html').read_bytes()
        pages = 120
        report = dict(scope='real Chrome delivered HTML; synthetic loopback HTTP data; not native lifecycle',
                      html_sha256=hashlib.sha256(html).hexdigest(), pages=pages,
                      rows_per_page=2, transcript_source_bytes=98304,
                      requests=[], samples=[], layouts=[], screenshots=[])
        failing = False

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                parsed = urlparse(self.path)
                query = parse_qs(parsed.query)
                status, content = 200, 'application/json'
                if parsed.path == '/favicon.ico':
                    status, body = 204, b''
                elif parsed.path == '/carry/dashboard':
                    body, content = html, 'text/html'
                elif parsed.path == '/carry/dashboard/stats':
                    first = int(query.get('after_id', ['0'])[0], 16) + 1
                    data = dict(sessions=[session_row(first), session_row(first + 1)],
                                usage=dict(primary=ledger(240, 12.345678, 1),
                                           reviewer=ledger(7, .654321), unavailable_owners=0),
                                cost_estimate=dict(carry_cost=3.25, traditional_cost=5.5,
                                                   savings=2.25, included_sessions=240),
                                session_page=dict(has_more=first + 1 < pages * 2,
                                                  next_after=f'{first + 1:064x}'),
                                unavailable_sessions=0)
                    body = json.dumps(data).encode()
                elif parsed.path.startswith('/carry/dashboard/transcript/'):
                    body = json.dumps(dict(owner=parsed.path.rsplit('/', 1)[-1],
                                           synthetic_source='x' * 98304)).encode()
                elif parsed.path == '/carry/dashboard/accounting':
                    if failing:
                        status, data = 507, {'error': 'synthetic storage bound'}
                    else:
                        data = dict(window=dict(
                            start_at=int(query['start_at'][0]), end_at=int(query['end_at'][0]),
                            coverage=dict(included_owners=3, observed_owners=5,
                                          missing_opening_owners=2, incompatible_owners=0,
                                          ambiguous_clock_owners=0),
                            usage=dict(primary=ledger(9, 7.125, 1), reviewer=ledger(2, .875),
                                       unavailable_owners=0),
                            comparison=dict(carry_cost=1.125, traditional_cost=4.625,
                                            savings=3.5, savings_percent=75.6756756757,
                                            included_owners=2, unavailable_owners=3,
                                            primary_responses=6, inferred_primary_responses=1),
                            completed_requests=7, failed_primaries=2, retention_truncated=False))
                    body = json.dumps(data).encode()
                else:
                    status, body = 404, b'not found'
                report['requests'].append(dict(path=self.path, status=status, bytes=len(body)))
                self.send_response(status)
                self.send_header('Content-Type', content)
                self.send_header('Content-Length', str(len(body)))
                self.send_header('Cache-Control', 'no-store')
                self.end_headers()
                self.wfile.write(body)

        server = HTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with socket.socket() as available:
            available.bind(('127.0.0.1', 0))
            port = available.getsockname()[1]
        with (directory / 'chromedriver.log').open('wb') as log:
            process = subprocess.Popen([driver, f'--port={port}', '--allowed-ips=127.0.0.1'],
                                       stdout=log, stderr=log, start_new_session=True)
            def stop():
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()
            session = None
            try:
                def command(path, data=None, method=None):
                    request = Request(f'http://127.0.0.1:{port}' + path,
                                      data=json.dumps(data).encode() if data is not None else None,
                                      headers={'Content-Type': 'application/json'}, method=method)
                    with urlopen(request, timeout=20) as response:
                        value = json.load(response)['value']
                    if isinstance(value, dict):
                        self.assertNotIn('error', value, value)
                    return value
                deadline = time.monotonic() + 20
                while True:
                    try:
                        command('/status')
                        break
                    except OSError:
                        self.assertIsNone(process.poll(), (directory / 'chromedriver.log').read_text())
                        self.assertLess(time.monotonic(), deadline, 'ChromeDriver startup deadline')
                        time.sleep(.1)
                created = command('/session', {'capabilities': {'alwaysMatch': {
                    'browserName': 'chrome', 'goog:loggingPrefs': {'browser': 'ALL'},
                    'goog:chromeOptions': {'args': ['--headless=new', '--no-sandbox',
                        '--disable-gpu', '--disable-dev-shm-usage', '--disable-background-networking',
                        '--no-first-run', '--window-size=1280,900',
                        f'--user-data-dir={directory / "profile"}']}}}})
                session = '/session/' + created['sessionId']
                report['browser'] = created['capabilities'].get('browserVersion')
                def cdp(name, params=None):
                    return command(session + '/goog/cdp/execute', dict(cmd=name, params=params or {}))
                def evaluate(expression):
                    result = cdp('Runtime.evaluate', dict(expression=expression, awaitPromise=True,
                                                         returnByValue=True))
                    self.assertNotIn('exceptionDetails', result, result)
                    return result['result'].get('value')
                def wait(expression):
                    deadline = time.monotonic() + 12
                    while not evaluate(expression):
                        self.assertLess(time.monotonic(), deadline, expression)
                        time.sleep(.05)
                cdp('Performance.enable')
                command(session + '/url', {'url': f'http://127.0.0.1:{server.server_port}/carry/dashboard'})
                wait("document.querySelectorAll('.session').length===2 && !busy")
                for page in range(pages):
                    self.assertEqual(evaluate("document.querySelectorAll('.session').length"), 2)
                    evaluate("document.querySelectorAll('.session-details,.transcript-view').forEach(x=>x.open=true)")
                    wait("transcriptText.size===2 && [...transcriptText.values()].every(x=>x.length>98000)")
                    self.assertTrue(evaluate("document.getElementById('cards').textContent.includes('$12.999999')"))
                    if page in [4, 39, 79, 119]:
                        cdp('HeapProfiler.collectGarbage')
                        metrics = cdp('Performance.getMetrics')
                        report['samples'].append(dict(page=page + 1,
                            heap_bytes=next(m['value'] for m in metrics['metrics'] if m['name'] == 'JSHeapUsedSize'),
                            dom=cdp('Memory.getDOMCounters'),
                            caches=evaluate("({text:transcriptText.size,requests:transcriptRequests.size,rows:document.querySelectorAll('.session').length})")))
                    if page < pages - 1:
                        previous = evaluate("document.querySelector('.session').id")
                        evaluate("document.getElementById('next-sessions').click()")
                        wait('!busy && document.querySelector(".session").id!==' + json.dumps(previous))
                self.assertTrue(evaluate("document.getElementById('next-sessions').disabled"))
                evaluate("document.getElementById('first-sessions').click()")
                wait("!busy && document.querySelector('.session').id.endsWith('0001')")
                self.assertEqual(evaluate("document.querySelectorAll('details.session-details[open],details.transcript-view[open]').length"), 0)
                self.assertEqual(evaluate('transcriptText.size'), 0)
                evaluate("document.getElementById('accounting-month').value='2026-10';document.getElementById('accounting-form').requestSubmit()")
                wait("!accountingBusy && document.getElementById('accounting-cards').children.length===4")
                report['monthly_values'] = evaluate("[...document.querySelectorAll('#accounting-cards .value')].map(x=>x.textContent)")
                self.assertEqual(report['monthly_values'], ['$8.000000', '$1.125000', '$4.625000', '$3.500000 (75.7%)'])
                status = evaluate("document.getElementById('accounting-status').textContent")
                self.assertIn('2 missing openings', status)
                self.assertIn('1 unpriced calls', status)
                for width in [1280, 390]:
                    cdp('Emulation.setDeviceMetricsOverride', dict(width=width, height=900 if width > 400 else 844,
                                                                  deviceScaleFactor=1, mobile=width < 400))
                    evaluate('window.scrollTo(0,0)')
                    layout = evaluate('({width:innerWidth,document:document.documentElement.scrollWidth})')
                    self.assertLessEqual(layout['document'], layout['width'] + 1)
                    report['layouts'].append(layout)
                    # Synthetic screenshot bytes are emitted for private inspection,
                    # not evidence inferred from DOM geometry alone.
                    screenshot = cdp('Page.captureScreenshot', {'format': 'png', 'captureBeyondViewport': False})['data']
                    report['screenshots'].append(dict(width=width, png_base64=screenshot,
                        sha256=hashlib.sha256(base64.b64decode(screenshot)).hexdigest()))
                    layout['amount_lines'] = evaluate("[...document.querySelectorAll('.card .value')].map(x=>{const r=document.createRange();r.selectNodeContents(x);return r.getClientRects().length})")
                    self.assertTrue(all(lines <= 1 for lines in layout['amount_lines']), layout)
                report['console_before_expected_507'] = command(session + '/log', {'type': 'browser'})
                self.assertFalse([x for x in report['console_before_expected_507'] if x['level'] == 'SEVERE'], report['console_before_expected_507'])
                failing = True
                evaluate("document.getElementById('accounting-form').requestSubmit()")
                wait("!accountingBusy && document.getElementById('accounting-status').textContent.includes('507')")
                self.assertEqual(evaluate("document.getElementById('accounting-cards').children.length"), 0)
                samples = report['samples']
                report['heap_growth_bytes'] = samples[-1]['heap_bytes'] - samples[0]['heap_bytes']
                self.assertLessEqual(report['heap_growth_bytes'], 16 * 1024 * 1024)
                self.assertTrue(all(s['caches'] == dict(text=2, requests=2, rows=2) for s in samples))
                self.assertLessEqual(samples[-1]['dom']['nodes'], samples[0]['dom']['nodes'] + 100)
                months = [r for r in report['requests'] if '/accounting?' in r['path']]
                self.assertEqual(len(months), 2)
                start = int(datetime(2026, 10, 1, tzinfo=timezone.utc).timestamp()) - 1
                end = int(datetime(2026, 11, 1, tzinfo=timezone.utc).timestamp()) - 1
                self.assertTrue(all(f'start_at={start}&end_at={end}' in r['path'] for r in months))
                command(session, method='DELETE')
                session = None
                report['passed'] = True
            finally:
                try:
                    if session is not None:
                        command(session, method='DELETE')
                finally:
                    stop()
                    # Keep measurements and bounded synthetic image chunks visible
                    # without workflow edits or a single enormous log line.
                    for request in report.pop('requests'):
                        print('retention-browser-request:' + json.dumps(request), flush=True)
                    for screenshot in report['screenshots']:
                        image = screenshot.pop('png_base64')
                        chunks = [image[i:i + 12000] for i in range(0, len(image), 12000)]
                        screenshot['chunks'] = len(chunks)
                        for index, chunk in enumerate(chunks):
                            print('retention-browser-image:' + json.dumps(dict(
                                width=screenshot['width'], index=index, data=chunk)), flush=True)
                    print('retention-browser-acceptance:' + json.dumps(report, separators=(',', ':')), flush=True)


if __name__ == '__main__':
    unittest.main(verbosity=2)
