"""Runtime command acceptance: read-only readiness before any model dispatch."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from scripts.test_benchmark_comparison import contract


class RuntimeCommandTests(unittest.TestCase):
    def test_cumulative_lifecycles_preserve_ownership_duplicates_and_unknowns(self):
        from scripts import slopcodebench_runtime as runtime, proxy_benchmark
        from scripts.test_swebench_comparison import events
        start, terminal = events()
        start['actor'] = terminal['actor'] = 'shadow'
        times, owners, observed = {'one': {}, 'two': {}}, {}, {}
        primary = [{**event, 'request_id': 'other-primary'} for event in events()]
        summary = lambda: proxy_benchmark.summarize_events(primary + times['one']['events'])
        runtime.reconcile_events(times, owners, observed, [start], 'one')
        self.assertFalse(summary()['ledger_complete'])
        runtime.reconcile_events(times, owners, observed, [start, terminal], 'two')
        runtime.reconcile_events(times, owners, observed, [start, terminal], 'two')
        self.assertEqual(times['one']['events'], [start, terminal])
        self.assertNotIn('events', times['two'])
        self.assertTrue(summary()['ledger_complete'])
        runtime.reconcile_events(times, owners, observed, [start], 'two')
        self.assertEqual(times['one']['events'], [start, terminal], 'lossy later snapshots cannot erase spend')
        runtime.reconcile_events(times, owners, observed, [start, terminal, terminal], 'two')
        self.assertEqual(times['one']['events'], [start, terminal, terminal])
        self.assertFalse(summary()['ledger_complete'],
                         'real duplicate terminals must reach the authoritative validator')

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
