#!/usr/bin/env python3
"""Exercise the copy-only retained-runtime Regex evidence recovery wrapper."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


RECOVER = Path(__file__).parents[2] / "scripts" / "recover-frontierharness-evidence.sh"
RUNTIME = "fh-9c1ec78442335b8e"
RUN_ID = "recovery-regex-001"


class RecoverRegexEvidenceTests(unittest.TestCase):
    def make_evaluator(self, root: Path, *, complete: bool = True) -> Path:
        evaluator = root / "evaluator"
        runner = evaluator / "skills" / "frontierharness-eval" / "scripts" / "run-trials.sh"
        runner.parent.mkdir(parents=True)
        runner.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, pathlib, sys\n"
            "args = sys.argv[1:]\n"
            "def value(name): return args[args.index(name) + 1]\n"
            "out = pathlib.Path(value('--out'))\n"
            "run_id = value('--run-id')\n"
            "tasks = pathlib.Path(value('--tasks'))\n"
            "trial = out / run_id / 'trials' / 'terminal-bench-regex-log' / 'trial.json'\n"
            "seed = json.loads(trial.read_text())\n"
            "assert tasks.read_text() == 'terminal-bench/regex-log\\n'\n"
            "assert seed['runtime'] == os.environ['EXPECTED_RUNTIME']\n"
            "assert seed['recovery'] is True\n"
            "assert seed['runner_command'].endswith(\"'/work/jobs/terminal-bench-regex-log'\")\n"
            "pathlib.Path(os.environ['FAKE_CALL']).write_text(json.dumps(args))\n"
            f"if {complete!r}:\n"
            "    (trial.parent / 'jobs').mkdir()\n"
            "    (trial.parent / 'jobs' / 'result.json').write_text('{\\\"ok\\\":true}\\n')\n"
            "    (trial.parent / 'completion.json').write_text('{\\\"exit_code\\\":0,\\\"duration_seconds\\\":1}\\n')\n"
            "    (trial.parent / 'runner.log').write_text('recovered\\n')\n"
            "    (trial.parent / 'manifest.json').write_text('{\\\"task\\\":\\\"regex-log\\\"}\\n')\n"
            "    seed.update(status='success', success=True, recovery=False)\n"
            "    trial.write_text(json.dumps(seed) + '\\n')\n"
        )
        runner.chmod(0o755)
        return evaluator

    def invoke(self, root: Path, evaluator: Path, runtime: str = RUNTIME, task: str = "terminal-bench/regex-log") -> subprocess.CompletedProcess[str]:
        source = root / "source"
        manifest = source / "benchmarks" / "frontierharness" / "tasks-30.txt"
        manifest.parent.mkdir(parents=True)
        manifest.write_text("terminal-bench/regex-log\nterminal-bench/chess-best-move\n")
        environment = {
            key: value
            for key, value in os.environ.items()
            if key not in {"FIREWORKS_API_KEY", "RUNTA_TOKEN"}
        }
        environment.update(
            EXPECTED_RUNTIME=RUNTIME,
            FAKE_CALL=str(root / "call.json"),
            RUNTA_TOKEN="test-runta-token",
        )
        return subprocess.run(
            [
                "bash", str(RECOVER), "--source", str(source), "--evaluator", str(evaluator), "--task", task, "--runtime", runtime,
                "--checkpoint", "carry-fh-fbafa2ad28bf", "--run-id", RUN_ID, "--out", str(root / "out"),
            ],
            text=True,
            capture_output=True,
            env=environment,
        )

    def test_recovers_existing_runtime_without_a_provider_key(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            completed = self.invoke(root, self.make_evaluator(root))
            self.assertEqual(completed.returncode, 0, completed.stderr)
            call = json.loads((root / "call.json").read_text())
            self.assertEqual(call[call.index("--checkpoint") + 1], "carry-fh-fbafa2ad28bf")
            self.assertEqual(call[call.index("--provider") + 1], "fireworks")
            self.assertEqual(call[call.index("--timeout") + 1], "5400")
            recovery = json.loads((root / "out" / "recovery.json").read_text())
            self.assertEqual(recovery["operation"], "existing-runtime-evidence-collection")
            self.assertFalse(recovery["model_execution"])
            self.assertEqual(recovery["runtime"], RUNTIME)

    def test_rejects_an_unsafe_runtime_before_invoking_evaluator(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            completed = self.invoke(root, self.make_evaluator(root), runtime="fh-unsafe;command")
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("unsafe --runtime", completed.stderr)
            self.assertFalse((root / "call.json").exists())

    def test_rejects_a_task_outside_the_frozen_manifest_before_invoking_evaluator(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            completed = self.invoke(root, self.make_evaluator(root), task="terminal-bench/not-in-manifest")
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("is not in the frozen task manifest", completed.stderr)
            self.assertFalse((root / "call.json").exists())

    def test_fails_if_the_upstream_resumption_does_not_produce_complete_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            completed = self.invoke(root, self.make_evaluator(root, complete=False))
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("did not produce complete evidence", completed.stderr)


if __name__ == "__main__":
    unittest.main()
