#!/usr/bin/env python3
"""Behavior tests for the run-scoped harness image adapter."""
import os
import pathlib
import subprocess
import tempfile
import unittest


ENTRYPOINT = pathlib.Path(__file__).parents[1] / "containers" / "swebench-harness" / "entrypoint.py"


class HarnessEntrypointTests(unittest.TestCase):
    def test_carry_resume_session_is_forwarded_only_to_the_carry_command(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo, prompt_dir, output = root / "repo", root / "input", root / "output"
            binary, session = root / "bin" / "carry", root / "session"
            repo.mkdir(); prompt_dir.mkdir(); output.mkdir(); binary.parent.mkdir(parents=True); session.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            (prompt_dir / "task.md").write_text("fix")
            binary.write_text(
                "#!/usr/bin/env python3\nimport pathlib,sys\n"
                "index=sys.argv.index('--resume')\n"
                "assert sys.argv[index + 1] == '/benchmark/session', sys.argv\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, OPENAI_API_KEY="unit-test-secret",
                       OPENAI_BASE_URL="http://openai-proxy:8080/v1",
                       PREPARED_HARNESS_ROOT=str(root), AGENT_TIMEOUT_SECONDS="30",
                       BENCHMARK_WORKSPACE=str(repo))
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--harness", "carry",
                 "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output),
                 "--resume-session", "/benchmark/session"],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("+after", (output / "final.patch").read_text())

    def test_carry_forwards_compaction_policy_and_keep_lease_to_native_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo, prompt_dir, output = root / "repo", root / "input", root / "output"
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
                "#!/usr/bin/env python3\nimport pathlib,sys\n"
                "index=sys.argv.index('--compaction-policy')\n"
                "assert sys.argv[index + 1] == 'disabled', sys.argv\n"
                "lease=sys.argv.index('--keep-lease-turns')\n"
                "assert sys.argv[lease + 1] == '8', sys.argv\n"
                "payoff=sys.argv.index('--compaction-payoff-requests')\n"
                "assert sys.argv[payoff + 1] == '5', sys.argv\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, OPENAI_API_KEY="unit-test-secret",
                       OPENAI_BASE_URL="http://openai-proxy:8080/v1",
                       PREPARED_HARNESS_ROOT=str(root), AGENT_TIMEOUT_SECONDS="30",
                       BENCHMARK_WORKSPACE=str(repo), CARRY_COMPACTION_POLICY="disabled",
                       CARRY_KEEP_LEASE_TURNS="8", CARRY_COMPACTION_PAYOFF_REQUESTS="5")
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--harness", "carry",
                 "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output)],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("+after", (output / "final.patch").read_text())

    def test_codex_resume_uses_a_durable_codex_home_and_native_thread_id(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo, prompt_dir, output, session = (
                root / "repo", root / "input", root / "output", root / "codex-session"
            )
            binary = root / "bin" / "codex"
            repo.mkdir(); prompt_dir.mkdir(); output.mkdir(); session.mkdir(); binary.parent.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            (prompt_dir / "task.md").write_text("fix")
            thread = "11111111-1111-4111-8111-111111111111"
            binary.write_text(
                "#!/usr/bin/env python3\nimport os,pathlib,sys\n"
                "if sys.argv[1] == 'login': sys.exit(0)\n"
                "assert os.environ['CODEX_HOME'] == " + repr(str(session)) + ", (os.environ, sys.argv)\n"
                "assert sys.argv[1:3] == ['exec', 'resume'], sys.argv\n"
                "assert '" + thread + "' in sys.argv, sys.argv\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, OPENAI_API_KEY="unit-test-secret",
                       OPENAI_BASE_URL="http://openai-proxy:8080/v1",
                       PREPARED_HARNESS_ROOT=str(root), AGENT_TIMEOUT_SECONDS="30",
                       BENCHMARK_WORKSPACE=str(repo))
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--harness", "codex",
                 "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output),
                 "--codex-session", str(session), "--codex-thread", thread],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("+after", (output / "final.patch").read_text())

    def test_codex_initial_task_uses_native_session_home_without_resume(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo, prompt_dir, output, session = (root / "repo", root / "input", root / "output", root / "codex-session")
            binary = root / "bin" / "codex"
            repo.mkdir(); prompt_dir.mkdir(); output.mkdir(); session.mkdir(); binary.parent.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            (prompt_dir / "task.md").write_text("fix")
            binary.write_text(
                "#!/usr/bin/env python3\nimport os,pathlib,sys\n"
                "if sys.argv[1] == 'login': sys.exit(0)\n"
                "assert os.environ['CODEX_HOME'] == " + repr(str(session)) + "\n"
                "assert sys.argv[1] == 'exec' and 'resume' not in sys.argv, sys.argv\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, OPENAI_API_KEY="unit-test-secret", OPENAI_BASE_URL="http://openai-proxy:8080/v1",
                       PREPARED_HARNESS_ROOT=str(root), AGENT_TIMEOUT_SECONDS="30", BENCHMARK_WORKSPACE=str(repo))
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--harness", "codex", "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output), "--codex-session", str(session)],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("+after", (output / "final.patch").read_text())

    def test_pi_native_session_does_not_include_no_session(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            repo, prompt_dir, output, session = (root / "repo", root / "input", root / "output", root / "pi-session")
            binary = root / "bin" / "pi"
            repo.mkdir(); prompt_dir.mkdir(); output.mkdir(); session.mkdir(); binary.parent.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            (prompt_dir / "task.md").write_text("fix")
            binary.write_text(
                "#!/usr/bin/env python3\nimport pathlib,sys\n"
                "assert '--no-session' not in sys.argv, sys.argv\n"
                "assert '--session' in sys.argv and '--session-dir' in sys.argv, sys.argv\n"
                "pathlib.Path(sys.argv[sys.argv.index('--session')+1]).write_text('{}\\n')\n"
                "pathlib.Path('file.txt').write_text('after\\n')\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, OPENAI_API_KEY="unit-test-secret", OPENAI_BASE_URL="http://openai-proxy:8080/v1",
                       PREPARED_HARNESS_ROOT=str(root), AGENT_TIMEOUT_SECONDS="30", BENCHMARK_WORKSPACE=str(repo))
            run = subprocess.run(
                ["python3", str(ENTRYPOINT), "run", "--harness", "pi", "--model", "model", "--reasoning", "medium",
                 "--prompt", str(prompt_dir / "task.md"), "--output", str(output), "--pi-session-dir", str(session)],
                cwd=repo, env=env, text=True, capture_output=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertTrue((session / "session.jsonl").is_file())

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


if __name__ == "__main__":
    unittest.main(verbosity=2)