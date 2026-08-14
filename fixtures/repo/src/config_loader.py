"""JSON configuration loading with includes and environment expansion."""

import json
import os
from pathlib import Path
import re
from typing import Any, Mapping

from .config_merge import deep_merge


_VARIABLE = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")


def load_config(
    path: str | Path, environ: Mapping[str, str] | None = None
) -> dict[str, Any]:
    """Load a JSON object and recursively apply its includes."""
    # fixture:config-loader:start
    environment = os.environ if environ is None else environ

    def expand(value: Any) -> Any:
        if isinstance(value, str):

            def replace(match: re.Match[str]) -> str:
                name = match.group(1)
                if name not in environment:
                    raise ValueError(f"missing environment variable: {name}")
                return environment[name]

            return _VARIABLE.sub(replace, value)
        if isinstance(value, list):
            return [expand(item) for item in value]
        if isinstance(value, dict):
            return {key: expand(item) for key, item in value.items()}
        return value

    def load(current: Path, stack: tuple[Path, ...]) -> dict[str, Any]:
        resolved = current.resolve()
        if resolved in stack:
            cycle = stack[stack.index(resolved) :] + (resolved,)
            raise ValueError(
                "include cycle: " + " -> ".join(item.name for item in cycle)
            )
        try:
            raw = json.loads(resolved.read_text())
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid JSON in {resolved.name}: {error.msg}") from error
        if not isinstance(raw, dict):
            raise ValueError(f"configuration must be an object: {resolved.name}")

        includes = raw.pop("include", [])
        if isinstance(includes, str):
            includes = [includes]
        elif not isinstance(includes, list) or not all(
            isinstance(item, str) for item in includes
        ):
            raise ValueError(
                f"include must be a string or list of strings: {resolved.name}"
            )

        merged: dict[str, Any] = {}
        next_stack = stack + (resolved,)
        for include in includes:
            merged = deep_merge(merged, load(resolved.parent / include, next_stack))
        return deep_merge(merged, raw)

    return expand(load(Path(path), ()))
    # fixture:config-loader:end
