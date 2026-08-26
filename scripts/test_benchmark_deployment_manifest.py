#!/usr/bin/env python3
"""Behavioral contract for the non-secret benchmark deployment manifest."""

import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "benchmark_deployment_manifest.py"


class BenchmarkDeploymentManifestTests(unittest.TestCase):
    def test_writes_a_complete_nonsecret_manifest_from_terraform_outputs(self):
        outputs = {
            "artifact_bucket_name": {"value": "carry-artifacts-123456789012-us-west-2-swebench"},
            "artifact_session_role_arn": {"value": "arn:aws:iam::123456789012:role/artifact-session"},
            "github_dispatch_role_arn": {"value": "arn:aws:iam::123456789012:role/github-dispatch"},
            "task_image_publisher_role_arn": {"value": "arn:aws:iam::123456789012:role/task-publisher"},
            "task_image_repository_uri": {"value": "public.ecr.aws/example/carry-swebench-tasks"},
            "worker_launch_template_id": {"value": "lt-0123456789abcdef0"},
            "worker_launch_template_version": {"value": "7"},
        }
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            terraform_output = root / "outputs.json"
            manifest = root / "deployment.json"
            terraform_output.write_text(json.dumps(outputs), encoding="utf-8")
            run = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "write",
                    "--terraform-output",
                    str(terraform_output),
                    "--backend-bucket",
                    "carry-tfstate-123456789012-us-west-2-swebench",
                    "--backend-key",
                    "carry/swebench-benchmark-infra/swebench.tfstate",
                    "--backend-region",
                    "us-west-2",
                    "--output",
                    str(manifest),
                ],
                text=True,
                capture_output=True,
            )

            self.assertEqual(run.returncode, 0, run.stderr)
            document = json.loads(manifest.read_text(encoding="utf-8"))
            self.assertEqual(document["schema"], "carry.swebench-deployment.v1")
            self.assertEqual(document["terraform_backend"], {
                "bucket": "carry-tfstate-123456789012-us-west-2-swebench",
                "key": "carry/swebench-benchmark-infra/swebench.tfstate",
                "region": "us-west-2",
            })
            self.assertEqual(document["aws_region"], "us-west-2")
            self.assertEqual(document["artifact_bucket"], outputs["artifact_bucket_name"]["value"])
            self.assertNotIn("secret", json.dumps(document).lower())

    def test_resolves_a_verified_manifest_to_github_environment_values(self):
        manifest = {
            "schema": "carry.swebench-deployment.v1",
            "terraform_backend": {
                "bucket": "carry-tfstate-123456789012-us-west-2-swebench",
                "key": "carry/swebench-benchmark-infra/swebench.tfstate",
                "region": "us-west-2",
            },
            "aws_region": "us-west-2",
            "artifact_bucket": "carry-artifacts-123456789012-us-west-2-swebench",
            "artifact_session_role_arn": "arn:aws:iam::123456789012:role/artifact-session",
            "github_dispatch_role_arn": "arn:aws:iam::123456789012:role/github-dispatch",
            "task_image_publisher_role_arn": "arn:aws:iam::123456789012:role/task-publisher",
            "task_image_repository": "public.ecr.aws/example/carry-swebench-tasks",
            "worker_launch_template_id": "lt-0123456789abcdef0",
            "worker_launch_template_version": "7",
        }
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            manifest_path = root / "deployment.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            run = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "resolve",
                    "--manifest",
                    str(manifest_path),
                    "--expected-backend-bucket",
                    manifest["terraform_backend"]["bucket"],
                    "--expected-backend-key",
                    manifest["terraform_backend"]["key"],
                    "--expected-backend-region",
                    "us-west-2",
                    "--expected-dispatch-role-arn",
                    manifest["github_dispatch_role_arn"],
                    "--catalog-digest",
                    "sha256:" + "a" * 64,
                ],
                text=True,
                capture_output=True,
            )

            self.assertEqual(run.returncode, 0, run.stderr)
            environment = dict(line.split("=", 1) for line in run.stdout.splitlines())
            self.assertEqual(environment["ARTIFACT_BUCKET"], manifest["artifact_bucket"])
            self.assertEqual(environment["TASK_IMAGE_CATALOG"], manifest["task_image_repository"] + "@sha256:" + "a" * 64)
            self.assertEqual(environment["CONFIGURATION_MANIFEST_SHA256"], hashlib.sha256(manifest_path.read_bytes()).hexdigest())

            bare_digest_command = list(run.args)
            bare_digest_command[-1] = "b" * 64
            bare_digest_run = subprocess.run(bare_digest_command, text=True, capture_output=True)
            self.assertEqual(bare_digest_run.returncode, 0, bare_digest_run.stderr)
            bare_environment = dict(line.split("=", 1) for line in bare_digest_run.stdout.splitlines())
            self.assertEqual(
                bare_environment["TASK_IMAGE_CATALOG"],
                manifest["task_image_repository"] + "@sha256:" + "b" * 64,
            )

            manifest["github_dispatch_role_arn"] = "arn:aws:iam::123456789012:role/unexpected-role"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            mismatched_run = subprocess.run(run.args, text=True, capture_output=True)
            self.assertEqual(mismatched_run.returncode, 2)
            self.assertIn("does not match the protected bootstrap role", mismatched_run.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
