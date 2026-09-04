#!/usr/bin/env python3
"""Behavior tests for compact EC2 user-data rendering."""
import pathlib
import tempfile
import unittest

from scripts.render_swebench_worker_user_data import render_user_data


ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKER = ROOT / "scripts" / "swebench_ec2_worker.sh"


class RenderWorkerUserDataTests(unittest.TestCase):
    def test_single_config_capability_keeps_real_worker_user_data_under_ec2_limit(self):
        # Presigned URLs contain large STS session tokens. Model a long but valid
        # one to prove that user data carries exactly one capability, not all
        # per-run credential-bearing URLs.
        config_url = "https://example.invalid/bootstrap?X-Amz-Security-Token=" + "x" * 3500
        rendered = render_user_data(WORKER.read_text(encoding="utf-8"), config_url)

        self.assertLessEqual(len(rendered.encode("utf-8")), 16_384)
        header, _worker = rendered.split("# BEGIN CARRY EC2 WORKER\n", 1)
        self.assertIn("BOOTSTRAP_CONFIG_URL_B64=", header)
        self.assertNotIn("SOURCE_URL_B64=", header)
        self.assertNotIn("RESULT_URL_B64=", header)

        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / "worker-user-data.sh"
            target.write_text(rendered, encoding="utf-8")
            self.assertTrue(target.stat().st_size <= 16_384)


if __name__ == "__main__":
    unittest.main()
