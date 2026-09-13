#!/usr/bin/env python3
"""Make pinned FrontierHarness base-tool provisioning resilient to mirror 5xxs."""

from __future__ import annotations

import argparse
from pathlib import Path


OLD = '''  if command -v apt-get >/dev/null; then
    apt-get update -qq
    apt-get install -y -qq git curl ca-certificates jq python3 python3-venv util-linux >/dev/null
  fi
'''

NEW = '''  if command -v apt-get >/dev/null; then
    for attempt in 1 2 3; do
      if apt-get update -qq && apt-get install -y -qq git curl ca-certificates jq python3 python3-venv util-linux >/dev/null; then
        break
      fi
      if [ "$attempt" = 3 ]; then
        echo "base tooling apt install failed after $attempt attempts" >&2
        exit 1
      fi
      sleep "$attempt"
    done
  fi
'''


def apply(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if 'base tooling apt install failed after $attempt attempts' in text:
        return
    if text.count(OLD) != 1:
        raise RuntimeError(f"unexpected FrontierHarness base-tool provisioning layout: {path}")
    path.write_text(text.replace(OLD, NEW, 1), encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, type=Path)
    args = parser.parse_args()
    apply(args.target)
