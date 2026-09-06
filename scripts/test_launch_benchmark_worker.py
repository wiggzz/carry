#!/usr/bin/env python3
"""Executable contract tests for multi-AZ benchmark worker launch fallback."""

import json
import os
import pathlib
import subprocess
import tempfile
import textwrap
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "launch_benchmark_worker.sh"
RUN_ID = "gh-12345-1"
TEMPLATES = [
    {"availability_zone": "us-west-2a", "launch_template_id": "lt-0123456789abcdef0", "version": "1"},
    {"availability_zone": "us-west-2b", "launch_template_id": "lt-0123456789abcdef1", "version": "2"},
]


class LaunchBenchmarkWorkerTests(unittest.TestCase):
    def run_launcher(self, fake_aws_body: str):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            log = root / "aws.log"
            fake_aws = fake_bin / "aws"
            fake_aws.write_text(
                "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\n' \"$*\" >> \"$AWS_LOG\"\n"
                + textwrap.dedent(fake_aws_body),
                encoding="utf-8",
            )
            fake_aws.chmod(0o755)
            user_data = root / "worker-user-data.sh"
            user_data.write_text("#!/usr/bin/env bash\n", encoding="utf-8")
            environment = {
                **os.environ,
                "PATH": str(fake_bin) + os.pathsep + os.environ["PATH"],
                "AWS_LOG": str(log),
                "RUN_ID": RUN_ID,
                "WORKER_LAUNCH_TEMPLATES": json.dumps(TEMPLATES),
                "WORKER_USER_DATA_FILE": str(user_data),
                "WORKER_TAGS": "Tags=[{Key=RunId,Value=gh-12345-1}]",
            }
            result = subprocess.run(["bash", str(SCRIPT)], text=True, capture_output=True, env=environment)
            calls = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
            return result, calls

    def test_retries_only_capacity_failure_with_next_template_and_distinct_client_token(self):
        result, calls = self.run_launcher(
            """
            if [[ "$*" == *"LaunchTemplateId=lt-0123456789abcdef0,Version=1"* ]]; then
              echo "An error occurred (InsufficientInstanceCapacity) when calling the RunInstances operation" >&2
              exit 255
            fi
            echo i-0123456789abcdef0
            """
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        environment = dict(line.split("=", 1) for line in result.stdout.splitlines())
        self.assertEqual(environment["WORKER_INSTANCE_ID"], "i-0123456789abcdef0")
        self.assertEqual(environment["WORKER_LAUNCH_ATTEMPTS"], "us-west-2a,us-west-2b")
        self.assertEqual(len(calls), 2)
        self.assertIn("--client-token gh-12345-1-1", calls[0])
        self.assertIn("--client-token gh-12345-1-2", calls[1])

    def test_stops_without_trying_another_template_for_non_capacity_error(self):
        result, calls = self.run_launcher(
            """
            echo "An error occurred (UnauthorizedOperation) when calling the RunInstances operation" >&2
            exit 255
            """
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("UnauthorizedOperation", result.stderr)
        self.assertEqual(len(calls), 1)

    def test_fails_closed_after_all_approved_templates_report_capacity_error(self):
        result, calls = self.run_launcher(
            """
            echo "An error occurred (InsufficientCapacityOnHost) when calling the RunInstances operation" >&2
            exit 255
            """
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no approved worker AZ has capacity", result.stderr)
        self.assertEqual(len(calls), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
