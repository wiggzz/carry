#!/usr/bin/env python3
"""Regression test: the benchmark infra must consume an S3 backend config."""

import http.server
import os
import pathlib
import shutil
import subprocess
import tempfile
import threading
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
INFRA = ROOT / "infra"
TERRAFORM = shutil.which("terraform")


@unittest.skipUnless(TERRAFORM, "terraform is required")
class TerraformBackendTests(unittest.TestCase):
    def test_init_attempts_the_configured_s3_backend(self):
        terraform = TERRAFORM
        assert terraform

        class RejectS3(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(500)
                self.end_headers()

            def log_message(self, format, *args):
                del format, args

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), RejectS3)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                copied_infra = root / "infra"
                copied_infra.mkdir()
                shutil.copy2(INFRA / "versions.tf", copied_infra / "versions.tf")
                shutil.copy2(INFRA / ".terraform.lock.hcl", copied_infra / ".terraform.lock.hcl")
                shutil.copytree(INFRA / ".terraform", copied_infra / ".terraform", symlinks=True)
                backend = root / "backend.hcl"
                backend.write_text(
                    "\n".join(
                        (
                            'bucket = "carry-backend-regression-test"',
                            'key = "state.tfstate"',
                            'region = "us-west-2"',
                            f'endpoint = "http://127.0.0.1:{server.server_port}"',
                            "skip_credentials_validation = true",
                            "skip_requesting_account_id = true",
                            "skip_metadata_api_check = true",
                            "skip_region_validation = true",
                            "skip_s3_checksum = true",
                            "max_retries = 0",
                            "",
                        )
                    ),
                    encoding="utf-8",
                )
                environment = dict(
                    os.environ,
                    AWS_ACCESS_KEY_ID="test",
                    AWS_SECRET_ACCESS_KEY="test",
                    AWS_EC2_METADATA_DISABLED="true",
                )
                run = subprocess.run(
                    [
                        terraform,
                        f"-chdir={copied_infra}",
                        "init",
                        f"-plugin-dir={INFRA / '.terraform' / 'providers'}",
                        "-reconfigure",
                        "-input=false",
                        f"-backend-config={backend}",
                    ],
                    env=environment,
                    text=True,
                    capture_output=True,
                    timeout=30,
                )

                combined = run.stdout + run.stderr
                self.assertNotEqual(run.returncode, 0, combined)
                self.assertNotIn("Missing backend configuration", combined)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

    def test_apply_migrates_existing_local_state_to_the_s3_backend(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            copied_infra = root / "infra"
            shutil.copytree(INFRA, copied_infra, ignore=shutil.ignore_patterns(".terraform", "*.tfstate*", "*.tfvars", "backend.hcl"))
            copied_scripts = root / "scripts"
            copied_scripts.mkdir()
            shutil.copy2(ROOT / "scripts" / "benchmark_deployment_manifest.py", copied_scripts / "benchmark_deployment_manifest.py")
            (copied_infra / "terraform.tfvars").write_text(
                "\n".join(
                    (
                        'aws_region = "us-west-2"',
                        'artifact_bucket_name = "carry-artifacts-123456789012-us-west-2-swebench"',
                        'github_oidc_provider_arn = "arn:aws:iam::123456789012:oidc-provider/token.actions.githubusercontent.com"',
                        'worker_ami_id = "ami-12345678"',
                        'worker_subnet_id = "subnet-12345678"',
                        'root_device_name = "/dev/xvda"',
                        "",
                    )
                ),
                encoding="utf-8",
            )
            (copied_infra / "terraform.tfstate").write_text("{}\n", encoding="utf-8")
            fake_bin = root / "bin"
            fake_bin.mkdir()
            aws = fake_bin / "aws"
            aws.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' \"$*\" >> \"$FAKE_AWS_LOG\"\n"
                "case \"$1 $2\" in\n"
                "  'configure get') printf '%s\\n' us-west-2 ;;\n"
                "  'sts get-caller-identity') printf '%s\\n' 123456789012 ;;\n"
                "  'iam get-open-id-connect-provider'|'s3api head-bucket'|'s3api put-bucket-versioning'|'s3api put-bucket-encryption'|'s3api put-public-access-block'|'s3api put-bucket-tagging'|'s3 cp') exit 0 ;;\n"
                "  *) printf 'unexpected aws invocation: %s\\n' \"$*\" >&2; exit 64 ;;\n"
                "esac\n"
            )
            aws.chmod(0o755)
            terraform = fake_bin / "terraform"
            terraform.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' \"$*\" >> \"$FAKE_TERRAFORM_LOG\"\n"
                "case \"$*\" in\n"
                "  *' output -json') printf '%s\\n' '{\"artifact_bucket_name\":{\"value\":\"carry-artifacts-123456789012-us-west-2-swebench\"},\"artifact_session_role_arn\":{\"value\":\"arn:aws:iam::123456789012:role/artifact-session\"},\"github_dispatch_role_arn\":{\"value\":\"arn:aws:iam::123456789012:role/github-dispatch\"},\"task_image_publisher_role_arn\":{\"value\":\"arn:aws:iam::123456789012:role/task-publisher\"},\"task_image_repository_uri\":{\"value\":\"public.ecr.aws/example/carry-swebench-tasks\"},\"worker_launch_template_id\":{\"value\":\"lt-0123456789abcdef0\"},\"worker_launch_template_version\":{\"value\":\"7\"}}' ;;\n"
                "esac\n"
                "exit 0\n"
            )
            terraform.chmod(0o755)
            log = root / "terraform.log"
            aws_log = root / "aws.log"
            run = subprocess.run(
                ["bash", str(copied_infra / "scripts" / "apply.sh")],
                env=dict(
                    os.environ,
                    PATH=f"{fake_bin}:{os.environ['PATH']}",
                    FAKE_TERRAFORM_LOG=str(log),
                    FAKE_AWS_LOG=str(aws_log),
                ),
                text=True,
                capture_output=True,
            )

            self.assertEqual(run.returncode, 0, run.stderr)
            init = log.read_text(encoding="utf-8").splitlines()[0]
            self.assertIn("init", init)
            self.assertIn("-migrate-state", init)
            self.assertIn("-force-copy", init)
            terraform_commands = "\n".join(log.read_text(encoding="utf-8").splitlines())
            self.assertIn("-chdir=", terraform_commands)
            self.assertIn("-var github_environment=swe-bench", terraform_commands)
            self.assertIn(
                "s3 cp",
                aws_log.read_text(encoding="utf-8"),
                "apply.sh must publish the non-secret deployment manifest after apply",
            )
            self.assertIn(
                "carry/swebench-benchmark-infra/swebench.deployment.json",
                aws_log.read_text(encoding="utf-8"),
            )

    def test_apply_rejects_a_non_deterministic_state_bucket_override(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            aws = fake_bin / "aws"
            aws.write_text(
                "#!/bin/sh\n"
                "case \"$1 $2\" in\n"
                "  'sts get-caller-identity') printf '%s\\n' 123456789012 ;;\n"
                "  *) printf 'unexpected aws invocation: %s\\n' \"$*\" >&2; exit 64 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            aws.chmod(0o755)
            run = subprocess.run(
                ["bash", str(INFRA / "scripts" / "apply.sh")],
                cwd=ROOT,
                env=dict(
                    os.environ,
                    PATH=f"{fake_bin}:{os.environ['PATH']}",
                    AWS_REGION="us-west-2",
                    TF_STATE_BUCKET="other-state-bucket",
                ),
                text=True,
                capture_output=True,
            )
            self.assertEqual(run.returncode, 64)
            self.assertIn("TF_STATE_BUCKET must match the deterministic benchmark state bucket", run.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
