"""Runtime command acceptance: read-only readiness before any model dispatch."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from scripts.test_benchmark_comparison import contract


class RuntimeCommandTests(unittest.TestCase):
    def test_preparation_command_has_real_manifest_gate_not_unconditional_paid_refusal(self):
        config = contract('slopcodebench')
        config.update(runner_commit='af5a9cbc311605b5cceb80200ca9832bdff2d7b1',
                      dataset_commit='9cd9ca3a51c3d3e2a99d2488a25baf73a2204451')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'contract.json'
            path.write_text(json.dumps(config))
            result = subprocess.run([sys.executable, '-m', 'scripts.slopcodebench', '--contract', str(path),
                                     '--prepare-readiness'], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('requires --prepared', result.stderr)
            self.assertNotIn('unrecognized arguments', result.stderr)


if __name__ == '__main__':
    unittest.main()
