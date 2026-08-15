#!/usr/bin/env python3
"""Contracts for the protected, credential-free EC2 bootstrap workflow."""

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "benchmark-ec2-bootstrap.yml"


class BenchmarkEc2BootstrapContracts(unittest.TestCase):
    def test_bootstrap_workflow_is_manual_protected_and_uses_oidc(self):
        self.assertTrue(WORKFLOW.is_file(), "missing EC2 bootstrap workflow")
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", workflow)
        self.assertNotIn("pull_request:", workflow)
        self.assertIn("environment: swe-bench", workflow)
        self.assertIn("id-token: write", workflow)
        self.assertIn("aws-actions/configure-aws-credentials@", workflow)

    def test_workflow_launches_only_canonical_workers_and_always_cleans_its_instance(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("aws sts assume-role", workflow)
        self.assertIn("BENCHMARK_ARTIFACT_SESSION_ROLE_ARN", workflow)
        self.assertIn("BENCHMARK_WORKER_LAUNCH_TEMPLATE_VERSION", workflow)
        self.assertIn("concurrency:", workflow)
        self.assertIn("cancel-in-progress: false", workflow)
        self.assertIn("aws ec2 run-instances", workflow)
        self.assertIn("--min-count 1", workflow)
        self.assertIn("--max-count 1", workflow)
        self.assertIn("Version=$LAUNCH_TEMPLATE_VERSION", workflow)
        self.assertIn("ResourceType=instance", workflow)
        self.assertIn("ResourceType=volume", workflow)
        self.assertIn("ManagedBy,Value=carry-swebench", workflow)
        self.assertIn("if: always()", workflow)
        self.assertIn("aws ec2 terminate-instances", workflow)
        self.assertNotIn("secrets.", workflow)

    def test_artifact_session_credentials_are_used_only_in_the_staging_step(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn('source "$RUNNER_TEMP/artifact-session.env"', workflow)
        self.assertIn('aws s3 cp carry-source.tar.gz', workflow)
        self.assertIn('aws s3 presign', workflow)
    def test_ci_runs_the_bootstrap_workflow_contracts(self):
        ci = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        self.assertIn("scripts/test_benchmark_ec2_bootstrap.py", ci)
    def test_isolation_policy_requires_an_external_boundary_for_every_agent_harness(self):
        policy = ROOT / "docs" / "benchmark-isolation.md"
        self.assertTrue(policy.is_file(), "missing benchmark isolation policy")
        text = policy.read_text(encoding="utf-8")
        self.assertIn("external isolation boundary", text)
        self.assertIn("Carry, Codex", text)
        self.assertIn("must not receive model credentials", text)
        self.assertIn("Docker socket", text)


if __name__ == "__main__":
    unittest.main()
