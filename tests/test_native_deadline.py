#!/usr/bin/env python3
"""Real native CLI against synthetic loopback HTTP; no provider or credentials."""
import argparse
import http.server
import json
import os
import pathlib
import signal
import subprocess
import tempfile
import threading
import time
import unittest

BINARY = None


class NativeDeadlineTests(unittest.TestCase):
    def exercise(self, *, deadline=False, truncated=False, invalid=False, shell=False, tool_deadline=False):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root/'home').mkdir()
            workspace = root/'workspace'; workspace.mkdir()
            requests = []
            entered = threading.Event(); release = threading.Event()
            class Handler(http.server.BaseHTTPRequestHandler):
                def log_message(self, *_): pass
                def do_POST(self):
                    requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                    entered.set()
                    if len(requests) == 1:
                        if not release.wait(5): return
                        if truncated:
                            body = b'data: {"type":"response.created"}\n\n'
                            self.send_response(200)
                            self.send_header('Content-Type', 'text/event-stream')
                            self.send_header('Content-Length', str(len(body)+1))
                            self.end_headers(); self.wfile.write(body); self.close_connection = True
                            return
                    name = 'shell' if shell else 'finish'
                    arguments = {'command': 'touch forbidden'} if shell else {'answer': 'done'}
                    if tool_deadline:
                        name = 'shell'
                        arguments = {'command': 'echo $$ > shell.pid; touch running; (sleep 2; touch forbidden) & wait'}
                    arguments['context'] = {'protected': [], 'removable': [], 'remember': []}
                    raw = {'id': 'synthetic', 'output': [{'type':'function_call', 'call_id':'call-1',
                        'name':name, 'arguments':'invalid-json' if invalid else json.dumps(arguments)}],
                        'usage': {'input_tokens':123,'output_tokens':4,
                                  'input_tokens_details':{'cached_tokens':0}}}
                    body = json.dumps(raw).encode()
                    self.send_response(200); self.send_header('Content-Type','application/json')
                    self.send_header('Content-Length',str(len(body))); self.end_headers()
                    try: self.wfile.write(body)
                    except (BrokenPipeError, ConnectionResetError): pass
            server = http.server.ThreadingHTTPServer(('127.0.0.1',0), Handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
            env = {k:v for k,v in os.environ.items() if not k.startswith(('OPENAI_', 'CARRY_', 'CODEX_'))}
            env.update(HOME=str(root/'home'), CARRY_HOME=str(root/'home'), OPENAI_API_KEY='synthetic-only',
                       OPENAI_BASE_URL=f'http://127.0.0.1:{server.server_port}/v1')
            process = subprocess.Popen([BINARY,'--cwd',str(workspace),'--session-dir',str(root/'run'),
                '--model','gpt-6-sol','--compaction-policy','disabled','-p','finish'],
                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                self.assertTrue(entered.wait(10), 'native request never reached fixture')
                if deadline:
                    process.send_signal(signal.SIGUSR1)
                    # Ensure the native signal task observes shutdown before response EOF.
                    time.sleep(0.15)
                release.set()
                if tool_deadline:
                    until = time.monotonic()+5
                    while not (workspace/'running').exists() and time.monotonic()<until:
                        time.sleep(.01)
                    self.assertTrue((workspace/'running').exists(), 'native shell did not start')
                    process.send_signal(signal.SIGUSR1)
                stdout, stderr = process.communicate(timeout=10)
                trace_file = root/'run/trace.jsonl'
                events = [json.loads(line) for line in trace_file.read_text().splitlines()] if trace_file.exists() else []
                result_file = root/'run/result.json'
                result = json.loads(result_file.read_text()) if result_file.exists() else None
                return dict(requests=[{'model': r.get('model')} for r in requests], events=events, result=result,
                    forbidden=(workspace/'forbidden').exists(), code=process.returncode,
                    stderr=stderr.decode(), stdout=stdout.decode())
            finally:
                release.set()
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)
                if (workspace/'shell.pid').exists():
                    try: os.killpg(int((workspace/'shell.pid').read_text()), signal.SIGKILL)
                    except ProcessLookupError: pass
                server.shutdown(); server.server_close(); thread.join(timeout=2)

    def test_deadline_drains_receipt_without_dispatching_tool(self):
        evidence = self.exercise(deadline=True, shell=True)
        self.assertIsNotNone(evidence['result'], evidence)
        self.assertFalse(evidence['result']['completed'])
        self.assertEqual(evidence['result']['usage']['input_tokens'], 123)
        self.assertFalse(evidence['forbidden'])
        self.assertEqual(len(evidence['requests']),1)

    def test_deadline_truncated_stream_never_retries(self):
        evidence = self.exercise(deadline=True, truncated=True)
        self.assertIsNotNone(evidence['result'], evidence)
        self.assertFalse(evidence['result']['completed'])
        self.assertEqual(len(evidence['requests']),1)

    def test_predeadline_truncated_stream_retry_remains_visible(self):
        evidence = self.exercise(truncated=True)
        self.assertEqual(len(evidence['requests']),2, evidence)
        self.assertEqual(evidence['result']['response_retries'],1)
        self.assertEqual(evidence['result']['usage']['input_tokens'],123)

    def test_invalid_action_still_persists_native_metering(self):
        evidence = self.exercise(invalid=True)
        responses = [e for e in evidence['events'] if e['event']=='model_response']
        self.assertEqual(len(responses),1, evidence)
        self.assertEqual(responses[0]['data']['raw']['usage']['input_tokens'],123)
        self.assertFalse(evidence['result']['completed'])
        self.assertEqual(evidence['result']['usage']['input_tokens'],123)


    def test_active_shell_is_quiesced_before_deadline_artifacts(self):
        evidence = self.exercise(tool_deadline=True)
        self.assertFalse(evidence['forbidden'], evidence)
        self.assertIsNotNone(evidence['result'], evidence)
        self.assertFalse(evidence['result']['completed'])
        self.assertEqual(len(evidence['requests']),1)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--binary', required=True)
    args, rest = parser.parse_known_args(); BINARY = str(pathlib.Path(args.binary).resolve())
    unittest.main(argv=[__file__, *rest])
