"""Credential-free checks for the local Pi launcher's reviewer selection."""
import io
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import MagicMock, patch


SCRIPT = Path(__file__).with_name('pi_carry.py')


class ReviewerModelTest(unittest.TestCase):
    def launch(self, settings):
        proxy = MagicMock()
        proxy.poll.return_value = None
        response = MagicMock()
        response.__enter__.return_value = io.StringIO('{"mode": "compact"}')
        with tempfile.TemporaryDirectory() as home, \
                patch('pathlib.Path.home', return_value=Path(home)), \
                patch.dict('os.environ', settings, clear=True), \
                patch('sys.argv', [str(SCRIPT), '--check']), \
                patch('subprocess.Popen', return_value=proxy) as start, \
                patch('subprocess.run', return_value=MagicMock(returncode=0)), \
                patch('urllib.request.urlopen', return_value=response), \
                patch('sys.stdout', new_callable=io.StringIO):
            with self.assertRaises(SystemExit) as exit:
                runpy.run_path(str(SCRIPT), run_name='__main__')
            self.assertEqual(exit.exception.code, 0)
            command = start.call_args.args[0]
            self.command = command
        proxy.terminate.assert_called_once()
        return command[command.index('--classifier-model') + 1]

    def test_default_reviewer_is_luna_independent_of_primary_and_auth(self):
        for auth in ('codex', 'api-key'):
            for primary in ('gpt-6.1-sol', 'gpt-6-sol'):
                with self.subTest(auth=auth, primary=primary):
                    self.assertEqual(self.launch({
                        'CARRY_PI_AUTH': auth,
                        'CARRY_PI_MODEL': primary,
                    }), 'gpt-6-luna')

    def test_launcher_declares_full_history_replay(self):
        self.launch({})
        self.assertIn('--review-replayed-history', self.command)

    def test_explicit_reviewer_override_is_preserved(self):
        self.assertEqual(self.launch({
            'CARRY_PI_CLASSIFIER_MODEL': 'gpt-6-sol',
        }), 'gpt-6-sol')


if __name__ == '__main__':
    unittest.main()
