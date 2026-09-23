"""Execute preparation heartbeat collection without cloud or model calls."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('benchmark_preparation_telemetry.py')


class PreparationTelemetryTests(unittest.TestCase):
    def run_snapshot(self, root):
        result = subprocess.run([sys.executable, str(SCRIPT), '--root', str(root), '--once'],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith('BENCHMARK_PREPARATION '))
        return json.loads(result.stdout.partition(' ')[2])

    def test_snapshot_reports_stage_counts_and_log_age_without_payloads(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prep = root / 'preparation'
            logs = prep / 'build-logs/env/sweb.env.py.x86_64.0123456789abcdef__latest'
            logs.mkdir(parents=True)
            log = logs / 'build_image.log'
            log.write_text('token=SECRET_SENTINEL https://private.invalid/?signature=SECRET_SENTINEL\nSolving environment: ...working...\n')
            os.utime(log, (1, 1))
            (prep / 'preparation-attempt.json').write_text(json.dumps({
                'phase': 'preparing', 'tasks': {'SECRET_SENTINEL': {}},
                'status_counts': {'cached': 3, 'published': 2, 'failed': 1, 'pending': 44},
                'stages': {'dependency_build': {'status': 'running'}},
            }))
            event = self.run_snapshot(root)
            self.assertEqual(event['phase'], 'preparing')
            self.assertEqual(event['dependency_status'], 'running')
            self.assertEqual(event['cached_count'], 3)
            self.assertEqual(event['published_count'], 2)
            self.assertEqual(event['failed_count'], 1)
            self.assertEqual(event['pending_count'], 44)
            self.assertEqual(event['log_count'], 1)
            self.assertEqual(event['latest_log_kind'], 'env')
            self.assertEqual(event['latest_log_activity'], 'solving')
            self.assertGreater(event['last_log_age_seconds'], 0)
            self.assertGreater(event['disk_free_bytes'], 0)
            self.assertNotIn('SECRET_SENTINEL', json.dumps(event))
            self.assertNotIn('private.invalid', json.dumps(event))

    def test_malformed_checkpoint_does_not_kill_heartbeat(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'preparation').mkdir()
            (root / 'preparation/preparation-attempt.json').write_text('{partial')
            event = self.run_snapshot(root)
            self.assertEqual(event['phase'], 'unknown')
            self.assertGreater(event['collection_errors'], 0)
            self.assertEqual(event['log_count'], 0)

    def test_untrusted_values_are_not_forwarded_and_links_are_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prep = root / 'preparation'
            logs = prep / 'build-logs/instances'
            logs.mkdir(parents=True)
            outside = root / 'SECRET_SENTINEL'
            outside.write_text('SECRET_SENTINEL')
            (logs / 'escape').symlink_to(root, target_is_directory=True)
            (prep / 'preparation-attempt.json').write_text(json.dumps({
                'phase': 'SECRET_SENTINEL',
                'status_counts': {'cached': 'SECRET_SENTINEL', 'failed': -1},
                'stages': {'dependency_build': {'status': 'SECRET_SENTINEL'}},
            }))
            event = self.run_snapshot(root)
            self.assertEqual(event['phase'], 'unknown')
            self.assertEqual(event['dependency_status'], 'unknown')
            self.assertEqual(event['cached_count'], 0)
            self.assertEqual(event['failed_count'], 0)
            self.assertNotIn('SECRET_SENTINEL', json.dumps(event))
            self.assertEqual(event['log_count'], 0)


if __name__ == '__main__':
    unittest.main()
