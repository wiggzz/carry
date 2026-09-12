#!/usr/bin/env python3
"""Behavior tests for FrontierHarness evidence normalization."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from carry_frontierharness.common import (  # noqa: E402
    carry_model_id,
    read_usage,
    run_command,
)


class CarryFrontierHarnessAdapterTests(unittest.TestCase):
    def write_trace(self, directory: Path) -> Path:
        trace = directory / "carry-trace.jsonl"
        events = [
            {
                "event": "model_response",
                "data": {
                    "usage": {
                        "input_tokens": 120,
                        "cached_input_tokens": 20,
                        "cache_write_input_tokens": 30,
                        "output_tokens": 15,
                    }
                },
            },
            {"event": "shell_finished", "data": {"result": {"exit_code": 0}}},
            {
                "event": "model_response",
                "data": {
                    "usage": {
                        "input_tokens": 280,
                        "cached_input_tokens": 80,
                        "cache_write_input_tokens": 40,
                        "output_tokens": 25,
                    }
                },
            },
        ]
        trace.write_text("\n".join(json.dumps(event) for event in events) + "\n")
        return trace

    def test_usage_uses_per_call_model_response_events(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            usage = read_usage(self.write_trace(Path(temp)))
        self.assertEqual(usage.calls, 2)
        self.assertEqual(usage.input_tokens, 400)
        self.assertEqual(usage.cached_input_tokens, 100)
        self.assertEqual(usage.cache_write_input_tokens, 70)
        self.assertEqual(usage.output_tokens, 40)
        self.assertEqual(usage.first_input_tokens, 120)
        self.assertEqual(usage.first_cached_input_tokens, 20)

    def test_model_route_removes_only_the_runner_provider_prefix(self) -> None:
        self.assertEqual(
            carry_model_id("fireworks_ai/accounts/fireworks/models/kimi-k3"),
            "accounts/fireworks/models/kimi-k3",
        )
        self.assertEqual(carry_model_id("moonshot/kimi-k3"), "moonshot/kimi-k3")

    def test_command_uses_stdin_prompt_and_does_not_embed_key(self) -> None:
        command = run_command(
            cwd="/workspace",
            session_dir="/logs/agent/carry",
            binary="/usr/local/bin/carry",
            model="accounts/fireworks/models/kimi-k3",
            api_base="https://api.fireworks.ai/inference/v1",
            prompt_path="/logs/agent/carry-task.md",
        )
        self.assertIn("< /logs/agent/carry-task.md", command)
        self.assertIn("--session-dir /logs/agent/carry", command)
        self.assertNotIn("FIREWORKS_API_KEY", command)
        self.assertNotIn("OPENAI_API_KEY", command)


if __name__ == "__main__":
    unittest.main()
