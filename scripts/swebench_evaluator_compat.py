#!/usr/bin/env python3
"""Fail-closed compatibility for the pinned SWE-bench SymPy parser."""

from __future__ import annotations

import re
import runpy
import json
import os
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


def install_prepared_runtime(images: dict[str, str]) -> None:
    """Replace only the Docker spawn boundary, not official grading or scripts.

    A measured paired lane supplies the resolver's exact alias->config-ID map.
    No pull/build fallback or evaluator egress is permitted in this mode.
    """
    from swebench.harness import docker_build
    if not images or any(not isinstance(k, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', v)
                         for k, v in images.items()):
        raise ValueError('prepared evaluator map requires immutable local image IDs')
    def spawn(test_spec, client, run_id, logger, nocache, force_rebuild=False):
        del logger, nocache
        expected = images.get(test_spec.instance_image_key)
        if force_rebuild or expected is None or client.images.get(test_spec.instance_image_key).id != expected:
            raise RuntimeError('official evaluator prepared image missing or changed; builds/pulls forbidden')
        return client.containers.create(image=expected, name=test_spec.get_instance_container_name(run_id),
            user='root', detach=True, command='tail -f /dev/null', platform=test_spec.platform,
            network_mode='none', cap_drop=['ALL'], security_opt=['no-new-privileges'],
            pids_limit=512, mem_limit='4g', nano_cpus=2_000_000_000)
    docker_build.build_container = spawn


def main() -> None:
    """Install the compatibility parser, then execute the official evaluator CLI."""
    if version("swebench") != "4.1.0":
        raise RuntimeError("evaluator compatibility requires swebench==4.1.0")
    from swebench.harness import run_evaluation
    from swebench.harness.log_parsers import MAP_REPO_TO_PARSER

    install_sympy_parser(MAP_REPO_TO_PARSER)
    if 'CARRY_SWEBENCH_PREPARED_IMAGES' in os.environ:
        install_prepared_runtime(json.loads(os.environ['CARRY_SWEBENCH_PREPARED_IMAGES']))
    evaluator_path = getattr(run_evaluation, "__file__", None)
    if not evaluator_path:
        raise RuntimeError("pinned SWE-bench evaluator module has no source path")
    runpy.run_path(evaluator_path, run_name="__main__")


if __name__ == "__main__":
    main()
