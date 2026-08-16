#!/usr/bin/env python3
"""Behavior tests for the credential-free worker lifecycle waiter."""
from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("wait_for_ec2_termination.sh")


class WaitForEc2TerminationTests(unittest.TestCase):
    def run_waiter(self, states: list[str]) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fake_aws = root / "aws"
            state_file = root / "states"
            state_file.write_text("\n".join(states) + "\n", encoding="utf-8")
            fake_aws.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "state_file=${FAKE_AWS_STATES:?}\n"
                "state=$(head -n 1 \"$state_file\")\n"
                "tail -n +2 \"$state_file\" > \"$state_file.next\"\n"
                "mv \"$state_file.next\" \"$state_file\"\n"
                "printf '%s\\n' \"$state\"\n",
                encoding="utf-8",
            )
            fake_aws.chmod(0o755)
            environment = {
                "PATH": f"{root}:{os.environ['PATH']}",
                "FAKE_AWS_STATES": str(state_file),
                "POLL_SECONDS": "0",
                "TIMEOUT_SECONDS": "5",
            }
            return subprocess.run(
                [str(SCRIPT), "i-0123456789abcdef0"],
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )

    def test_waits_through_pending_and_running_until_terminated(self):
        result = self.run_waiter(["pending", "running", "stopping", "terminated"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("terminated", result.stdout)

    def test_rejects_an_unknown_instance_state(self):
        result = self.run_waiter(["mystery"])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected instance state", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
