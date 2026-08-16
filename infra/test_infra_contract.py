#!/usr/bin/env python3
"""Static contracts for the benchmark's no-idle-cost AWS foundation."""

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parent


class BenchmarkInfraContractTests(unittest.TestCase):
    def read(self, name: str) -> str:
        return (ROOT / name).read_text(encoding="utf-8")

    def test_launch_template_is_ephemeral_and_has_no_inbound_access(self):
        main = self.read("main.tf")
        self.assertIn('resource "aws_launch_template" "worker"', main)
        self.assertIn('instance_initiated_shutdown_behavior = "terminate"', main)
        self.assertIn("delete_on_termination = true", main)
        self.assertIn("volume_size           = var.worker_root_volume_gib", main)
        self.assertNotIn('resource "aws_instance"', main)
        self.assertIn('values   = [aws_launch_template.worker.id]', main)
        self.assertIn('variable = "ec2:IsLaunchTemplateResource"', main)
        self.assertIn('worker_resource_tags = {', main)
        self.assertIn('ManagedBy   = "carry-swebench"', main)
        self.assertIn("subnet_id                   = var.worker_subnet_id", main)
        self.assertIn("device_index                = 0", main)
        self.assertNotIn("ingress {", main)

    def test_artifact_bucket_is_private_encrypted_and_expires_data(self):
        main = self.read("main.tf")
        self.assertIn('resource "aws_s3_bucket" "artifacts"', main)
        self.assertIn('resource "aws_s3_bucket_public_access_block" "artifacts"', main)
        self.assertIn('resource "aws_s3_bucket_server_side_encryption_configuration" "artifacts"', main)
        self.assertIn('sse_algorithm', main)
        self.assertIn('"AES256"', main)
        self.assertIn('resource "aws_s3_bucket_lifecycle_configuration" "artifacts"', main)
        self.assertIn("expiration {", main)

    def test_github_role_requires_the_protected_benchmark_environment(self):
        main = self.read("main.tf")
        self.assertIn('resource "aws_iam_role" "github_dispatch"', main)
        self.assertIn('token.actions.githubusercontent.com:aud', main)
        self.assertIn('repo:${var.github_repository}:environment:${var.github_environment}', main)
        self.assertNotIn('repo:${var.github_repository}:pull_request', main)

    def test_worker_termination_uses_the_instance_shutdown_path_only(self):
        main = self.read("main.tf")
        outputs = self.read("outputs.tf")
        self.assertIn('instance_initiated_shutdown_behavior = "terminate"', main)
        self.assertFalse((ROOT / "watchdog.tf").exists())
        self.assertFalse((ROOT / "lambda" / "watchdog.py").exists())
        self.assertFalse((ROOT / "test_watchdog.py").exists())
        self.assertIn('output "worker_launch_template_version"', outputs)
        self.assertIn('aws_launch_template.worker.latest_version', outputs)

    def test_artifact_access_is_scoped_to_an_assumed_run_session(self):
        main = self.read("main.tf")
        self.assertIn('resource "aws_iam_role" "artifact_session"', main)
        self.assertIn("&{aws:PrincipalTag/RunId}", main)
        self.assertIn('"sts:TagSession"', main)
        self.assertIn('variable = "aws:RequestTag/RunId"', main)
        self.assertNotIn('sid    = "WriteAndReadOnlyBenchmarkArtifacts"', main)

    def test_all_taggable_resources_are_marked_as_carry_related(self):
        main = self.read("main.tf")
        versions = self.read("versions.tf")
        self.assertIn('Application = "Carry"', main)
        self.assertIn('Repository  = "wiggzz/carry"', main)
        self.assertIn("default_tags", versions)

    def test_apply_script_is_the_single_noninteractive_deployment_command(self):
        apply = self.read("scripts/apply.sh")
        self.assertIn('BACKEND_CONFIG="$INFRA_DIR/backend.hcl"', apply)
        self.assertIn('terraform -chdir="$INFRA_DIR" init -reconfigure -input=false -backend-config="$BACKEND_CONFIG"', apply)
        self.assertIn('terraform -chdir="$INFRA_DIR" apply -input=false -auto-approve', apply)
        self.assertNotIn("read -r", apply)

    def test_worker_has_an_empty_instance_role_and_uses_presigned_artifacts(self):
        main = self.read("main.tf")
        self.assertIn('resource "aws_iam_role" "worker"', main)
        self.assertIn('resource "aws_iam_instance_profile" "worker"', main)
        self.assertNotIn('aws_iam_role_policy" "worker', main)
        readme = self.read("README.md")
        self.assertIn("pre-signed", readme)
        self.assertIn("model credentials", readme)
    def test_dispatch_requires_run_id_on_every_worker_resource(self):
        main = self.read("main.tf")
        dispatch = main.split('data "aws_iam_policy_document" "github_dispatch" {', 1)[1].split('resource "aws_iam_role_policy" "github_dispatch"', 1)[0]
        self.assertGreaterEqual(dispatch.count('variable = "aws:RequestTag/RunId"'), 2)
        self.assertGreaterEqual(dispatch.count('values   = ["gh-*"]'), 2)
        self.assertIn('"RunId"]', dispatch)


if __name__ == "__main__":
    unittest.main()
