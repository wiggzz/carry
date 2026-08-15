#!/usr/bin/env python3
"""Unit tests for the independent Lambda cleanup path."""

from __future__ import annotations

from datetime import datetime, timezone
import importlib.util
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
SOURCE = ROOT / "lambda" / "watchdog.py"


class FakeEC2:
    def __init__(
        self,
        tags: dict[str, str],
        state: str = "running",
        launch_time: datetime | None = None,
    ) -> None:
        self.instance = {
            "State": {"Name": state},
            "LaunchTime": launch_time or datetime(2020, 1, 1, tzinfo=timezone.utc),
            "Tags": [{"Key": key, "Value": value} for key, value in tags.items()],
        }
        self.terminated: list[list[str]] = []

    def describe_instances(self, *, InstanceIds: list[str]) -> dict:
        self.instance["InstanceId"] = InstanceIds[0]
        return {"Reservations": [{"Instances": [self.instance]}]}

    def terminate_instances(self, *, InstanceIds: list[str]) -> None:
        self.terminated.append(InstanceIds)


def load_watchdog(ec2: FakeEC2):
    fake_boto3 = types.SimpleNamespace(client=lambda service: ec2)
    spec = importlib.util.spec_from_file_location("carry_worker_watchdog", SOURCE)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    with patch.dict(
        sys.modules,
        {"boto3": fake_boto3},
    ), patch.dict("os.environ", {"MAX_WORKER_RUNTIME_MINUTES": "720"}):
        spec.loader.exec_module(module)
    return module


class WorkerWatchdogTests(unittest.TestCase):
    def tags(self, expiry: str, **extra: str) -> dict[str, str]:
        return {
            "ManagedBy": "carry-swebench",
            "Project": "carry-swebench",
            "Purpose": "benchmark-worker",
            "ExpiresAt": expiry,
            **extra,
        }

    def test_terminates_an_expired_canonical_worker(self):
        ec2 = FakeEC2(self.tags("2020-01-01T00:00:00Z"))
        watchdog = load_watchdog(ec2)

        result = watchdog.lambda_handler({"instance_id": "i-expired"}, None)

        self.assertEqual(result, {"terminated": ["i-expired"], "inspected": 1})
        self.assertEqual(ec2.terminated, [["i-expired"]])

    def test_refuses_a_worker_before_its_expiry(self):
        ec2 = FakeEC2(
            self.tags("2999-01-01T00:00:00Z"),
            launch_time=datetime.now(timezone.utc),
        )
        watchdog = load_watchdog(ec2)

        result = watchdog.lambda_handler({"instance_id": "i-early"}, None)

        self.assertEqual(result, {"terminated": [], "inspected": 1})
        self.assertEqual(ec2.terminated, [])

    def test_refuses_an_instance_without_all_canonical_tags(self):
        ec2 = FakeEC2(self.tags("2020-01-01T00:00:00Z", Project="other"))
        watchdog = load_watchdog(ec2)

        result = watchdog.lambda_handler({"instance_id": "i-other"}, None)

        self.assertEqual(result, {"terminated": [], "inspected": 1})
        self.assertEqual(ec2.terminated, [])


if __name__ == "__main__":
    unittest.main()
