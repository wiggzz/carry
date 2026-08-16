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
                path.write_text("#!/bin/sh\nexit 0\n")
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
                SKIP_SHUTDOWN="1",
                FAKE_SOURCE_ARCHIVE=str(archive),
            )
            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual((carry_root / "source" / "marker.txt").read_text(), "immutable source\n")
            self.assertFalse((root / "secret").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
