#!/usr/bin/env python3
"""Behavior tests for S3 pre-signed capability generation."""
import importlib.util
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).with_name("presign_s3.py")


class FakeClient:
    def __init__(self):
        self.calls = []

    def generate_presigned_url(self, operation, **kwargs):
        self.calls.append((operation, kwargs))
        return "https://example.invalid/capability"


class PresignTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("presign_s3", SCRIPT)
        assert spec and spec.loader
        cls.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.module)

    def test_put_capability_is_signed_for_put_object_and_http_put(self):
        client = FakeClient()
        url = self.module.generate(
            client, bucket="bucket", key="runs/run/results.tar.gz", method="put", expires=21600,
        )
        self.assertEqual(url, "https://example.invalid/capability")
        self.assertEqual(client.calls, [(
            "put_object",
            {"Params": {"Bucket": "bucket", "Key": "runs/run/results.tar.gz"},
             "ExpiresIn": 21600, "HttpMethod": "PUT"},
        )])

    def test_rejects_unknown_method_and_overlong_lifetime(self):
        for values in ({"method": "delete", "expires": 10}, {"method": "get", "expires": 21601}):
            with self.assertRaises(ValueError):
                self.module.generate(
                    FakeClient(), bucket="bucket", key="key",
                    method=values["method"], expires=values["expires"],
                )


if __name__ == "__main__":
    unittest.main(verbosity=2)
