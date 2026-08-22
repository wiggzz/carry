#!/usr/bin/env python3
"""Reject pull-request titles omitted by this repository's Release Please config."""
import argparse
import re


VISIBLE_TYPES = ("feat", "fix", "perf", "revert")
BREAKING_ONLY_TYPES = ("build", "chore", "ci", "docs", "refactor", "style", "test")
SCOPE = r"(?:\([a-z0-9][a-z0-9._/-]*\))?"
TITLE_PATTERN = re.compile(
    rf"(?:{'|'.join(VISIBLE_TYPES)}){SCOPE}!?: [^\s].*"
    rf"|(?:{'|'.join(BREAKING_ONLY_TYPES)}){SCOPE}!: [^\s].*"
    r"|chore\(main\): release [0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?"
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
        "title must be a release-note-eligible Conventional Commit; use "
        "feat:, fix:, perf:, or revert: (optional scope/!), for example "
        "'feat: add shell completion' or 'fix(parser): preserve nested output'"
    )


if __name__ == "__main__":
    raise SystemExit(main())
