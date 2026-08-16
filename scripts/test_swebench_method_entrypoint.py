#!/usr/bin/env python3
"""Behavior tests for the run-scoped method image adapter."""
import os
import pathlib
import subprocess
import tempfile
import unittest


ENTRYPOINT = pathlib.Path(__file__).parents[1] / "containers" / "swebench-method" / "entrypoint.py"


class MethodEntrypointTests(unittest.TestCase):
    def test_reads_prompt_as_one_argument_and_redacts_secret_from_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo = root / "repo"
            prompt_dir = root / "input"
            output = root / "output"
            repo.mkdir(); prompt_dir.mkdir(); output.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            prompt = "fix the thing with spaces"
            (prompt_dir / "task.md").write_text(prompt)
            helper = root / "agent.py"
            helper.write_text(
                "import os,pathlib,sys\n"
                "assert sys.argv[1] == 'fix the thing with spaces'\n"
                "print(os.environ['OPENAI_API_KEY'])\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
                "pathlib.Path('new.txt').write_text('new\\n')\n"
            )
            env = dict(os.environ, OPENAI_API_KEY="unit-test-secret",
                       AGENT_COMMAND=f"python3 {helper} {{prompt_text}}", AGENT_TIMEOUT_SECONDS="30",
                       BENCHMARK_WORKSPACE=str(repo))
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output)],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("[REDACTED]", (output / "trace.log").read_text())
            self.assertNotIn("unit-test-secret", (output / "trace.log").read_text())
            self.assertIn("+after", (output / "final.patch").read_text())
            self.assertIn("new.txt", (output / "final.patch").read_text())


if __name__ == "__main__":
    unittest.main(verbosity=2)