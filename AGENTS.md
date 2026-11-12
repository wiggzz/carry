# AGENTS.md

## Project goals

Carry is a coding harness and harness proxy that reduces cost by reducing the
context sent to the model. Context is selected either by the agent model itself
or by a secondary reviewer model.

While Carry is on 0.x.x, backward compatibility and preservation of existing
flags or behavior are not goals in themselves. Keep them only to the extent
that they remain useful. Ruthlessly restructure and remove functionality and
code to reach the right shape and behavior.

The priorities of this version are, in order:

1. **Task performance and solution quality:** perform roughly as well as a
   traditional harness; small differences are acceptable.
2. **Lowest total cost possible:** include reviewer calls, compaction and retry
   overhead, not just the primary model's input cost.
3. **Observability:** make cost savings, latency and task performance visible,
   with reasonable estimates of how Carry compares to a traditional harness.
   Distinguish observed results from estimates.
4. **Ease of use.**
5. **Cross-provider compatibility.**

Use this ordering to resolve trade-offs.

## Engineering principles

- Use Conventional Commits for every commit.
- Before making a test pass, run it and confirm it fails for the intended missing behavior—not a setup, syntax, or unrelated failure.
- Keep pull-request descriptions concise and structured with at least `# Why` and `# What`. Add `# Verification` only for notable manual verification that CI will not run automatically. Use optional `# Refs` and `# Notes` for connected documentation, issues, pull requests, or relevant observations that are not directly part of the change. Do not include mechanical file/change lists.
  - In the PR's `# Why` section: Describe the impact: for bugs how bad is it, when is it experienced and how broad, for features why will it help, how many users or potential users will it help
- Simplicity is the most important principle. If a simpler approach achieves the goal, always take it. Additional layers can be added later if they become necessary. Unneeded code and features create complexity and cruft, hinder refactoring and readability, and often introduce subtle bugs.
- Treat every bug, including those discovered during implementation, as a signal to inspect structural root causes and consider how to prevent the class of problem; not every bug needs a redesign.
- Treat new special cases, defensive checks, and layered fallbacks as design smells. Consider improving representation, ownership, or control flow so the problematic state cannot arise; conditionals are not inherently wrong.
- Make small, directly relevant, verifiable structural improvements within the task and existing permission, testing, and safety constraints. Flag larger opportunities for follow-up rather than silently expanding scope.
- Give defaults and fallback policy one authoritative owner. Resolve values at boundaries and maintain internal invariants instead of layering defensive checks with separate fallback behavior. Preserve necessary trust-boundary validation and explicit recovery from expected external failures.
- Do not write tests that assert on the wording or contents of system prompts or tool descriptions. Test behavior instead (ideally through evals).
