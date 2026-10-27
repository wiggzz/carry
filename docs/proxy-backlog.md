# Proxy follow-up backlog

This repository-relative backlog tracks the integrated proxy. PR140 was closed
without merging; its work is included in the PR141 candidate, not a separate
active worktree or merge prerequisite.

- [x] Request strict provider-enforced reviewer JSON schema, including Codex.
  Keep local semantic validation and fail closed on unsupported providers/refusals.
- [x] Replace bytes/4 estimates with a simple local `o200k_base` tokenizer.
  Use token budgets for ordinary review admission and bootstrap windows;
  keep a separate byte transport guard. Native usage remains the billing evidence.
- [x] Verify that a real user message survives compaction.
  The latest live `user` group is pinned; regression covers bad advice and stale
  drop opinions. This does not fabricate a user message for user-less inputs.
- [x] Dashboard cost comparison using the main estimator's frozen-trajectory
  approach: conventional threshold summaries vs Carry, including reviewer cost,
  separate from observed provider-usage costs and with incomplete coverage visible.
- [x] Consolidate the proxy estimator with the shared savings/report estimator.
  Both use the same local token basis; do not add tokenized removals directly
  to provider input usage.
- [x] Replace import windows, persistent selection, reviewer replay and keep leases
  with immutable original-prefix semantic advice and disposable projections.
  See [the current contract](proxy-fork-continuity.md); old checkpoint migration
  and operator compatibility are intentionally not provided.
- [x] Apply valid reviewer items independently, preserving sources for rejected
  replacement facts and returning bounded feedback in the next current ledger.
- [x] Asynchronous ordinary/replay review captures immutable H without holding a
  fitting primary. Completion may publish at H after the paid owner advances;
  sibling branches cannot inherit advice whose observation included a divergent
  suffix. Cohort/protection/dependency checks and paid settlement remain required.
- [x] One bounded no-fit review acquisition, never unbounded capacity rescue or
  partial-observation authority. Failure/no-help/timeout preserves local rejection.

No running proxy is restarted or upgraded by these source changes.
