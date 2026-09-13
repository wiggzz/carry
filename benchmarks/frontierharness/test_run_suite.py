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
    def invoke(self, suite: str, *, runtime_stub_present: bool = True) -> list[str]:
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
            environment["PATH"] = f"{fake_bin}:{environment['PATH']}"
            if runtime_stub_present:
                environment["FIREWORKS_API_KEY"] = "runta-secret-stub"
            else:
                environment.pop("FIREWORKS_API_KEY", None)
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
        self.assertIn("binary_env=/work/harness/target/x86_64-unknown-linux-musl/release/carry", text)
        self.assertIn("key_present=yes", text)
        self.assertIn("arg=-a\narg=carry_frontierharness.harbor_agent:CarryAgent", text)
        self.assertIn("arg=-d\narg=terminal-bench@2.0\narg=-i\narg=regex-log", text)
        self.assertNotIn("runta-secret-stub", text)

    def test_launcher_supplies_a_nonsecret_stub_when_runtime_does_not(self) -> None:
        lines = self.invoke("terminal-bench", runtime_stub_present=False)
        self.assertIn("key_present=yes", "\n".join(lines))

    def test_datacurve_uses_pier_with_the_custom_agent(self) -> None:
        lines = self.invoke("datacurve")
        text = "\n".join(lines)
        self.assertIn("binary_env=/work/harness/target/x86_64-unknown-linux-musl/release/carry", text)
        self.assertIn("key_present=yes", text)
        self.assertIn("arg=--agent-import-path\narg=carry_frontierharness.pier_agent:CarryAgent", text)
        self.assertIn("arg=--environment-import-path\narg=carry_frontierharness.pier_environment:RuntaDockerEnvironment", text)
        self.assertIn("arg=--environment-kwarg\narg=runta_compose_file=/work/runta-ca-overlay.yaml", text)
        self.assertIn("arg=-p\narg=/work/deep-swe/tasks/regex-log", text)
        self.assertNotIn("runta-secret-stub", text)


if __name__ == "__main__":
    unittest.main()
