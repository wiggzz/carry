"""Runner-neutral Carry command and usage helpers.

The FrontierHarness runners import suite-specific adapters, but both persist the
same Carry trace format. Keep evidence parsing here so the runner context and
the patched FrontierHarness cost collector agree exactly.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
import shlex
from typing import Any


FIREWORKS_RESPONSES_BASE = "https://api.fireworks.ai/inference/v1"


@dataclass(frozen=True)
class Usage:
    calls: int = 0
    input_tokens: int = 0
    cached_input_tokens: int = 0
    cache_write_input_tokens: int = 0
    output_tokens: int = 0
    first_input_tokens: int | None = None
    first_cached_input_tokens: int | None = None


def carry_model_id(model: str) -> str:
    """Translate runner model notation to the Fireworks Responses model ID."""
    return model.removeprefix("fireworks_ai/")


def _integer(value: Any) -> int:
    return value if isinstance(value, int) and value >= 0 else 0


def read_usage(trace: Path) -> Usage:
    """Read only complete Carry model_response events from a JSONL trace."""
    calls: list[tuple[int, int, int, int]] = []
    try:
        lines = trace.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError):
        return Usage()

    for line in lines:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict) or event.get("event") != "model_response":
            continue
        data = event.get("data")
        usage = data.get("usage") if isinstance(data, dict) else None
        if not isinstance(usage, dict):
            continue
        item = (
            _integer(usage.get("input_tokens")),
            _integer(usage.get("cached_input_tokens")),
            _integer(usage.get("cache_write_input_tokens")),
            _integer(usage.get("output_tokens")),
        )
        if any(item):
            calls.append(item)

    if not calls:
        return Usage()
    return Usage(
        calls=len(calls),
        input_tokens=sum(item[0] for item in calls),
        cached_input_tokens=sum(item[1] for item in calls),
        cache_write_input_tokens=sum(item[2] for item in calls),
        output_tokens=sum(item[3] for item in calls),
        first_input_tokens=calls[0][0],
        first_cached_input_tokens=calls[0][1],
    )


def run_command(
    *,
    cwd: str,
    session_dir: str,
    binary: str,
    model: str,
    api_base: str,
    prompt_path: str,
) -> str:
    """Build a shell-safe noninteractive Carry command.

    The API key is injected as process environment by the runner adapter. It
    intentionally never appears in this command or the runner command log.
    """
    values = [
        binary,
        "--cwd",
        cwd,
        "--session-dir",
        session_dir,
        "--model",
        model,
        "--api-base",
        api_base,
    ]
    return " ".join(shlex.quote(value) for value in values) + " < " + shlex.quote(prompt_path)
