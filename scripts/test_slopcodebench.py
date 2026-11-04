"""Owned protocol fixtures for the SCB adapter. No model, Docker or dataset calls."""
import os
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from scripts.test_benchmark_comparison import contract

ROOT = Path(__file__).resolve().parents[1]
COMMAND = ROOT / 'scripts/slopcodebench.py'


class SlopCodeBenchCommandTest(unittest.TestCase):
    def test_noop_plan_preserves_official_checkpoint_lifecycle(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'contract.json'
            config = contract('slopcodebench')
            config['runner_commit'] = 'af5a9cbc311605b5cceb80200ca9832bdff2d7b1'
            config['dataset_commit'] = '9cd9ca3a51c3d3e2a99d2488a25baf73a2204451'
            config['selection'][0]['checkpoints'] = ['checkpoint_1', 'checkpoint_2']
            path.write_text(json.dumps(config))
            result = subprocess.run([sys.executable, str(COMMAND), '--contract', str(path)],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            plan = json.loads(result.stdout)
            self.assertEqual(plan['expected_records'], 4)
            self.assertEqual(plan['lifecycle']['workspace'], 'per-problem-arm-repetition')
            self.assertEqual(plan['lifecycle']['checkpoint_conversation'], 'fresh-exec')
            self.assertEqual(plan['lifecycle']['retry_conversation'], 'resume-last')
            self.assertFalse(plan['paid_dispatch'])
            blocked = subprocess.run([sys.executable, str(COMMAND), '--contract', str(path),
                                      '--allow-paid'], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(blocked.returncode, 0)
            self.assertIn('prepared isolated SCB runtime', blocked.stderr)

    @unittest.skipUnless(os.environ.get('SCB_OFFICIAL_REPORT_MODULE'),
                         'pinned official report component supplied explicitly by hosted CI')
    def test_actual_pinned_official_scorer_reads_per_checkpoint_results(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = contract('slopcodebench')
            config.update(runner_commit='af5a9cbc311605b5cceb80200ca9832bdff2d7b1',
                          dataset_commit='9cd9ca3a51c3d3e2a99d2488a25baf73a2204451')
            config['settings']['scb_pass_policy'] = 'core-cases'
            config['selection'][0]['checkpoints'] = ['checkpoint_1', 'checkpoint_2']
            path = root / 'contract.json'
            path.write_text(json.dumps(config))
            argv = [sys.executable, str(COMMAND), '--contract', str(path)]
            plan = json.loads(subprocess.run(argv, check=True, capture_output=True, text=True).stdout)
            for slot in plan['slots']:
                for checkpoint in slot['checkpoints']:
                    cp = root / 'scores' / slot['namespace'] / slot['task_id'] / checkpoint
                    cp.mkdir(parents=True)
                    passed = checkpoint == 'checkpoint_1'
                    result = {'schema_version': 1, 'problem_name': slot['task_id'], 'problem_version': 1,
                              'checkpoint_name': checkpoint, 'checkpoint_version': 1, 'duration': 2.5,
                              'entrypoint': 'python synthetic_owned_fixture.py',
                              'tests': {checkpoint + '-Core': {'passed': ['owned'] if passed else [],
                                                              'failed': [] if passed else ['owned']}},
                              'pass_counts': {'Core': int(passed)}, 'total_counts': {'Core': 1},
                              'pytest_exit_code': 0 if passed else 1, 'pytest_collected': 1,
                              'infrastructure_failure': False}
                    (cp / 'evaluation.json').write_text(json.dumps(result))
                    quality = cp / 'quality_analysis'
                    quality.mkdir()
                    (quality / 'overall_quality.json').write_text('{"lines":{"loc":3}}')
            argv += ['--score-root', str(root / 'scores'), '--official-report-module',
                     os.environ['SCB_OFFICIAL_REPORT_MODULE']]
            result = subprocess.run(argv, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            scored = json.loads(result.stdout)
            self.assertEqual(len(scored['checkpoints']), 4)
            self.assertEqual([r['resolved'] for r in scored['checkpoints']], [True, False, True, False])
            self.assertEqual(scored['checkpoints'][0]['quality']['lines']['loc'], 3)
            self.assertEqual(scored['checkpoints'][0]['evaluation_seconds'], 2.5)
            # Use the official runner's PassPolicy.check, not the report's
            # older two-string convenience method (which rejects ANY_CASE).
            config['settings']['scb_pass_policy'] = 'any-case'
            path.write_text(json.dumps(config))
            any_case = subprocess.run(argv, capture_output=True, text=True, timeout=10)
            self.assertEqual(any_case.returncode, 0, any_case.stderr)
            self.assertEqual([r['resolved'] for r in json.loads(any_case.stdout)['checkpoints']],
                             [True, False, True, False])
            # Missing official outcomes remain visible, not silently dropped.
            (cp / 'evaluation.json').unlink()
            again = subprocess.run(argv, capture_output=True, text=True, timeout=10)
            self.assertEqual(again.returncode, 0, again.stderr)
            missing = json.loads(again.stdout)['checkpoints'][-1]
            self.assertEqual(missing['status'], 'evaluation-error')
            self.assertIsNone(missing['resolved'])


if __name__ == '__main__':
    unittest.main()
