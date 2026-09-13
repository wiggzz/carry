#!/usr/bin/env python3
"""Patch the pinned collector to select a canonical task attempt result."""

from __future__ import annotations

import argparse
from pathlib import Path


OLD = '''    paths = sorted((trial_dir / "jobs").rglob("result.json"))
    # Job summaries contain totals for their children. Never charge both, or
    # silently combine retries into the canonical attempt.
    leaves = [p for p in paths if not any(p.parent in q.parents for q in paths if q != p)]
'''

NEW = '''    paths = sorted((trial_dir / "jobs").rglob("result.json"))
    # A canonical Harbor/Pier task result has an immediate agent directory. Nested
    # agent-native result.json files are auxiliary evidence, not evaluator results.
    leaves = [p for p in paths if (p.parent / "agent").is_dir()]
'''


def apply(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if 'leaves = [p for p in paths if (p.parent / "agent").is_dir()]' in text:
        return
    if text.count(OLD) != 1:
        raise RuntimeError(f"unexpected FrontierHarness calculate-cost layout: {path}")
    path.write_text(text.replace(OLD, NEW, 1), encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, type=Path)
    args = parser.parse_args()
    apply(args.target)
