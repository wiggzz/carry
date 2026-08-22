# AGENTS.md

- Use Conventional Commits for every commit.
- Before making a test pass, run it and confirm it fails for the intended missing behavior—not a setup, syntax, or unrelated failure.
- Keep pull-request descriptions concise and structured with at least `# Why` and `# What`. Add `# Verification` only for notable manual verification that CI will not run automatically. Use optional `# Refs` and `# Notes` for connected documentation, issues, pull requests, or relevant observations that are not directly part of the change. Do not include mechanical file/change lists.
- Simplicity is the most important principle. If a simpler approach achieves the goal, always take it. Additional layers can be added later if they become necessary. Unneeded code and features create complexity and cruft, hinder refactoring and readability, and often introduce subtle bugs.
