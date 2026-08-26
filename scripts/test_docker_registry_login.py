#!/usr/bin/env python3
"""Behavior tests for secret-safe Docker registry authentication."""
import json
import os
import pathlib
import subprocess
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("docker_registry_login.py")


class DockerRegistryLoginTests(unittest.TestCase):
    def test_passes_token_only_on_stdin_and_removes_staging_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            argv_log = root / "argv.log"
            stdin_log = root / "stdin.log"
            docker = fake_bin / "docker"
            docker.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' \"$*\" > \"$ARGV_LOG\"\n"
                "cat > \"$STDIN_LOG\"\n"
            )
            docker.chmod(0o755)
            auth_file = root / "dockerhub-auth.json"
            token = "secret-token-value"
            auth_file.write_text(json.dumps({"username": "carry-benchmark", "token": token}))
            docker_config = root / "docker-config"
            env = dict(
                os.environ,
                PATH=f"{fake_bin}:{os.environ['PATH']}",
                ARGV_LOG=str(argv_log),
                STDIN_LOG=str(stdin_log),
            )

            run = subprocess.run(
                ["python3", str(SCRIPT), str(auth_file), str(docker_config)],
                env=env, text=True, capture_output=True,
            )

            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(
                argv_log.read_text().strip(),
                "login --username carry-benchmark --password-stdin",
            )
            self.assertEqual(stdin_log.read_text(), token)
            self.assertNotIn(token, run.stdout + run.stderr)
            self.assertFalse(auth_file.exists())
            self.assertEqual(docker_config.stat().st_mode & 0o777, 0o700)

    def test_logs_in_to_an_explicit_registry_without_exposing_token(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            argv_log = root / "argv.log"
            stdin_log = root / "stdin.log"
            docker = fake_bin / "docker"
            docker.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' \"$*\" > \"$ARGV_LOG\"\n"
                "cat > \"$STDIN_LOG\"\n"
            )
            docker.chmod(0o755)
            auth_file = root / "ecr-auth.json"
            token = "temporary-ecr-token"
            auth_file.write_text(json.dumps({"username": "AWS", "token": token}))
            env = dict(
                os.environ,
                PATH=f"{fake_bin}:{os.environ['PATH']}",
                ARGV_LOG=str(argv_log), STDIN_LOG=str(stdin_log),
            )

            run = subprocess.run(
                ["python3", str(SCRIPT), str(auth_file), str(root / "docker-config"),
                 "public.ecr.aws"],
                env=env, text=True, capture_output=True,
            )

            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(
                argv_log.read_text().strip(),
                "login --username AWS --password-stdin public.ecr.aws",
            )
            self.assertEqual(stdin_log.read_text(), token)
            self.assertNotIn(token, run.stdout + run.stderr)
            self.assertFalse(auth_file.exists())

    def test_failure_removes_staging_file_without_echoing_secret(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            docker = fake_bin / "docker"
            docker.write_text("#!/bin/sh\ncat >/dev/null\necho registry-failed >&2\nexit 1\n")
            docker.chmod(0o755)
            auth_file = root / "dockerhub-auth.json"
            token = "secret-token-value"
            auth_file.write_text(json.dumps({"username": "carry-benchmark", "token": token}))
            env = dict(os.environ, PATH=f"{fake_bin}:{os.environ['PATH']}")

            run = subprocess.run(
                ["python3", str(SCRIPT), str(auth_file), str(root / "docker-config")],
                env=env, text=True, capture_output=True,
            )

            self.assertNotEqual(run.returncode, 0)
            self.assertNotIn(token, run.stdout + run.stderr)
            self.assertFalse(auth_file.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
