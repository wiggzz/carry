#!/usr/bin/env python3
"""Patch the pinned FrontierHarness image pre-pull path safely.

The pinned runner interpolates a Docker image through a nested remote shell. Runta
executes argv directly, so preserve the image as one argv element instead.
"""

from __future__ import annotations

import argparse
from pathlib import Path


OLD = '''  if [ "$suite" = datacurve ]; then
    image=$(retry_transport runta exec "$runtime" -- sh -lc \\
      "awk -F '\\"' '/^docker_image *=/ { print \\$2; exit }' /work/deep-swe/tasks/$(shell_quote "$task")/task.toml") || return 1
  else
    toml="$TASKS/$task/task.toml"
    # A Skills CLI install has no adjacent task data. For a subset list, resolve
    # task images from the benchmark workspace before trying a repository install.
    [ -f "$toml" ] || toml="tasks/$task/task.toml"
    [ -f "$toml" ] || toml="$SCRIPT_DIR/../../../tasks/$task/task.toml"
    [ -f "$toml" ] || return 0 # A custom --cmd may prepare its own environment.
    image=$(awk -F'"' '/^docker_image *=/ { print $2; exit }' "$toml")
  fi
  [ -n "$image" ] || return 0
  retry_transport runta exec "$runtime" -- sh -lc "timeout 1800 docker pull $(shell_quote "$image")"
'''

NEW = '''  # Prefer the workspace task.toml so image names are not parsed through nested
  # remote shells. DeepSWE copies in this repo match the pinned corpus.
  toml="$TASKS/$task/task.toml"
  [ -f "$toml" ] || toml="tasks/$task/task.toml"
  [ -f "$toml" ] || toml="$SCRIPT_DIR/../../../tasks/$task/task.toml"
  if [ -f "$toml" ]; then
    image=$(awk -F'"' '/^docker_image *=/ { print $2; exit }' "$toml")
  elif [ "$suite" = datacurve ]; then
    image=$(retry_transport runta exec "$runtime" -- sh -lc \\
      "awk -F '\\"' '/^docker_image *=/ { print \\$2; exit }' /work/deep-swe/tasks/$(shell_quote "$task")/task.toml" \\
      | tail -n1 | tr -d '\\r') || return 1
  else
    return 0
  fi
  image=${image//$'\\r'/}
  printf 'prepare_image: pulling %s\\n' "$image" >&2
  [ -n "$image" ] || return 0
  retry_transport runta exec "$runtime" -- timeout 1800 docker pull "$image" || return 1
  # Pier builds an egress-proxy from ubuntu:24.04 after the trial allowlist is applied.
  printf 'prepare_image: pulling ubuntu:24.04\\n' >&2
  retry_transport runta exec "$runtime" -- timeout 1800 docker pull ubuntu:24.04
'''


def apply(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if 'retry_transport runta exec "$runtime" -- timeout 1800 docker pull "$image"' in text:
        return
    if text.count(OLD) != 1:
        raise RuntimeError(f"unexpected FrontierHarness prepare_image layout: {path}")
    path.write_text(text.replace(OLD, NEW, 1), encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, type=Path)
    args = parser.parse_args()
    apply(args.target)
