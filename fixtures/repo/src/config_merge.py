"""Non-mutating merge support for the configuration fixture."""

from copy import deepcopy
from typing import Any


def deep_merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    """Merge nested mappings while treating every other value as replaceable."""
    # fixture:config-loader:start
    result = deepcopy(base)
    for key, value in override.items():
        previous = result.get(key)
        if isinstance(previous, dict) and isinstance(value, dict):
            result[key] = deep_merge(previous, value)
        else:
            result[key] = deepcopy(value)
    return result
    # fixture:config-loader:end
