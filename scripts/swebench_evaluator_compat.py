#!/usr/bin/env python3
"""Fail-closed compatibility for the pinned SWE-bench SymPy parser."""

from __future__ import annotations

import re
import runpy
from collections.abc import MutableMapping
from importlib.metadata import version
from typing import Any


_STATUS_PRIORITY = {"PASSED": 1, "FAILED": 2, "ERROR": 3}


def _record_status(statuses: dict[str, str], test: str, status: str) -> None:
    """Keep the most severe observation for duplicate unqualified test names."""
    previous = statuses.get(test)
    if previous is None or _STATUS_PRIORITY[status] > _STATUS_PRIORITY[previous]:
        statuses[test] = status


def parse_log_sympy_fail_closed(log: str, test_spec: Any) -> dict[str, str]:
    """Parse SymPy output without allowing a later duplicate pass to hide failure."""
    del test_spec
    statuses: dict[str, str] = {}
    for match in re.findall(r"(_*) (.*)\.py:(.*) (_*)", log):
        _record_status(statuses, f"{match[1]}.py:{match[2]}", "FAILED")
    for raw_line in log.split("\n"):
        line = raw_line.strip()
        if not line.startswith("test_"):
            continue
        test = line.split()[0]
        if line.endswith(" E"):
            _record_status(statuses, test, "ERROR")
        elif line.endswith(" F"):
            _record_status(statuses, test, "FAILED")
        elif line.endswith(" ok"):
            _record_status(statuses, test, "PASSED")
    return statuses


def install_sympy_parser(parsers: MutableMapping[str, Any]) -> None:
    """Replace only the pinned SymPy parser and fail if its registration moved."""
    if "sympy/sympy" not in parsers:
        raise RuntimeError("pinned SWE-bench parser map lacks sympy/sympy")
    parsers["sympy/sympy"] = parse_log_sympy_fail_closed


def main() -> None:
    """Install the compatibility parser, then execute the official evaluator CLI."""
    if version("swebench") != "4.1.0":
        raise RuntimeError("evaluator compatibility requires swebench==4.1.0")
    from swebench.harness import run_evaluation
    from swebench.harness.log_parsers import MAP_REPO_TO_PARSER

    install_sympy_parser(MAP_REPO_TO_PARSER)
    evaluator_path = getattr(run_evaluation, "__file__", None)
    if not evaluator_path:
        raise RuntimeError("pinned SWE-bench evaluator module has no source path")
    runpy.run_path(evaluator_path, run_name="__main__")


if __name__ == "__main__":
    main()
