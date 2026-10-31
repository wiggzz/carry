"""Behavioral controls for standalone integration orchestration."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from scripts import run_python_integration as runner


class RunnerTests(unittest.TestCase):
    def test_failure_is_reported_after_every_case_runs(self):
        seen = []
        def execute(case, binary):
            seen.append((case, binary))
            return case, int(case == 'failing'), 0, ''
        with patch.object(runner, 'run_case', side_effect=execute), contextlib.redirect_stdout(io.StringIO()):
            result = runner.run(['passing', 'failing', 'later'], '/built/carry', 2)
        self.assertEqual(set(seen), {('passing', '/built/carry'), ('failing', '/built/carry'), ('later', '/built/carry')})
        self.assertEqual(result, 1)

    def test_workers_are_bounded(self):
        lock = threading.Lock()
        active = peak = 0
        def execute(case, binary):
            nonlocal active, peak
            with lock:
                active += 1
                peak = max(peak, active)
            time.sleep(0.02)
            with lock:
                active -= 1
            return case, 0, 0, ''
        with patch.object(runner, 'run_case', side_effect=execute), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(runner.run(list('abcdef'), '/built/carry', 2), 0)
        self.assertEqual(peak, 2)

    def test_build_uses_cargo_reported_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'custom-target' / 'carry'
            binary.parent.mkdir()
            binary.write_text('#!/bin/sh\nexit 0\n')
            binary.chmod(0o755)
            artifact = {'reason': 'compiler-artifact', 'executable': str(binary),
                        'target': {'name': 'carry', 'kind': ['bin']}}
            result = subprocess.CompletedProcess([], 0, json.dumps(artifact) + '\n')
            with patch.object(runner.subprocess, 'run', return_value=result) as build:
                self.assertEqual(runner.build(), str(binary))
            self.assertEqual(build.call_args.args[0],
                             ['cargo', 'build', '--locked', '--bin', 'carry', '--message-format=json'])
            self.assertEqual(build.call_args.kwargs['cwd'], runner.ROOT)
            self.assertTrue(build.call_args.kwargs['check'])

    def test_build_failure_prevents_execution(self):
        with patch.object(runner.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'cargo')):
            with self.assertRaises(subprocess.CalledProcessError):
                runner.build()

    def test_process_receives_exact_binary_and_isolated_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'tests').mkdir()
            (root / 'tests' / 'fixture.py').write_text(
                'import os, pathlib\n'
                'assert os.environ["CARRY_TEST_BINARY"] == "/built/carry"\n'
                'e = pathlib.Path(os.environ["CARRY_TEST_EVIDENCE_DIR"])\n'
                'assert not list(e.iterdir())\n'
                '(e / "proof").write_text("isolated")\n'
                'print(e)\n')
            with patch.object(runner, 'ROOT', root), patch.dict(os.environ, {'CARRY_TEST_BINARY': '/stale'}):
                first = runner.run_case('fixture.Case.test', '/built/carry')
                second = runner.run_case('fixture.Case.test', '/built/carry')
            self.assertEqual(first[1], 0, first[3])
            self.assertEqual(second[1], 0, second[3])
            self.assertNotEqual(first[3], second[3])
            self.assertFalse(Path(first[3].strip()).exists())

    def test_timeout_is_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'tests').mkdir()
            (root / 'tests' / 'fixture.py').write_text('import time\ntime.sleep(30)\n')
            with patch.object(runner, 'ROOT', root):
                result = runner.run_case('fixture.Case.test', '/built/carry', timeout=0.05)
            self.assertEqual(result[1], 124)


if __name__ == '__main__':
    unittest.main()
