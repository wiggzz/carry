#!/usr/bin/env python3
"""Exercise copying a verified provisioning manifest without probing a frozen runtime."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).parents[2] / "scripts" / "frontierharness_provision_artifacts.sh"


class ProvisionArtifactTests(unittest.TestCase):
    def invoke(self, source: Path, destination: Path, commit: str, checkpoint: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; copy_verified_provision_manifest "$2" "$3" "$4" "$5"',
                "bash",
                str(HELPER),
                str(source),
                str(destination),
                commit,
                checkpoint,
            ],
            text=True,
            capture_output=True,
        )

    def test_copies_matching_local_manifest_without_any_runtime_client(self) -> None:
        commit = "a" * 40
        checkpoint = "carry-fh-test"
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "manifest.json"
            destination = root / "out" / "manifest.json"
            source.write_text(json.dumps({"harness_commit": commit, "checkpoint": checkpoint}) + "\n")
            completed = self.invoke(source, destination, commit, checkpoint)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(json.loads(destination.read_text()), json.loads(source.read_text()))

    def test_rejects_wrong_checkpoint_before_writing_output(self) -> None:
        commit = "b" * 40
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "manifest.json"
            destination = root / "out" / "manifest.json"
            source.write_text(json.dumps({"harness_commit": commit, "checkpoint": "wrong"}) + "\n")
            completed = self.invoke(source, destination, commit, "carry-fh-test")
            self.assertNotEqual(completed.returncode, 0)
            self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main()
