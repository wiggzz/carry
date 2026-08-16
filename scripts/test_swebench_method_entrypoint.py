#!/usr/bin/env python3
"""Behavior tests for the run-scoped method image adapter."""
import os
import pathlib
import subprocess
import tempfile
import unittest


ENTRYPOINT = pathlib.Path(__file__).parents[1] / "containers" / "swebench-method" / "entrypoint.py"


class MethodEntrypointTests(unittest.TestCase):
    def test_codex_logs_in_from_stdin_then_removes_key_from_agent_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo = root / "repo"; prompt_dir = root / "input"; output = root / "output"
            repo.mkdir(); prompt_dir.mkdir(); output.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            (prompt_dir / "task.md").write_text("fix")
            login = root / "codex-login"
            login.write_text(
                "#!/usr/bin/env python3\nimport os,pathlib,sys\n"
                "assert sys.argv[1:] == ['login', '--with-api-key']\n"
                "secret=sys.stdin.read()\nprint(secret)\n"
                "pathlib.Path(os.environ['LOGIN_CAPTURE']).write_text(secret)\n"
            )
            login.chmod(0o755)
            agent = root / "agent.py"
            agent.write_text(
                "import os,pathlib\nassert 'OPENAI_API_KEY' not in os.environ\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            capture = root / "login-secret"
            env = dict(
                os.environ,
                OPENAI_API_KEY="unit-test-secret",
                AGENT_METHOD="codex",
                CODEX_BINARY=str(login),
                LOGIN_CAPTURE=str(capture),
                AGENT_COMMAND=f"python3 {agent} {{prompt_text}}",
                AGENT_TIMEOUT_SECONDS="30",
                BENCHMARK_WORKSPACE=str(repo),
            )
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output)],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(capture.read_text(), "unit-test-secret")
            self.assertNotIn("unit-test-secret", (output / "trace.log").read_text())
            self.assertIn("+after", (output / "final.patch").read_text())

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