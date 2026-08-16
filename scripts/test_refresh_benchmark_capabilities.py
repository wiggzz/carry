#!/usr/bin/env python3
"""Behavior tests for rotating long-running benchmark upload capabilities."""
import importlib.util
import json
import pathlib
import tempfile
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("refresh_benchmark_capabilities.py")


class CapabilityRefreshTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("refresh_benchmark_capabilities", SCRIPT)
        assert spec and spec.loader
        cls.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.module)

    def test_rotation_fetches_control_and_atomically_replaces_result_capability(self):
        payload = json.dumps({
            "control_get_url": "https://bucket.s3.us-west-2.amazonaws.com/control?next",
            "result_put_url": "https://bucket.s3.us-west-2.amazonaws.com/result?next",
        }).encode()
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = payload
        with tempfile.TemporaryDirectory() as directory:
            destination = pathlib.Path(directory) / "result-url"
            next_control = self.module.rotate_once(
                "https://bucket.s3.us-west-2.amazonaws.com/control?current",
                destination,
                fetch=mock.Mock(return_value=response),
            )
            self.assertEqual(next_control, "https://bucket.s3.us-west-2.amazonaws.com/control?next")
            self.assertEqual(destination.read_text(), "https://bucket.s3.us-west-2.amazonaws.com/result?next")
            self.assertEqual(destination.stat().st_mode & 0o777, 0o600)

    def test_rotation_rejects_non_https_or_non_aws_capabilities(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = pathlib.Path(directory) / "result-url"
            for url in ("http://bucket.s3.amazonaws.com/object", "https://example.com/object"):
                payload = json.dumps({"control_get_url": url, "result_put_url": url}).encode()
                response = mock.MagicMock()
                response.__enter__.return_value.read.return_value = payload
                with self.subTest(url=url), self.assertRaises(ValueError):
                    self.module.rotate_once(
                        "https://bucket.s3.amazonaws.com/control", destination,
                        fetch=mock.Mock(return_value=response),
                    )
            self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
