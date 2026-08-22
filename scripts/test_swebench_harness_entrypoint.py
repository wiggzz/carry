#!/usr/bin/env python3
"""Behavior tests for the run-scoped harness image adapter."""
import os
import pathlib
import re
import subprocess
import tempfile
import unittest


ENTRYPOINT = pathlib.Path(__file__).parents[1] / "containers" / "swebench-harness" / "entrypoint.py"


class HarnessEntrypointTests(unittest.TestCase):
    def test_prepared_image_selects_harness_without_image_environment_template(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo = root / "repo"; prompt_dir = root / "input"; output = root / "output"
            binary = root / "bin" / "carry"
            repo.mkdir(); prompt_dir.mkdir(); output.mkdir(); binary.parent.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            (prompt_dir / "task.md").write_text("fix")
            binary.write_text(
                "#!/usr/bin/env python3\nimport pathlib\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            binary.chmod(0o755)
            env = dict(
                os.environ,
                OPENAI_API_KEY="unit-test-secret",
                OPENAI_BASE_URL="http://openai-proxy:8080/v1",
                PREPARED_HARNESS_ROOT=str(root),
                AGENT_TIMEOUT_SECONDS="30",
                BENCHMARK_WORKSPACE=str(repo),
            )
            env.pop("AGENT_COMMAND", None)
            env.pop("AGENT_HARNESS", None)
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--harness", "carry",
                 "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output)],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("+after", (output / "final.patch").read_text())

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
                "config=(pathlib.Path(os.environ['HOME'])/'.codex/config.toml').read_text()\n"
                "assert 'model_provider = \"openai-benchmark\"' in config\n"
                "assert 'base_url = \"http://openai-proxy:8080/v1\"' in config\n"
                "assert 'wire_api = \"responses\"' in config\n"
                "assert 'requires_openai_auth = true' in config\n"
                "assert 'supports_websockets = false' in config\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            capture = root / "login-secret"
            home = root / "home"
            home.mkdir()
            env = dict(
                os.environ,
                HOME=str(home),
                OPENAI_API_KEY="unit-test-secret",
                OPENAI_BASE_URL="http://openai-proxy:8080/v1",
                AGENT_HARNESS="codex",
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
                       OPENAI_BASE_URL="http://openai-proxy:8080/v1",
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

    def test_carry_benchmark_command_relies_on_overall_timeout_without_step_cap(self):
        dockerfile = ENTRYPOINT.with_name("Dockerfile.carry").read_text(encoding="utf-8")
        match = re.search(r'^ENV AGENT_COMMAND="(.*)"$', dockerfile, re.MULTILINE)
        if match is None:
            self.fail("Carry benchmark command is missing")
        command_template = match.group(1)

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo = root / "repo"; prompt_dir = root / "input"; output = root / "output"
            bin_dir = root / "bin"
            repo.mkdir(); prompt_dir.mkdir(); output.mkdir(); bin_dir.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            (prompt_dir / "task.md").write_text("fix")
            fake_carry = bin_dir / "carry"
            fake_carry.write_text(
                "#!/usr/bin/env python3\nimport pathlib,sys\n"
                "assert '--max-steps' not in sys.argv[1:], sys.argv\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            fake_carry.chmod(0o755)
            env = dict(
                os.environ,
                OPENAI_API_KEY="unit-test-secret",
                OPENAI_BASE_URL="http://openai-proxy:8080/v1",
                AGENT_COMMAND=command_template,
                AGENT_TIMEOUT_SECONDS="1",
                BENCHMARK_WORKSPACE=str(repo),
                PATH=f"{bin_dir}:{os.environ['PATH']}",
            )
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output)],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, (output / "trace.log").read_text())
            self.assertIn("+after", (output / "final.patch").read_text())


if __name__ == "__main__":
    unittest.main(verbosity=2)