#!/usr/bin/env python3
"""Behavior tests for the pull-request title validator."""
import pathlib
import subprocess
import sys
import unittest


SCRIPT = pathlib.Path(__file__).with_name("validate_pr_title.py")


class PullRequestTitleTests(unittest.TestCase):
    def validate(self, title: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), title],
            text=True,
            capture_output=True,
            check=False,
        )

    def test_accepts_release_note_eligible_conventional_titles(self):
        titles = (
            "feat: add shell completion",
            "fix(parser): preserve nested tool results",
            "perf!: replace the cache format",
            "revert: restore the previous cache format",
            "docs!: remove a deprecated public command",
            "chore(main): release 0.6.0",
        )
        for title in titles:
            with self.subTest(title=title):
                result = self.validate(title)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_accepts_non_release_note_conventional_titles(self):
        titles = (
            "build(deps): update a development dependency",
            "chore: tidy generated metadata",
            "ci: cache Rust dependencies",
            "docs: clarify first-run setup",
            "refactor(parser): simplify title parsing",
            "style: normalize Markdown wrapping",
            "test: cover malformed titles",
        )
        for title in titles:
            with self.subTest(title=title):
                result = self.validate(title)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_malformed_or_unknown_titles(self):
        titles = (
            "Handle historical Git metadata in benchmark preflight",
            "Fix: uppercase types are not conventional",
            "feature: unknown types are not accepted",
            "fix missing separator",
            "fix: ",
            " fix: leading whitespace",
            "fix: multiline subject\nwith a second line",
        )
        for title in titles:
            with self.subTest(title=title):
                result = self.validate(title)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Conventional Commit", result.stderr)


if __name__ == "__main__":
    unittest.main()
