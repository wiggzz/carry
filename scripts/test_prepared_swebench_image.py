#!/usr/bin/env python3
"""Docker integration test for the dependency-ready SWE-bench task image."""
from __future__ import annotations

import importlib.util
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).parents[1]
SCRIPT = ROOT / "scripts" / "swebench_smoke.py"
SPEC = importlib.util.spec_from_file_location("swebench_smoke_prepared_test", SCRIPT)
assert SPEC and SPEC.loader
WORKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKER)


@unittest.skipUnless(shutil.which("docker"), "Docker is required for the prepared-image test")
class PreparedSWEbenchImageTests(unittest.TestCase):
    def test_prepared_image_hides_parent_checkout_and_runs_adapter_with_dependency_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fixture = root / "fixture"
            fixture.mkdir()
            (fixture / "entrypoint.py").write_bytes(
                (ROOT / "containers" / "swebench-harness" / "entrypoint.py").read_bytes()
            )
            (fixture / "conda.sh").write_text(
                "conda() {\n"
                "  if [ \"${1:-}\" = activate ]; then\n"
                "    export CONDA_PREFIX=/opt/miniconda3/envs/testbed\n"
                "    export PATH=\"$CONDA_PREFIX/bin:$PATH\"\n"
                "  fi\n"
                "}\n",
                encoding="utf-8",
            )
            (fixture / "dependency-proof").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            (fixture / "carry").write_text(
                "#!/bin/sh\ndependency-proof\ntest -f compiled-proof.so\nprintf 'after\\n' > file.txt\n",
                encoding="utf-8",
            )
            (fixture / "node").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            (fixture / "cli").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            for path in ("dependency-proof", "carry", "node", "cli"):
                (fixture / path).chmod(0o755)

            (fixture / "Dockerfile.task").write_text(
                "FROM python:3.11-slim\n"
                "RUN apt-get update && apt-get install -y --no-install-recommends git \\\n"
                " && rm -rf /var/lib/apt/lists/*\n"
                "RUN ln -sf /usr/local/bin/python3 /usr/bin/python3 \\\n"
                " && mkdir -p /opt/miniconda3/bin /opt/miniconda3/envs/testbed/bin /testbed \\\n"
                " && printf future > /testbed/future-git-object \\\n"
                " && printf setup > /root/setup_env.sh \\\n"
                " && printf setup > /root/setup_repo.sh\n"
                "RUN cd /testbed && git init -q \\\n"
                " && git config user.email test@example.com && git config user.name Test \\\n"
                " && printf '*.so\\n' > .gitignore && printf tracked > tracked.txt \\\n"
                " && git add .gitignore tracked.txt && git commit -qm base \\\n"
                " && printf compiled > compiled-proof.so\n"
                "COPY conda.sh /opt/miniconda3/bin/activate\n"
                "COPY dependency-proof /opt/miniconda3/envs/testbed/bin/dependency-proof\n",
                encoding="utf-8",
            )
            (fixture / "Dockerfile.carry").write_text(
                "ARG BASE\nFROM ${BASE}\n"
                "RUN mkdir -p /opt/swebench-harness/bin\n"
                "COPY carry /opt/swebench-harness/bin/carry\n"
                "COPY entrypoint.py /opt/swebench-harness/bin/adapter\n"
                "RUN chmod 0555 /opt/swebench-harness/bin/carry /opt/swebench-harness/bin/adapter\n",
                encoding="utf-8",
            )
            (fixture / "Dockerfile.node").write_text(
                "ARG BASE\nARG BIN\nFROM ${BASE}\n"
                "RUN mkdir -p /opt/swebench-harness/bin /opt/swebench-harness/lib/node_modules/fake\n"
                "COPY node /opt/swebench-harness/bin/node\n"
                "COPY cli /opt/swebench-harness/lib/node_modules/fake/cli\n"
                "COPY entrypoint.py /opt/swebench-harness/bin/adapter\n"
                "RUN chmod 0555 /opt/swebench-harness/bin/node /opt/swebench-harness/bin/adapter "
                "/opt/swebench-harness/lib/node_modules/fake/cli \\\n"
                " && ln -s ../lib/node_modules/fake/cli /opt/swebench-harness/bin/${BIN}\n",
                encoding="utf-8",
            )

            def build(tag: str, dockerfile: str, *args: str) -> str:
                command = ["docker", "build", "--quiet", "--tag", tag, "--file", str(fixture / dockerfile)]
                for value in args:
                    command.extend(("--build-arg", value))
                command.append(str(fixture))
                try:
                    subprocess.run(command, check=True, text=True, capture_output=True)
                except subprocess.CalledProcessError as error:
                    self.fail(f"docker build failed: {error.stdout}\n{error.stderr}")
                return subprocess.run(
                    ["docker", "image", "inspect", "--format", "{{.Id}}", tag],
                    check=True, text=True, capture_output=True,
                ).stdout.strip()

            prefix = f"prepared-fixture-{os.getpid()}"
            task_tag = f"{prefix}-task"
            task_id = build(task_tag, "Dockerfile.task")
            carry_id = build(f"{prefix}-carry", "Dockerfile.carry", f"BASE={task_tag}")
            codex_id = build(
                f"{prefix}-codex", "Dockerfile.node", f"BASE={task_tag}", "BIN=codex"
            )
            pi_id = build(f"{prefix}-pi", "Dockerfile.node", f"BASE={task_tag}", "BIN=pi")
            prepared = {
                harness: WORKER.build_prepared_task_image(
                    source=ROOT,
                    run_id=prefix,
                    instance_id="fixture__task-1",
                    harness=harness,
                    task_image_id=task_id,
                    harness_image_id=image_id,
                )
                for harness, image_id in {
                    "carry": carry_id, "codex": codex_id, "pi": pi_id,
                }.items()
            }
            subprocess.run(
                [
                    "docker", "run", "--rm", "--network", "none",
                    "--entrypoint", "/bin/bash", prepared["carry"]["tag"], "-lc",
                    "test ! -e /testbed/future-git-object "
                    "&& test ! -e /root/setup_env.sh && test ! -e /root/setup_repo.sh "
                    "&& test -x /opt/swebench-harness/bin/carry "
                    "&& test ! -e /opt/swebench-harness/bin/codex "
                    "&& test ! -e /opt/swebench-harness/bin/pi",
                ],
                check=True,
            )

            repo = root / "repo"; prompt = root / "input"; output = root / "output"
            repo.mkdir(); prompt.mkdir(); output.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "file.txt").write_text("before\n", encoding="utf-8")
            (repo / ".gitignore").write_text("*.so\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(repo), "add", "file.txt", ".gitignore"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
            (prompt / "task.md").write_text("fix", encoding="utf-8")
            subprocess.run(
                [
                    "docker", "run", "--rm", "--network", "none", "--read-only",
                    "--cap-drop=ALL", "--security-opt", "no-new-privileges",
                    "--env", "OPENAI_API_KEY=test-only",
                    "--env", "OPENAI_BASE_URL=http://unused.invalid/v1",
                    "--env", "HOME=/agent-home", "--tmpfs", "/agent-home:rw,nosuid,nodev",
                    "--tmpfs", "/tmp:rw,nosuid,nodev",
                    "--mount", f"type=bind,src={repo},dst=/testbed",
                    "--mount", f"type=bind,src={prompt},dst=/benchmark/input,readonly",
                    "--mount", f"type=bind,src={output},dst=/benchmark/output",
                    "--workdir", "/testbed", prepared["carry"]["tag"], "run", "--harness", "carry",
                    "--model", "fixture", "--reasoning", "medium",
                    "--prompt", "/benchmark/input/task.md", "--output", "/benchmark/output",
                ],
                check=True,
            )
            patch = (output / "final.patch").read_text(encoding="utf-8")
            self.assertIn("+after", patch)
            self.assertNotIn("compiled-proof", patch)
            self.assertTrue((repo / "compiled-proof.so").is_file())
            self.assertFalse((repo / "future-git-object").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
