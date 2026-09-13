#!/usr/bin/env python3
"""Exercise the clean-runtime installation contract for FrontierHarness."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


INSTALL = Path(__file__).with_name("install.sh")


class FrontierHarnessInstallTests(unittest.TestCase):
    def test_clean_runtime_bootstraps_rust_before_building_carry(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            log = root / "commands.log"
            installer = fake_bin / "curl"
            installer.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "printf 'curl %s\\n' \"$*\" >> \"$TEST_LOG\"\n"
                "cat <<'INSTALLER'\n"
                "mkdir -p \"$HOME/.cargo/bin\"\n"
                "cat > \"$HOME/.cargo/bin/cargo\" <<'CARGO'\n"
                "#!/usr/bin/env bash\n"
                "printf 'cargo %s\\n' \"$*\" >> \"$TEST_LOG\"\n"
                "CARGO\n"
                "chmod 0755 \"$HOME/.cargo/bin/cargo\"\n"
                "INSTALLER\n"
            )
            installer.chmod(0o755)
            python = fake_bin / "python3"
            python.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "printf 'python3 %s\\n' \"$*\" >> \"$TEST_LOG\"\n"
            )
            python.chmod(0o755)
            environment = {
                "HOME": str(root / "home"),
                "PATH": f"{fake_bin}:/usr/bin:/bin",
                "TEST_LOG": str(log),
            }
            self.assertIsNone(shutil.which("cargo", path=environment["PATH"]))
            completed = subprocess.run(
                ["bash", str(INSTALL)],
                cwd=INSTALL.parents[2],
                text=True,
                capture_output=True,
                env=environment,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            commands = log.read_text().splitlines()
            self.assertTrue(any(line.startswith("curl ") for line in commands))
            self.assertIn("cargo build --locked --release", commands)
            self.assertEqual(
                commands[-3:],
                [
                    "python3 benchmarks/frontierharness/test_adapter.py",
                    "python3 benchmarks/frontierharness/test_agents.py",
                    "python3 benchmarks/frontierharness/test_run_suite.py",
                ],
            )


if __name__ == "__main__":
    unittest.main()
