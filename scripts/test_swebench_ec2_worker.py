#!/usr/bin/env python3
"""Behavior test for the EC2 worker bootstrap path without AWS or Docker."""
import base64
import hashlib
import os
import pathlib
import subprocess
import tarfile
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("swebench_ec2_worker.sh")


class Ec2WorkerBootstrapTests(unittest.TestCase):
    def test_bootstrap_downloads_verifies_extracts_and_exits_without_secret(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"
            payload.mkdir()
            (payload / "marker.txt").write_text("immutable source\n")
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload / "marker.txt", arcname="marker.txt")

            fake_bin = root / "bin"
            fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                body = "#!/bin/sh\n"
                if command == "dnf":
                    body += 'printf "%s\\n" "$*" > "$FAKE_DNF_LOG"\n'
                body += "exit 0\n"
                path.write_text(body)
                path.chmod(0o755)
            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\n"
                "while [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then cp \"$FAKE_SOURCE_ARCHIVE\" \"$2\"; exit 0; fi\n"
                "  shift\n"
                "done\n"
                "exit 2\n"
            )
            curl.chmod(0o755)

            carry_root = root / "worker"
            env = dict(
                os.environ,
                PATH=f"{fake_bin}:{os.environ['PATH']}",
                SOURCE_URL_B64=base64.b64encode(b"https://example.invalid/source").decode(),
                KEY_URL_B64="",
                RESULT_URL_B64="",
                SOURCE_SHA256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                SOURCE_COMMIT="a" * 40,
                BENCHMARK_MODE="bootstrap",
                BOOTSTRAP_WAIT_SECONDS="1",
                RUN_ID="gh-test-1",
                CARRY_ROOT=str(carry_root),
                SECRET_FILE=str(root / "secret"),
                PYTHON_BIN="python3",
                SKIP_SHUTDOWN="1",
                FAKE_SOURCE_ARCHIVE=str(archive),
                FAKE_DNF_LOG=str(root / "dnf.log"),
            )
            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual((carry_root / "source" / "marker.txt").read_text(), "immutable source\n")
            self.assertFalse((root / "secret").exists())
            packages = (root / "dnf.log").read_text(encoding="utf-8").split()
            self.assertIn("python3.11", packages)
            self.assertNotIn("curl", packages)

    def test_official_worker_launches_runner_with_bounded_evaluator_concurrency(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"
            (payload / "scripts").mkdir(parents=True)
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload / "scripts", arcname="scripts")

            fake_bin = root / "bin"
            fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)

            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\n"
                "output=\n"
                "while [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then output=$2; shift 2; continue; fi\n"
                "  shift\n"
                "done\n"
                "if [ \"$output\" = \"$CARRY_ROOT/source.tar.gz\" ]; then\n"
                "  cp \"$FAKE_SOURCE_ARCHIVE\" \"$output\"\n"
                "else\n"
                "  printf fake > \"$output\"\n"
                "fi\n"
            )
            curl.chmod(0o755)

            python = fake_bin / "python3"
            python.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = -m ] && [ \"${2:-}\" = venv ]; then\n"
                "  mkdir -p \"$3/bin\"\n"
                "  printf '#!/bin/sh\\nexit 0\\n' > \"$3/bin/pip\"\n"
                "  chmod +x \"$3/bin/pip\"\n"
                "  exit 0\n"
                "fi\n"
                "case \"$*\" in\n"
                "  *swebench_smoke.py*)\n"
                "    printf 'evaluator=%s\\nmode=%s\\n' \"$EVALUATOR_CONCURRENCY\" \"$BENCHMARK_MODE\" > \"$FAKE_RUNNER_ENV\";;\n"
                "esac\n"
                "exit 0\n"
            )
            python.chmod(0o755)

            carry_root = root / "worker"
            runner_env = root / "runner.env"
            capability = base64.b64encode(b"https://example.invalid/object").decode()
            env = dict(
                os.environ,
                PATH=f"{fake_bin}:{os.environ['PATH']}",
                SOURCE_URL_B64=capability,
                KEY_URL_B64=capability,
                DOCKER_AUTH_URL_B64=capability,
                RESULT_URL_B64="",
                SOURCE_SHA256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                SOURCE_COMMIT="a" * 40,
                BENCHMARK_MODE="official-50",
                BENCHMARK_HARNESS="carry",
                BOOTSTRAP_WAIT_SECONDS="1",
                RUN_ID="gh-test-2",
                CARRY_ROOT=str(carry_root),
                SECRET_FILE=str(root / "secret"),
                PYTHON_BIN="python3",
                SKIP_SHUTDOWN="1",
                FAKE_SOURCE_ARCHIVE=str(archive),
                FAKE_RUNNER_ENV=str(runner_env),
            )
            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)

            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(runner_env.read_text(), "evaluator=5\nmode=official-50\n")

    def test_prepare_worker_uses_registry_auth_without_model_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"
            (payload / "scripts").mkdir(parents=True)
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload / "scripts", arcname="scripts")

            fake_bin = root / "bin"
            fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\n"
                "output=\n"
                "while [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then output=$2; shift 2; continue; fi\n"
                "  shift\n"
                "done\n"
                "if [ \"$output\" = \"$CARRY_ROOT/source.tar.gz\" ]; then\n"
                "  cp \"$FAKE_SOURCE_ARCHIVE\" \"$output\"\n"
                "else\n"
                "  printf '{\"username\":\"AWS\",\"token\":\"temporary\"}' > \"$output\"\n"
                "fi\n"
            )
            curl.chmod(0o755)
            python = fake_bin / "python3"
            python.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = -m ] && [ \"${2:-}\" = venv ]; then\n"
                "  mkdir -p \"$3/bin\"\n"
                "  printf '#!/bin/sh\\nexit 0\\n' > \"$3/bin/pip\"\n"
                "  chmod +x \"$3/bin/pip\"\n"
                "  exit 0\n"
                "fi\n"
                "case \"$*\" in\n"
                "  *docker_registry_login.py*) printf 'registry=%s\\n' \"${4:-}\" > \"$FAKE_LOGIN_ENV\";;\n"
                "  *swebench_smoke.py*)\n"
                "    printf 'argv=%s\\nrepository=%s\\nopenai=%s\\n' \"$*\" \"$TASK_IMAGE_REPOSITORY\" \"${OPENAI_API_KEY-unset}\" > \"$FAKE_RUNNER_ENV\";;\n"
                "esac\n"
                "exit 0\n"
            )
            python.chmod(0o755)

            carry_root = root / "worker"
            capability = base64.b64encode(b"https://example.invalid/object").decode()
            runner_env = root / "runner.env"
            login_env = root / "login.env"
            env = dict(
                os.environ,
                PATH=f"{fake_bin}:{os.environ['PATH']}",
                SOURCE_URL_B64=capability,
                KEY_URL_B64="",
                DOCKER_AUTH_URL_B64=capability,
                REGISTRY_AUTH_URL_B64=capability,
                RESULT_URL_B64="",
                SOURCE_SHA256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                SOURCE_COMMIT="a" * 40,
                BENCHMARK_MODE="prepare-50",
                BENCHMARK_HARNESS="all",
                BOOTSTRAP_WAIT_SECONDS="1",
                RUN_ID="gh-test-3",
                TASK_IMAGE_REPOSITORY="public.ecr.aws/example/carry-swebench-tasks",
                CARRY_ROOT=str(carry_root),
                PYTHON_BIN="python3",
                SKIP_SHUTDOWN="1",
                FAKE_SOURCE_ARCHIVE=str(archive),
                FAKE_RUNNER_ENV=str(runner_env),
                FAKE_LOGIN_ENV=str(login_env),
            )

            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)

            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("--prepare-images", runner_env.read_text())
            self.assertIn("repository=public.ecr.aws/example/carry-swebench-tasks", runner_env.read_text())
            self.assertIn("openai=unset", runner_env.read_text())
            self.assertEqual(login_env.read_text(), "registry=public.ecr.aws\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
