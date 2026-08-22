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

    def test_rejects_titles_omitted_from_release_notes(self):
        titles = (
            "Handle historical Git metadata in benchmark preflight",
            "Fix: uppercase types are not conventional",
            "feature: unknown types are not accepted",
            "docs: hidden Release Please types need a breaking marker",
            "build(deps): hidden dependency updates are not release notes",
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
