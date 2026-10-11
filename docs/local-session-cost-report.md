# Local-session cost report: private-archive case study

Carry's local report is intended to make the cost of iterative work inspectable.
Removing stale tool output before a conventional full-context summary boundary
can avoid carrying it through later requests. The size of that opportunity is
workload-dependent; compaction can also incur cache rewrites or lose useful
information.

## Archive coverage

The private archive covers August 16–October 2, 2026 (US Central time), with five
models and multiple Carry policy/logging versions. It contains:

- 43 session directories; 36 have traces and 30 have completed responses.
- 1,383 recorded requests and 1,370 unique completed provider responses.
- 142 recorded compactions across 20 response-bearing sessions.
- 13 unmatched requests, including requests in four response-bearing sessions.
- Two recorded retry counters; any additional retry charges are not inferred.

The normalized input, cached-input, cache-write, output and reasoning usage was
checked against the raw counters on all 1,370 completed responses. The
[aggregate evidence](local-session-cost-report.json) is deliberately
content-free: no prompts, tool output, response contents, session identities or
credentials are published. The underlying archive remains private, so this is
not an externally reproducible dataset release or a controlled quality trial.

## Two different kinds of cost

**Observed modeled usage cost** prices recorded completed-response usage using
the report's model rate card. It is not an invoice. Unanswered or unmetered
requests are not assigned zero cost, so a
known-response subtotal must not be presented as complete session spend.

**Scenario cost and reduction** belong to a separately labeled hypothetical
comparison. Both scenario branches must use a consistent input-size accounting
basis. A serialized-content byte proxy is not a provider tokenizer, and adding
that proxy to metered input would not identify actual billed savings.

The comparison freezes the recorded model/tool activity. It cannot tell us how
summarization or deletion would change future actions, re-reading, elapsed time
or task quality. Context limits, retained recent content, summary size and cache
behavior remain explicit scenario assumptions rather than reconstructed Pi
execution.

## Corrected replay results

The corrected shared estimator was executed on all 36 private full traces and
its aggregate output checked against the actual generated HTML report:

- **Observed modeled completed-response subtotal: $35.0652** across all 1,370
  responses. This is not complete spend because unanswered requests and extra
  retry charges remain unknown.
- **Scenario-eligible cohort: 25 of 30 response-bearing sessions, 1,122
  responses.** Four sessions with unmatched requests and one with retries are
  excluded from both scenario totals. The six traces without responses are also
  ineligible; seven archive directories have no trace.
- **Carry proxy scenario: $84.0242; Pi-style proxy scenario: $138.0847.**
  Conditional reduction: **$54.0605, or 39.2%** of the Pi-style scenario cost.
  These are hypothetical proxy-priced dollars, not those sessions' observed
  spend. The eligible cohort's observed modeled subtotal is separately $29.0770.
- **Zero simulated Pi-style summaries** in the eligible cohort. This scenario's
  reduction comes from carrying less removed context through repeated requests,
  not avoiding summary calls.

Both input branches use serialized prompt-bearing UTF-8 bytes divided by four,
rounded up; frozen metered output tokens are a common charge. The default
scenario window is 272,000 proxy units, with a 16,384 reserve, 20,000 recent
units and a 1,000-unit summary. Recorded window metadata overrides that configured
window when present; this is not validation of a provider model's token limit.
The existing model rate card is applied in the proxy domain, including its
input-size tiers and cache minimums.

Caching is modeled symmetrically: initial writes, minimum eligibility,
model/cache-key/recorded-route changes, recorded TTL and prefix edits. After a
Carry removal, the alternative assumes append/frozen-size reuse of the restored
context until its own summary. Its literal prefix is unknown. Across the full
archive, 1,383 requests lack route metadata and 27 lack usable expiry metadata;
reuse is conditional on same-route/unexpired caching in those cases. On 111
completed requests, unknown cache capability is priced uncached. Provider cache
acceptance is not established by the replay.

The proxy totals are much larger than the provider-usage subtotal. That is a
reason **not to multiply actual spend by 39.2%**: this byte proxy is uncalibrated,
not a replacement tokenizer. The comparison demonstrates conditional opportunity
under stated assumptions, not the magnitude of realizable billed savings.

## Inspect your own sessions

```sh
carry report cost --sessions /path/to/local/sessions --output local-cost-report.html
```

The HTML is local and contains session identities; review it before sharing.
Carry's live web UI and the offline report use the same estimator. Consult the
report's coverage and scenario labels rather than treating its percentage as a
billing claim.

## Interpretation

This archive motivates evaluating repeated, interactive work rather than
assuming that making a single coding task longer will reproduce the same
retention opportunities. It is also a different experiment from a deletion
oracle on no-compaction benchmark trajectories: their savings percentages and
search guarantees are not transferable ceilings for every local workflow.

An earlier coarse implementation at `8b09e9f` mixed byte-based removal estimates
with provider-metered input and assumed cache behavior. Its percentage has been
superseded as a headline claim; reproducing that arithmetic does not calibrate
its monetary interpretation. The corrected report keeps measured usage and the
hypothetical scenario separate, with missing-cost coverage visible.
