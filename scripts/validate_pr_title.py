#!/usr/bin/env python3
"""Reject malformed or unknown Conventional Commit pull-request titles."""
import argparse
import re


VISIBLE_TYPES = ("feat", "fix", "perf", "revert")
NON_RELEASE_TYPES = ("build", "chore", "ci", "docs", "refactor", "style", "test")
CONVENTIONAL_TYPES = VISIBLE_TYPES + NON_RELEASE_TYPES
SCOPE = r"(?:\([a-z0-9][a-z0-9._/-]*\))?"
TITLE_PATTERN = re.compile(
    rf"(?:{'|'.join(CONVENTIONAL_TYPES)}){SCOPE}!?: [^\s].*"
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
        "'feat: add shell completion', 'docs: clarify setup', or "
        "'fix(parser): preserve nested output'"
    )


if __name__ == "__main__":
    raise SystemExit(main())
