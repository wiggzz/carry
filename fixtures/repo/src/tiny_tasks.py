"""Small functions used by Carry's local coding fixtures."""

import re
import unicodedata


def clamp(value: float, lower: float, upper: float) -> float:
    """Return value constrained to the inclusive range [lower, upper]."""
    # fixture:clamp:start
    if lower > upper:
        raise ValueError("lower must not exceed upper")
    return min(max(value, lower), upper)
    # fixture:clamp:end


def slugify(value: str) -> str:
    """Convert a title into a lowercase ASCII URL slug."""
    # fixture:slugify:start
    ascii_value = (
        unicodedata.normalize("NFKD", value).encode("ascii", "ignore").decode("ascii")
    )
    return re.sub(r"[^a-z0-9]+", "-", ascii_value.lower()).strip("-")
    # fixture:slugify:end


def median(values: list[float]) -> float:
    """Return the median without modifying the caller's list."""
    # fixture:median:start
    if not values:
        raise ValueError("median requires at least one value")
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2
    # fixture:median:end
