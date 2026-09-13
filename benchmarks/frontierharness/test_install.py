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
                "cat > \"$HOME/.cargo/bin/rustup\" <<'RUSTUP'\n"
                "#!/usr/bin/env bash\n"
                "printf 'rustup %s\\n' \"$*\" >> \"$TEST_LOG\"\n"
                "RUSTUP\n"
                "chmod 0755 \"$HOME/.cargo/bin/rustup\"\n"
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
            apt_get = fake_bin / "apt-get"
            apt_get.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "printf 'apt-get %s\\n' \"$*\" >> \"$TEST_LOG\"\n"
                "if [[ $1 == install ]]; then\n"
                "  if [[ ! -e \"$TEST_FAKE_BIN/apt-install-failed\" ]]; then\n"
                "    : > \"$TEST_FAKE_BIN/apt-install-failed\"\n"
                "    exit 100\n"
                "  fi\n"
                "  if [[ \" $* \" == *\" build-essential \"* ]]; then\n"
                "    cat > \"$TEST_FAKE_BIN/cc\" <<'CC'\n"
                "#!/usr/bin/env bash\n"
                "exit 0\n"
                "CC\n"
                "    chmod 0755 \"$TEST_FAKE_BIN/cc\"\n"
                "  fi\n"
                "  if [[ \" $* \" == *\" musl-tools \"* ]]; then\n"
                "    cat > \"$TEST_FAKE_BIN/musl-gcc\" <<'MUSL'\n"
                "#!/usr/bin/env bash\n"
                "exit 0\n"
                "MUSL\n"
                "    chmod 0755 \"$TEST_FAKE_BIN/musl-gcc\"\n"
                "  fi\n"
                "fi\n"
            )
            apt_get.chmod(0o755)
            # Isolate PATH without relying on whether a runner happens to expose
            # /bin/cc as a symlink.  Supply only the shell utilities the mocked
            # installer needs; notably, do not supply cc until fake apt-get does.
            for command in ("bash", "sh", "mkdir", "cat", "chmod", "sleep"):
                resolved = shutil.which(command)
                if resolved is None:
                    self.fail(f"required shell utility is unavailable: {command}")
                (fake_bin / command).symlink_to(resolved)
            environment = {
                "HOME": str(root / "home"),
                "PATH": str(fake_bin),
                "TEST_LOG": str(log),
                "TEST_FAKE_BIN": str(fake_bin),
            }
            self.assertIsNone(shutil.which("cargo", path=environment["PATH"]))
            self.assertIsNone(shutil.which("cc", path=environment["PATH"]))
            completed = subprocess.run(
                ["/bin/bash", str(INSTALL)],
                cwd=INSTALL.parents[2],
                text=True,
                capture_output=True,
                env=environment,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            commands = log.read_text().splitlines()
            self.assertEqual(
                commands[:4],
                [
                    "apt-get update -qq",
                    "apt-get install -y -qq build-essential",
                    "apt-get update -qq",
                    "apt-get install -y -qq build-essential",
                ],
            )
            self.assertTrue(any(line.startswith("curl ") for line in commands))
            self.assertIn("rustup target add x86_64-unknown-linux-musl", commands)
            self.assertIn("cargo build --locked --release --target x86_64-unknown-linux-musl", commands)
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
