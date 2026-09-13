#!/usr/bin/env python3
"""Exercise the real suite launcher with fake Harbor and Pier CLIs."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


RUNNER = Path(__file__).with_name("run-suite.sh")


class RunSuiteTests(unittest.TestCase):
    def invoke(self, suite: str) -> list[str]:
        with tempfile.TemporaryDirectory() as raw:
            fake_bin = Path(raw)
            for name in ("harbor", "pier"):
                command = fake_bin / name
                command.write_text(
                    "#!/usr/bin/env bash\n"
                    "printf 'binary=%s\\n' \"$0\"\n"
                    "printf 'binary_env=%s\\n' \"$CARRY_FRONTIER_BINARY\"\n"
                    "printf 'key_present=%s\\n' \"${FIREWORKS_API_KEY:+yes}\"\n"
                    "printf 'arg=%s\\n' \"$@\"\n"
                )
                command.chmod(0o755)
            environment = dict(os.environ)
            environment.update(
                {
                    "PATH": f"{fake_bin}:{environment['PATH']}",
                    "FIREWORKS_API_KEY": "runta-secret-stub",
                }
            )
            completed = subprocess.run(
                [str(RUNNER), "regex-log", suite, "fireworks_ai/accounts/fireworks/models/kimi-k3", "/jobs"],
                check=True,
                text=True,
                capture_output=True,
                env=environment,
            )
            return completed.stdout.splitlines()

    def test_terminal_bench_uses_harbor_with_the_custom_agent(self) -> None:
        lines = self.invoke("terminal-bench")
        text = "\n".join(lines)
        self.assertIn("binary_env=/work/harness/target/release/carry", text)
        self.assertIn("key_present=yes", text)
        self.assertIn("arg=-a\narg=carry_frontierharness.harbor_agent:CarryAgent", text)
        self.assertIn("arg=-d\narg=terminal-bench@2.0\narg=-i\narg=regex-log", text)
        self.assertNotIn("runta-secret-stub", text)

    def test_datacurve_uses_pier_with_the_custom_agent(self) -> None:
        lines = self.invoke("datacurve")
        text = "\n".join(lines)
        self.assertIn("binary_env=/work/harness/target/release/carry", text)
        self.assertIn("key_present=yes", text)
        self.assertIn("arg=--agent-import-path\narg=carry_frontierharness.pier_agent:CarryAgent", text)
        self.assertIn("arg=-p\narg=/work/deep-swe/tasks/regex-log", text)
        self.assertNotIn("runta-secret-stub", text)


if __name__ == "__main__":
    unittest.main()
