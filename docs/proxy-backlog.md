# Proxy follow-up backlog

Current work is in `feat/proxy-local-client-support` (`~/carry-pr-140`).

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
- [x] Consolidate the proxy estimator with the shared savings/report estimator
  after `feat/pi-cost-savings-estimate` merges. Both must use the same local token
  basis; do not add tokenized removals directly to provider input usage.
- [x] Shared bounded windows for bootstrap and oversized ongoing reviews.
  The live `0d16e73c823a…` session had ~1.07 MiB / 436,617 estimated tokens in its
  reviewer projection alone, beyond both the old byte cap and the new token cap.
  Both paths now retire old projections and carry sourced memories; atomic
  oversized groups stay protected. Ongoing sweep progress survives failed
  primaries/restarts, with newer context deferred to later sweeps.
  `--review-max-calls` bounds both paths; the old bootstrap flag remains an alias.
- [x] Apply valid reviewer items independently and return bounded rejection
  feedback in the next mutable ledger, persisted across failed primaries/restarts.
- [x] Opt-in background ordinary reviews (`--background-reviews`) to remove reviewer
  round-trip latency from primary requests; bootstrap remains synchronous and is
  deferred while old-lineage review work is still in flight.
  - Dispatch from an immutable snapshot without holding the primary session lock.
  - Reacquire the lock only to merge usage and validated advice; never overwrite a
    newer checkpoint or mutate a request already submitted to the provider.
  - Check lineage/epoch and current group eligibility; discard stale advice after
    a rebase, removal or changed tool cohort. Keep source provenance validation.
  - Bound one in-flight review per session, handle shutdown/restart and failures,
    and test latency, simultaneous continuation, rebases and failed primaries.

No running proxy is restarted or upgraded by these source changes.
