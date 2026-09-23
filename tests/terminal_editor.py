"""Unix PTY smoke test. Run after cargo build: python3 tests/terminal_editor.py."""
import fcntl
import http.server
import json
import os
import pty
import select
import struct
import subprocess
import tempfile
import termios
import threading
import time


def check(pasted):
    with tempfile.TemporaryDirectory() as directory:
        steps = os.path.join(directory, 'steps.jsonl')
        context = {'protected': [], 'removable': [], 'remember': []}
        actions = [
            {'kind': 'shell', 'command': 'sleep 1', 'message': 'Checking draft preservation.'},
            {'kind': 'finish', 'answer': 'done'},
            {'kind': 'finish', 'answer': 'follow-up done'},
        ]
        with open(steps, 'w') as file:
            for action in actions:
                file.write(json.dumps({'action': action, 'context': context}) + '\n')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
        session = os.path.join(directory, 'session')
        process = subprocess.Popen(
            ['target/debug/carry', '--interactive', '--scripted-steps', steps,
             '--session-dir', session], stdin=slave, stdout=slave, stderr=slave,
            env=dict(os.environ, TERM='xterm-256color'))
        os.close(slave)
        output = bytearray()

        def pump(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError:
                        break
                    output.extend(chunk)
                    # Respond to cursor-position queries from the line editor.
                    for _ in range(chunk.count(b'\x1b[6n')):
                        os.write(master, b'\x1b[1;1R')

        def events():
            try:
                with open(os.path.join(session, 'trace.jsonl')) as file:
                    return [json.loads(line) for line in file if line.strip()]
            except FileNotFoundError:
                return []

        try:
            pump(.5)
            if pasted:
                os.write(master, b'\x1b[200~first\n\n  second\x1b[201~')
                pump(.5)
                assert not events(), 'paste submitted without Enter'
                expected = 'first\n\n  second'
                os.write(master, b'\r')
            else:
                os.write(master, b'first\x1b\rsecond\r')
                expected = 'first\nsecond'
            pump(.5)
            os.write(master, b'draft\x1b\rretained')
            pump(2)  # Shell finishes and output is printed with a draft in the editor.
            started = next(e for e in events() if e['event'] == 'run_started')
            assert started['data']['prompt'] == expected, started
            assert [e['data']['message'] for e in events() if e['event'] == 'human_message'] == [expected]
            os.write(master, b'\r')
            pump(1)
            messages = [e['data']['message'] for e in events() if e['event'] == 'human_message']
            assert messages == [expected, 'draft\nretained'], messages
            assert b'\x1b[?1049h' not in output, 'editor switched to alternate screen'
            os.write(master, b'/quit\r')
            pump(.5)
            assert process.poll() == 0, 'editor failed to exit'
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
            os.close(master)


def check_redirected_stdout():
    with tempfile.TemporaryDirectory() as directory:
        steps = os.path.join(directory, 'steps.jsonl')
        context = {'protected': [], 'removable': [], 'remember': []}
        with open(steps, 'w') as file:
            file.write(json.dumps({
                'action': {'kind': 'finish', 'answer': '# Answer'},
                'context': context,
            }) + '\n')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
        session = os.path.join(directory, 'session')
        process = subprocess.Popen(
            ['target/debug/carry', '--interactive', '--scripted-steps', steps,
             '--session-dir', session], stdin=slave, stdout=subprocess.PIPE, stderr=slave,
            env=dict(os.environ, TERM='xterm-256color'))
        os.close(slave)
        terminal_output = bytearray()

        def pump(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    try:
                        terminal_output.extend(os.read(master, 65536))
                    except OSError:
                        break

        try:
            pump(.3)
            os.write(master, b'prompt\r')
            pump(1)
            os.write(master, b'/quit\r')
            pump(.5)
            assert process.poll() == 0, 'redirected interactive session failed to exit'
            assert process.stdout is not None
            stdout = process.stdout.read()
            assert stdout == b'# Answer\n', stdout
            assert b'\x1b[' not in stdout, stdout
            assert b'terminal editor failed:' not in terminal_output, terminal_output
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
            os.close(master)


def check_stream_exit_order():
    prefix = 'STREAM_PREFIX_7a9'
    suffix = '_STREAM_SUFFIX_8b2'
    first_sent = threading.Event()
    release = threading.Event()

    class Responses(http.server.BaseHTTPRequestHandler):
        def log_message(self, format, *args):
            del format, args

        def do_POST(self):
            length = int(self.headers.get('Content-Length', '0'))
            self.rfile.read(length)
            arguments = json.dumps({
                'answer': prefix + suffix,
                'context': {'protected': [], 'removable': [], 'remember': []},
            }, separators=(',', ':'))
            initial = [
                {'type': 'response.output_item.added',
                 'item': {'type': 'function_call', 'name': 'finish'}},
                {'type': 'response.function_call_arguments.delta',
                 'delta': '{"answer":"' + prefix},
            ]
            final_item = {
                'type': 'function_call', 'call_id': 'call-1',
                'name': 'finish', 'arguments': arguments,
            }
            remaining = [
                {'type': 'response.function_call_arguments.delta',
                 'delta': suffix + '","context":{"protected":[],"removable":[],"remember":[]}}'},
                {'type': 'response.output_item.done', 'item': final_item},
                {'type': 'response.completed', 'response': {
                    'id': 'response-1', 'output': [final_item], 'usage': {},
                }},
            ]

            def frames(events):
                return ''.join('data: ' + json.dumps(event, separators=(',', ':')) + '\n\n'
                               for event in events).encode()

            first = frames(initial)
            rest = frames(remaining)
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(first) + len(rest)))
            self.end_headers()
            self.wfile.write(first)
            self.wfile.flush()
            first_sent.set()
            release.wait(3)
            self.wfile.write(rest)
            self.wfile.flush()

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Responses)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    with tempfile.TemporaryDirectory() as directory:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
        process = subprocess.Popen(
            ['target/debug/carry', '--interactive', '--api-base',
             f'http://127.0.0.1:{server.server_port}', '--session-dir',
             os.path.join(directory, 'session')], stdin=slave, stdout=slave, stderr=slave,
            env=dict(os.environ, TERM='xterm-256color', OPENAI_API_KEY='test-key'))
        os.close(slave)
        output = bytearray()

        def pump(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError:
                        break
                    output.extend(chunk)
                    for _ in range(chunk.count(b'\x1b[6n')):
                        os.write(master, b'\x1b[1;1R')

        try:
            pump(.3)
            os.write(master, b'prompt\r')
            assert first_sent.wait(3), 'server did not emit the first stream fragment'
            os.write(master, b'/quit\r')
            pump(.3)
            release.set()
            deadline = time.monotonic() + 5
            while process.poll() is None and time.monotonic() < deadline:
                pump(.1)
            pump(.2)
            assert process.poll() == 0, output
            prefix_index = output.find(prefix.encode())
            suffix_index = output.find(suffix.encode())
            assert prefix_index >= 0 and suffix_index >= 0, output
            assert prefix_index < suffix_index, (prefix_index, suffix_index, output)
        finally:
            release.set()
            if process.poll() is None:
                process.kill()
            process.wait()
            os.close(master)
            server.shutdown()
            server.server_close()
            server_thread.join()


check(False)
check(True)
check_redirected_stdout()
check_stream_exit_order()
print('PTY checks passed: Alt+Enter, bracketed paste, draft preservation, redirected stdout, stream exit order.')
