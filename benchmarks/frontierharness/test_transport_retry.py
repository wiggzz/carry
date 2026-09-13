#!/usr/bin/env python3
"""Regression coverage for Runta readiness retries used during provisioning."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import unittest


HELPER = Path(__file__).parents[2] / "scripts" / "frontierharness_transport_retry.sh"


def retry_environment(extra: dict[str, str] | None = None) -> dict[str, str]:
    environment = {"PATH": os.environ["PATH"]}
    if extra:
        environment.update(extra)
    completed = subprocess.run(
        [
            "bash", "-c",
            'source "$1"; run_with_frontierharness_ready_retries env',
            "--", str(HELPER),
        ],
        text=True,
        capture_output=True,
        env=environment,
    )
    if completed.returncode:
        raise AssertionError(completed.stderr)
    return dict(line.split("=", 1) for line in completed.stdout.splitlines() if "=" in line)


class FrontierHarnessTransportRetryTests(unittest.TestCase):
    def test_defaults_cover_normal_runta_runtime_startup(self) -> None:
        values = retry_environment()
        self.assertEqual(values["FH_TRANSPORT_ATTEMPTS"], "12")
        self.assertEqual(values["FH_RETRY_DELAY"], "5")

    def test_explicit_operator_retry_values_are_preserved(self) -> None:
        values = retry_environment({"FH_TRANSPORT_ATTEMPTS": "20", "FH_RETRY_DELAY": "1"})
        self.assertEqual(values["FH_TRANSPORT_ATTEMPTS"], "20")
        self.assertEqual(values["FH_RETRY_DELAY"], "1")


if __name__ == "__main__":
    unittest.main()
