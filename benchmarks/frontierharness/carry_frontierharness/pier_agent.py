"""Pier adapter for Carry.

This module is loaded by Pier inside the Runta runtime through --agent-import-path.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import tempfile

from pier.agents.base import BaseAgent
from pier.environments.base import BaseEnvironment
from pier.models.agent.context import AgentContext

from .common import FIREWORKS_RESPONSES_BASE, carry_model_id, read_usage, run_command


_DIAGNOSTIC_LIMIT = 64 * 1024
_SECRET_ASSIGNMENT = re.compile(r"(?im)^(\s*(?:OPENAI_API_KEY|FIREWORKS_API_KEY)\s*=\s*)\S+")
_BEARER = re.compile(r"(?i)(Authorization\s*:\s*Bearer\s+)\S+")


def _safe_output(value: object, secret: str) -> tuple[str, bool]:
    text = str(value or "")
    truncated = len(text.encode("utf-8", errors="replace")) > _DIAGNOSTIC_LIMIT
    if truncated:
        text = text.encode("utf-8", errors="replace")[-_DIAGNOSTIC_LIMIT:].decode(
            "utf-8", errors="replace"
        )
    if secret:
        text = text.replace(secret, "[REDACTED]")
    text = _SECRET_ASSIGNMENT.sub(r"\1[REDACTED]", text)
    return _BEARER.sub(r"\1[REDACTED]", text), truncated


class CarryAgent(BaseAgent):
    """Run the pinned Carry binary inside each Pier task container."""

    @staticmethod
    def name() -> str:
        return "carry"

    def version(self) -> str | None:
        return os.environ.get("CARRY_FRONTIER_VERSION", "pinned-source")

    def _value(self, key: str, default: str | None = None) -> str | None:
        return os.environ.get(key) or default

    async def setup(self, environment: BaseEnvironment) -> None:
        source = Path(self._value("CARRY_FRONTIER_BINARY", "") or "")
        if not source.is_file():
            raise RuntimeError("CARRY_FRONTIER_BINARY is missing or not a regular file")
        await environment.upload_file(source, "/usr/local/bin/carry")
        result = await environment.exec("chmod 755 /usr/local/bin/carry", user="root")
        if result.return_code != 0:
            raise RuntimeError("could not make the Carry binary executable")

    def _write_failure_diagnostic(self, result: object, secret: str) -> None:
        stdout, stdout_truncated = _safe_output(getattr(result, "stdout", ""), secret)
        stderr, stderr_truncated = _safe_output(getattr(result, "stderr", ""), secret)
        payload = {
            "exit_code": getattr(result, "return_code", None),
            "stdout": stdout,
            "stderr": stderr,
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated,
        }
        target = self.logs_dir / "carry-exec-failure.json"
        try:
            with tempfile.NamedTemporaryFile(
                mode="w", encoding="utf-8", dir=self.logs_dir, delete=False
            ) as temporary:
                temporary.write(json.dumps(payload, sort_keys=True) + "\n")
                temporary.flush()
                os.fchmod(temporary.fileno(), 0o600)
                temporary_path = Path(temporary.name)
            os.replace(temporary_path, target)
        except OSError:
            self.logger.warning("could not persist Carry failure diagnostics")

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", delete=False) as prompt:
            prompt.write(instruction)
            prompt_path = Path(prompt.name)
        try:
            await environment.upload_file(prompt_path, "/logs/agent/carry-task.md")
        finally:
            prompt_path.unlink(missing_ok=True)

        cwd_result = await environment.exec("pwd")
        cwd = (cwd_result.stdout or "/").strip() or "/"
        api_key = self._value("FIREWORKS_API_KEY")
        if not api_key:
            raise RuntimeError("FIREWORKS_API_KEY stub is not available to the Carry adapter")
        command = run_command(
            cwd=cwd,
            session_dir="/logs/agent/carry",
            binary="/usr/local/bin/carry",
            model=carry_model_id(self.model_name or ""),
            api_base=self._value("CARRY_FRONTIER_API_BASE", FIREWORKS_RESPONSES_BASE)
            or FIREWORKS_RESPONSES_BASE,
            prompt_path="/logs/agent/carry-task.md",
        )
        result = await environment.exec(
            "mkdir -p /logs/agent/carry; " + command,
            env={"OPENAI_API_KEY": api_key},
        )
        trace = await environment.exec("test -s /logs/agent/carry/trace.jsonl")
        await environment.exec(
            "test -f /logs/agent/carry/trace.jsonl && cp /logs/agent/carry/trace.jsonl "
            "/logs/agent/carry-trace.jsonl || true"
        )
        if result.return_code != 0 and trace.return_code != 0:
            self._write_failure_diagnostic(result, api_key)
            raise RuntimeError("Carry exited without producing a model trace")
        if result.return_code != 0:
            self.logger.warning(
                "Carry exited non-zero after producing evidence; leave task state to verifier",
                extra={"return_code": result.return_code},
            )

    def populate_context_post_run(self, context: AgentContext) -> None:
        usage = read_usage(self.logs_dir / "carry-trace.jsonl")
        context.n_input_tokens = usage.input_tokens
        context.n_cache_tokens = usage.cached_input_tokens
        context.n_output_tokens = usage.output_tokens
