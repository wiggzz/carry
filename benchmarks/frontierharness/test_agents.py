#!/usr/bin/env python3
"""Exercise Carry's Harbor/Pier adapters without a live benchmark runtime."""

from __future__ import annotations

import asyncio
import importlib
import json
import logging
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest

ROOT = Path(__file__).parent
sys.path.insert(0, str(ROOT))


class _FakeBaseAgent:
    def __init__(self, logs_dir: Path, model_name: str | None = None, **_: object) -> None:
        self.logs_dir = logs_dir
        self.model_name = model_name
        self.logger = logging.getLogger("fake-agent")

    def _get_env(self, key: str, *_: str) -> str | None:
        return os.environ.get(key)

    @classmethod
    def import_path(cls) -> str:
        return f"{cls.__module__}:{cls.__name__}"


class _FakeContext:
    n_input_tokens = 0
    n_cache_tokens = 0
    n_output_tokens = 0


class _Result:
    def __init__(self, return_code: int = 0, stdout: str = "") -> None:
        self.return_code = return_code
        self.stdout = stdout


class _Environment:
    def __init__(self, *, carry_return_code: int = 0, trace_exists: bool = True) -> None:
        self.carry_return_code = carry_return_code
        self.trace_exists = trace_exists
        self.uploads: list[tuple[Path, str]] = []
        self.commands: list[tuple[str, dict[str, str] | None]] = []

    async def upload_file(self, local: Path, remote: str) -> None:
        self.uploads.append((Path(local), remote))

    async def exec(self, command: str, **kwargs: object) -> _Result:
        env = kwargs.get("env")
        self.commands.append((command, env if isinstance(env, dict) else None))
        if command == "pwd":
            return _Result(stdout="/workspace\n")
        if command == "test -s /logs/agent/carry/trace.jsonl":
            return _Result(0 if self.trace_exists else 1)
        if "--session-dir /logs/agent/carry" in command:
            return _Result(self.carry_return_code)
        return _Result()


def _module(name: str, **members: object) -> None:
    module = types.ModuleType(name)
    for key, value in members.items():
        setattr(module, key, value)
    sys.modules[name] = module


def _install_runner_stubs() -> None:
    for root in ("harbor", "pier"):
        _module(root)
        _module(f"{root}.agents")
        _module(f"{root}.agents.base", BaseAgent=_FakeBaseAgent)
        _module(f"{root}.environments")
        _module(f"{root}.environments.base", BaseEnvironment=_Environment)
        _module(f"{root}.models")
        _module(f"{root}.models.agent")
        _module(f"{root}.models.agent.context", AgentContext=_FakeContext)


class CarryAgentContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        _install_runner_stubs()
        cls.harbor = importlib.import_module("carry_frontierharness.harbor_agent").CarryAgent
        cls.pier = importlib.import_module("carry_frontierharness.pier_agent").CarryAgent

    def exercise(self, agent_type: type[_FakeBaseAgent]) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            binary = root / "carry"
            binary.write_bytes(b"binary")
            logs = root / "logs"
            logs.mkdir()
            (logs / "carry-trace.jsonl").write_text(
                json.dumps(
                    {
                        "event": "model_response",
                        "data": {"usage": {"input_tokens": 8, "cached_input_tokens": 3, "output_tokens": 5}},
                    }
                )
                + "\n"
            )
            old = dict(os.environ)
            os.environ.update(
                {
                    "CARRY_FRONTIER_BINARY": str(binary),
                    "FIREWORKS_API_KEY": "runta-secret-stub",
                }
            )
            try:
                agent = agent_type(logs, model_name="fireworks_ai/accounts/fireworks/models/kimi-k3")
                environment = _Environment()
                context = _FakeContext()
                asyncio.run(agent.setup(environment))
                asyncio.run(agent.run("solve the task", environment, context))
                agent.populate_context_post_run(context)
            finally:
                os.environ.clear()
                os.environ.update(old)

        self.assertEqual(environment.uploads[0][1], "/usr/local/bin/carry")
        self.assertEqual(environment.uploads[1][1], "/logs/agent/carry-task.md")
        carry_calls = [item for item in environment.commands if "--session-dir /logs/agent/carry" in item[0]]
        self.assertEqual(len(carry_calls), 1)
        command, env = carry_calls[0]
        self.assertIn("accounts/fireworks/models/kimi-k3", command)
        self.assertNotIn("runta-secret-stub", command)
        self.assertEqual(env, {"OPENAI_API_KEY": "runta-secret-stub"})
        self.assertEqual(context.n_input_tokens, 8)
        self.assertEqual(context.n_cache_tokens, 3)
        self.assertEqual(context.n_output_tokens, 5)

    def test_harbor_adapter_uploads_and_runs_carry(self) -> None:
        self.exercise(self.harbor)

    def test_pier_adapter_uploads_and_runs_carry(self) -> None:
        self.exercise(self.pier)

    def test_missing_trace_after_a_crash_is_an_infrastructure_error(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            binary = root / "carry"
            binary.write_bytes(b"binary")
            old = dict(os.environ)
            os.environ.update({"CARRY_FRONTIER_BINARY": str(binary), "FIREWORKS_API_KEY": "stub"})
            try:
                agent = self.harbor(root, model_name="fireworks_ai/accounts/fireworks/models/kimi-k3")
                environment = _Environment(carry_return_code=1, trace_exists=False)
                asyncio.run(agent.setup(environment))
                with self.assertRaisesRegex(RuntimeError, "without producing a model trace"):
                    asyncio.run(agent.run("solve", environment, _FakeContext()))
            finally:
                os.environ.clear()
                os.environ.update(old)


if __name__ == "__main__":
    unittest.main()
