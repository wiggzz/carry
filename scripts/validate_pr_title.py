#!/usr/bin/env python3
"""Reject pull-request titles that Release Please cannot parse."""
import argparse
import re


TYPES = (
    "build",
    "chore",
    "ci",
    "docs",
    "feat",
    "fix",
    "perf",
    "refactor",
    "revert",
    "style",
    "test",
)
TITLE_PATTERN = re.compile(
    rf"(?:{'|'.join(TYPES)})(?:\([a-z0-9][a-z0-9._/-]*\))?!?: [^\s].*"
)


def is_valid_title(title: str) -> bool:
    return bool(TITLE_PATTERN.fullmatch(title))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("title", help="pull-request title")
    args = parser.parse_args()
    if is_valid_title(args.title):
        return 0
    parser.error(
        "title must be a Conventional Commit, for example "
        "'feat: add shell completion' or 'fix(parser): preserve nested output'"
    )


if __name__ == "__main__":
    raise SystemExit(main())
