"""Execute the final CI gate, including failure/cancellation/skip controls."""
import itertools
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[1]


class CiWorkflowTests(unittest.TestCase):
    def test_final_check_requires_all_lanes_to_succeed(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())
        gate = workflow['jobs']['ci-status']
        self.assertEqual(set(gate['needs']), {'test', 'integration', 'native', 'python', 'infra'})
        step = gate['steps'][0]
        self.assertEqual(set(step['env'].values()),
                         {'${{ needs.' + lane + '.result }}' for lane in gate['needs']})
        for results in itertools.product(('success', 'failure', 'cancelled', 'skipped', ''), repeat=5):
            with self.subTest(results=results):
                env = {**os.environ, **dict(zip(
                    ('RUST_RESULT', 'PYTHON_RESULT', 'INFRA_RESULT', 'INTEGRATION_RESULT', 'NATIVE_RESULT'), results))}
                result = subprocess.run(['bash', '-euo', 'pipefail', '-c', step['run']],
                                        env=env, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode == 0, results == ('success',) * 5,
                                 result.stdout + result.stderr)

    def test_binary_contract_lane_fails_before_tests_if_binary_is_missing(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())
        step = next(s for s in workflow['jobs']['integration']['steps']
                    if s.get('name') == 'Proxy native binary contract tests')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake = root / 'python3'
            fake.write_text('#!/bin/sh\nprintf \'%s\\n\' "$*" >> "$RECORDED"\n')
            fake.chmod(0o755)
            record = root / 'commands'
            env = {**os.environ, 'PATH': str(root), 'RECORDED': str(record)}
            command = ['/bin/bash', '-euo', 'pipefail', '-c', step['run']]
            missing = subprocess.run(command, cwd=root, env=env, capture_output=True, timeout=5)
            self.assertNotEqual(missing.returncode, 0, 'missing binary must not silently skip contracts')
            self.assertFalse(record.exists(), 'test runner must not be invoked without the binary')
            binary = root / 'target/debug/carry'
            binary.parent.mkdir(parents=True)
            binary.write_text('#!/bin/sh\nexit 0\n')
            binary.chmod(0o755)
            present = subprocess.run(command, cwd=root, env=env, capture_output=True, timeout=5)
            self.assertEqual(present.returncode, 0, present.stderr)
            self.assertEqual(record.read_text(), '-m unittest -v scripts/test_proxy_native_fixture.py\n')


if __name__ == '__main__':
    unittest.main()
