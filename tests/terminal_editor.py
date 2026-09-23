"""Unix PTY smoke test. Run after cargo build: python3 tests/terminal_editor.py."""
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import tempfile
import termios
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


check(False)
check(True)
check_redirected_stdout()
print('PTY checks passed: Alt+Enter, bracketed paste, draft preservation, redirected stdout, exit.')
