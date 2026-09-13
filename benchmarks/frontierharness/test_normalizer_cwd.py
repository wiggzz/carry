#!/usr/bin/env python3
"""Exercise the entrypoint's normalizer from the evaluator checkout."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ENTRYPOINT = Path(__file__).parents[2] / "scripts" / "run-frontierharness-ci.sh"


class NormalizerWorkingDirectoryTests(unittest.TestCase):
    def test_smoke_normalizer_runs_with_the_evaluator_as_its_working_directory(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "source"
            helper = source / "benchmarks" / "frontierharness" / "patch_usage_details.py"
            helper.parent.mkdir(parents=True)
            helper.write_text("raise SystemExit(0)\n")
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(source), "config", "user.name", "Test"], check=True)
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)
            subprocess.run(["git", "-C", str(source), "commit", "-qm", "fixture"], check=True)
            commit = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()

            evaluator = root / "evaluator"
            scripts = evaluator / "skills" / "frontierharness-eval" / "scripts"
            scripts.mkdir(parents=True)
            (evaluator / "benchmark.json").write_text("{}\n")
            run_trials = scripts / "run-trials.sh"
            run_trials.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "while [[ $# -gt 0 ]]; do\n"
                "  case \"$1\" in --out) out=$2; shift 2 ;; --run-id) run_id=$2; shift 2 ;; *) shift ;; esac\n"
                "done\n"
                "mkdir -p \"$out/$run_id\"\n"
                "printf '{}\\n' > \"$out/$run_id/run.json\"\n"
            )
            run_trials.chmod(0o755)
            (scripts / "normalize-results.mjs").write_text("// Fake node command is used by this test.\n")

            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake_git = fake_bin / "git"
            fake_git.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "if [[ $1 == clone ]]; then\n"
                "  cp -R \"$EVALUATOR_FIXTURE\" \"${@: -1}\"\n"
                "  exit 0\n"
                "fi\n"
                "if [[ $1 == -C && $3 == checkout ]]; then exit 0; fi\n"
                "exec /usr/bin/git \"$@\"\n"
            )
            fake_git.chmod(0o755)
            fake_runta = fake_bin / "runta"
            fake_runta.write_text(
                "#!/usr/bin/env bash\n"
                "if [[ $1 == checkpoint && $2 == ls && ${3:-} == --json ]]; then\n"
                "  printf '{\\\"checkpoints\\\":[{\\\"display_name\\\":\\\"test-checkpoint\\\",\\\"state\\\":\\\"ready\\\"}]}'\n"
                "fi\n"
            )
            fake_runta.chmod(0o755)
            fake_jq = fake_bin / "jq"
            fake_jq.write_text("#!/usr/bin/env bash\ncat >/dev/null || true\n")
            fake_jq.chmod(0o755)
            node_cwd = root / "node-cwd"
            fake_node = fake_bin / "node"
            fake_node.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "test -f benchmark.json\n"
                "printf '%s\\n' \"$PWD\" > \"$NODE_CWD_FILE\"\n"
            )
            fake_node.chmod(0o755)
            tasks = root / "tasks.txt"
            tasks.write_text("terminal-bench/regex-log\n")
            environment = dict(os.environ)
            environment.update(
                {
                    "PATH": f"{fake_bin}:{environment['PATH']}",
                    "EVALUATOR_FIXTURE": str(evaluator),
                    "NODE_CWD_FILE": str(node_cwd),
                    "RUNTA_TOKEN": "test-runta-token",
                    "FIREWORKS_API_KEY": "test-fireworks-key",
                }
            )
            out = root / "out"
            completed = subprocess.run(
                [
                    "bash", str(ENTRYPOINT), "--mode", "smoke-2", "--source", str(source),
                    "--commit", commit, "--repo", "https://example.invalid/carry.git",
                    "--checkpoint", "test-checkpoint", "--run-id", "test-run", "--out", str(out),
                    "--tasks", str(tasks),
                ],
                cwd=root,
                text=True,
                capture_output=True,
                env=environment,
            )
            self.assertEqual(completed.returncode, 0, f"stdout={completed.stdout}\nstderr={completed.stderr}")
            self.assertEqual(node_cwd.read_text().strip(), str(out / "frontierharness-eval"))


if __name__ == "__main__":
    unittest.main()
