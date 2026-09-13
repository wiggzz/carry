#!/usr/bin/env python3
"""Regression coverage for retrying transient base-tool apt mirror failures."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import os
import subprocess
import tempfile
import unittest


PATCHER = Path(__file__).with_name("patch_provision_base_tools.py")
spec = importlib.util.spec_from_file_location("patch_provision_base_tools", PATCHER)
assert spec and spec.loader
patcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(patcher)


class ProvisionBaseToolsPatchTests(unittest.TestCase):
    def test_retries_a_transient_apt_install_failure(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            fake = root / "bin"
            fake.mkdir()
            log = root / "apt.log"
            (fake / "apt-get").write_text(
                "#!/usr/bin/env bash\n"
                "set -eu\n"
                "printf '%s\\n' \"$*\" >> \"$APT_LOG\"\n"
                "if [ \"$1\" = install ]; then\n"
                "  count=$(grep -c '^install ' \"$APT_LOG\" || true)\n"
                "  [ \"$count\" -ge 3 ] || exit 100\n"
                "fi\n"
            )
            (fake / "apt-get").chmod(0o755)
            (fake / "sleep").write_text("#!/usr/bin/env bash\nexit 0\n")
            (fake / "sleep").chmod(0o755)
            script = root / "base-tools.sh"
            script.write_text("#!/usr/bin/env bash\nset -eu\n" + patcher.NEW)
            script.chmod(0o755)
            environment = {**os.environ, "PATH": f"{fake}:{os.environ['PATH']}", "APT_LOG": str(log)}
            completed = subprocess.run(["bash", str(script)], text=True, capture_output=True, env=environment)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(sum(line.startswith("install ") for line in log.read_text().splitlines()), 3)

    def test_applies_once_and_rejects_unknown_layout(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            target = Path(raw) / "provision.sh"
            target.write_text("before\n" + patcher.OLD + "after\n")
            patcher.apply(target)
            once = target.read_text()
            patcher.apply(target)
            self.assertEqual(target.read_text(), once)
            target.write_text("unexpected\n")
            with self.assertRaisesRegex(RuntimeError, "base-tool provisioning layout"):
                patcher.apply(target)


if __name__ == "__main__":
    unittest.main()
