#!/usr/bin/env python3
"""Add Carry trace accounting to the pinned FrontierHarness v1 collector.

This patch is intentionally narrow and refuses an unexpected upstream layout.
The evaluation workflow pins the FrontierHarness commit before calling it.
"""

from __future__ import annotations

import argparse
from pathlib import Path


CARRY_PARSER = '''\n\ndef _carry(text: str) -> Detail:
    # Carry trace.jsonl records exactly one model_response event per completed
    # Responses API call. input_tokens includes cached reads; cached_input_tokens
    # is the corresponding cached portion.
    usages = []
    for event in _jsonl(text):
        if event.get("event") != "model_response":
            continue
        data = event.get("data") or {}
        usage = data.get("usage") if isinstance(data, dict) else None
        if not isinstance(usage, dict):
            continue
        total = int(usage.get("input_tokens") or 0)
        cached = int(usage.get("cached_input_tokens") or 0)
        if total or cached or usage.get("output_tokens"):
            usages.append((total, cached))
    return _detail(usages)


def _carry_totals(text: str) -> tuple[int, int, int, int] | None:
    total_input = cached = cache_write = output = calls = 0
    for event in _jsonl(text):
        if event.get("event") != "model_response":
            continue
        data = event.get("data") or {}
        usage = data.get("usage") if isinstance(data, dict) else None
        if not isinstance(usage, dict):
            continue
        item_input = int(usage.get("input_tokens") or 0)
        item_cached = int(usage.get("cached_input_tokens") or 0)
        item_cache_write = int(usage.get("cache_write_input_tokens") or 0)
        item_output = int(usage.get("output_tokens") or 0)
        if item_input or item_cached or item_cache_write or item_output:
            calls += 1
            total_input += item_input
            cached += item_cached
            cache_write += item_cache_write
            output += item_output
    if not calls:
        return None
    return total_input, cached, cache_write, output
'''


def apply(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if '"carry": ("carry-trace.jsonl", _carry),' in text:
        return
    marker = "\n# harness name -> (glob relative to the trial agent dir, parser)\n"
    if text.count(marker) != 1:
        raise RuntimeError(f"unexpected FrontierHarness usage_details layout: {path}")
    text = text.replace(marker, CARRY_PARSER + marker, 1)
    registry_marker = '    "hermes": ("hermes-calls.jsonl", _hermes),\n'
    if text.count(registry_marker) != 1:
        raise RuntimeError(f"could not find FrontierHarness registry marker: {path}")
    text = text.replace(
        registry_marker,
        registry_marker + '    "carry": ("carry-trace.jsonl", _carry),\n',
        1,
    )
    old = '''    if harness != "hermes" or not agent_dir.exists():
        return None
    files = sorted(agent_dir.glob("hermes-calls.jsonl"))
'''
    new = '''    if harness not in ("hermes", "carry") or not agent_dir.exists():
        return None
    pattern = "carry-trace.jsonl" if harness == "carry" else "hermes-calls.jsonl"
    files = sorted(agent_dir.glob(pattern))
'''
    if text.count(old) != 1:
        raise RuntimeError(f"could not find FrontierHarness totals marker: {path}")
    text = text.replace(old, new, 1)
    old_return = "        return _hermes_totals(files[-1].read_text())\n"
    new_return = "        parser = _carry_totals if harness == \"carry\" else _hermes_totals\n        return parser(files[-1].read_text())\n"
    if text.count(old_return) != 1:
        raise RuntimeError(f"could not find FrontierHarness totals return: {path}")
    path.write_text(text.replace(old_return, new_return, 1), encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, type=Path)
    args = parser.parse_args()
    apply(args.target)
