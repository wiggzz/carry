# Context policy

Carry keeps one chronological context ledger. It is designed to make a context
rewrite an explicit, inspectable decision instead of a periodic summary.

## Item lifecycle

- Human messages and saved memories enter stable retention.
- Tool interactions begin volatile.
- Stable items stay retained by default.
- Neutral volatile items remain in a recent working window and can be removed
  when the planner needs room.
- The model can mark visible item IDs as `protected` or `removable`.

Carry preserves the original order. A later stable message does not force an
earlier volatile tool result to become stable. The stable cache frontier is the
longest chronological prefix made entirely of stable items.

Tool rounds end with one immutable marker such as `[context 2 volatile]`. A
round atomically contains the provider-native assistant tool call and its matching
function result, so compaction never leaves an orphaned tool result or tool call.
The marker records the block's creation class; later retention changes never
rewrite a historical marker.

## Model signals

Each action makes progress and may attach a sparse context update:

```json
{
  "command": "...",
  "message": "Checking the focused tests first.",
  "context": {
    "protected": [2, 3],
    "removable": [],
    "remember": ["Concise learning preserved from bulky evidence"]
  }
}
```

`protected` and `removable` each accept up to four IDs per turn. Protect an
item only when it contains learning not represented elsewhere. Leave eligible
items unlisted; use `removable` to release an already protected item only when
it taught nothing, or after its learning has been preserved. A later opposite
signal reverses the earlier opinion. If both name the same ID in one response,
protection wins. Unknown and stale IDs are ignored.

`remember` accepts one concise learning per turn. When it safely represents a
bulky source, leave that source eligible or mark it `removable` if it was
protected. The memory stays associated with its source tool result without
duplicating the text in the rendered history. If a later compaction removes the
source but retains the memory, Carry materializes the memory as an assistant
message with the same ID.

## Economic compaction

Between rewrites, retained history grows by exact appends so the model provider can
reuse a stable prompt-cache prefix. The planner compares a compact-now candidate
with retaining the current history over the same projected cost model.

`--compaction-min-payback-percent P` (or
`CARRY_COMPACTION_MIN_PAYBACK_PERCENT=P`) makes that admission margin explicit:
Carry compacts only when modeled savings exceed `P%` of the retain-path payoff cost.
It accepts integer values from 0 through 100 and defaults to **25**. Carry previously
used a hard-coded 10% gate; the new default more deliberately filters marginal
rewrites. `0` means any strictly positive modeled saving; lower the value if
you need to admit more speculative rewrites.

`--compaction-payoff-requests N` (or `CARRY_COMPACTION_PAYOFF_REQUESTS=N`)
sets the maximum deterministic forecast horizon; `N` must be positive and
defaults to `5`. The immediate request is always priced. Each subsequent
request has probability `(1-q)^(t-1)` of occurring, where
`q=--compaction-rollout-stop-probability-percent` (or
`CARRY_COMPACTION_ROLLOUT_STOP_PROBABILITY_PERCENT`), an integer from 0 to
100 defaulting to 10. Each surviving request grows by one estimated tool-turn
item on **all** compared paths. The planner sums request costs weighted by
these survival probabilities; it does not sample rollouts. `q=0` prices the
full horizon and `q=100` prices only the first request. The old
`--compaction-rollout-samples` mode is retired and rejected.

Under the experimental `batch-ordinary` lease policy, every structurally
valid compact-now candidate is priced against keeping the same projected
future. The candidate with the largest positive expected saving is admitted
only if it exceeds `P%` of the expected keep-path cost. That percentage gate
can change when genuinely common future work increases both path costs, even
if their absolute difference stays the same. An admitted rewrite still begins
a new cache generation; otherwise model-visible history is prefix-continuous.
The baseline lease policy keeps its original next-request admission gate as an
unchanged control.

Neutral working-set hysteresis is configurable with
`--compaction-neutral-high-watermark-tokens N` / `CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS=N`
and `--compaction-neutral-low-watermark-tokens N` /
`CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS=N`. Both default to **zero**,
so Carry retains no ordinary neutral working set after a qualifying compaction.
The low watermark must not exceed the high watermark. Setting both to `32768`
and `24576`, respectively, restores the former 32 Ki-token high watermark and
24 Ki-token post-compaction target. With the zero defaults, every otherwise
eligible neutral item is a drop candidate at each planner boundary. This does
not bypass ordinary economic admission, cache-safety, human retention, or
explicit model protection; it only removes the neutral-budget reason to retain
an item.

A compaction can remove explicitly removable items and selected neutral volatile
items, retain protected evidence, preserve chronology, and establish a new
explicit cache frontier. After the first rewrite that removes history, Carry
adds one stable status item stating that earlier context was removed.

Use `--compaction-policy disabled` when you need a no-compaction control. Carry
still records the session, but it never asks the planner to rewrite history.
Both the selected policy and aggregate compaction count appear in `result.json`
and `trace.jsonl`.

## Experimental keep leases

`--keep-lease-turns N` (or `CARRY_KEEP_LEASE_TURNS=N`) defaults to **8**
in the native CLI. A blank benchmark input omits the flag but inherits that
native default; it does **not** disable leases. The current CLI has no
lease-only disable switch. A model `protected` signal is a lease for `N`
later model turns, not a permanent lock. `--lease-review-policy baseline`
is the default: an already-qualified ordinary compaction takes precedence over
a due-lease review.
Use `--lease-review-policy batch-ordinary` (or
`CARRY_LEASE_REVIEW_POLICY=batch-ordinary`) to opt into the experimental
comparison on the same binary and source commit. Once one or more leases are
due, Carry asks the ordinary planner whether compaction already qualifies.
Under the `batch-ordinary` treatment, Carry compares **keep**, **compact
now**, and **review first** over the same probability-weighted future. Each
branch receives the same projected future tool-turn items, including one on
each surviving later request. The first request pays its actual modeled cache
read/write or rewrite cost; review also pays the annotation and cannot remove
reviewed material until the second request. The chance of reaching request
`t` is `(1 - q)^(t - 1)`, where `q` is the configured stop probability.
Both ordinary compaction and review must beat **keeping** on expected net
input-equivalent cost, and review must also beat compacting now. There is no
extra margin on this final three-way comparison; the configured payback
margin still applies when the ordinary planner admits candidates, so use `--compaction-min-payback-percent 0`
to test a margin-free treatment. The delayed candidate projects a protected
tool-turn item through the second request and is compared against the same
future growth and stopping distribution as keep and compact-now.
This is an **experimental forecast**, not a calibrated prediction: it assumes
reviewed IDs are omitted and a delayed rewrite occurs, without forecasting
renewal or further planner actions. Positive forecast savings are not observed
cost savings. The content-free `keep_lease_review_decision.data.paired_forecast`
and `context_compacted.data.expected_value` fields record the compared costs
and settings. No sampled rollout policy remains. A review never occurs when
compaction is disabled or the horizon contains no post-review request.

A qualifying review names at most the four largest due blocks, matching the
`protected` field's four-ID limit. The concise tool-result annotation directs
the model to renew an ID only by naming it in `context.protected`; it can use
`context.remember` for one concise durable learning. Any reviewed ID that is
not renewed is released from working memory: after the next model response it
becomes neutral and eligible for the next normal compaction. Unreviewed due
leases stay protected and due, so a later review advances to the next largest
set rather than asking the model to decide more IDs than it can renew. Expiry
is not an implicit `removable` signal and never deletes an item by itself.

A resume and final answer do not independently create a review/status block:
reviews are attached only to completed real tool results. The tool result plus
its optional review is checkpointed as one context block before the next
provider request, preserving prompt-cache prefix continuity until an
intentional compaction rewrite.

Each `context_compacted` trace event includes `retention_audit`, with every
pre-rewrite item’s ID, estimated tokens, kept/removed outcome, and reason
(active lease, expired lease, explicit removable, neutral policy, or stable
baseline). A `retention_revalidation_requested` event records reviewed IDs and the
selection scope (`reviewed_wave_virtual_omission` for a qualifying ordinary
plan under `batch-ordinary`, or `all_due_virtual_release` when no ordinary plan
qualifies). With leases enabled, each completed tool-result planner boundary also
emits a content-free `keep_lease_review_decision` event: due/reviewable counts and
estimated tokens, the selected wave size, request/skip and reason, and available
ordinary, expanded, delayed and annotation-cost estimates. A skipped decision is
not a review; count `retention_revalidation_requested` separately and join
subsequent `context_signals` and `context_compacted` events for actual renewal
and removal. The review wave event records projected savings, the one-request
delay estimate, and annotation cost. Pending review suppresses compaction
until the next model response has seen the annotation and provided renewal
signals, even if a resumed run disables new leases; afterward normal
compaction resumes. The
review is appended to persisted native context, extending the previous request
history and preserving prompt-cache continuity until a normal rewrite.

## Session-persistence benchmark mode

`session-smoke-5` and `session-20` are retained-session experiment modes, not ordinary
SWE-bench scores. Each accepts exactly one sequential native harness: Carry, Codex,
or Pi (never `all` or a mixed selection). `session-smoke-5` uses the frozen five-task
smoke manifest; `session-20` uses the first twenty IDs in the recorded frozen-50
order. Every task gets a fresh prepared workspace/image and normal per-task
SWE-bench grading.

Carry retains its existing behavior: each completed slot writes its native
versioned `context-state.json` checkpoint into that slot's output directory; the
next slot mounts that whole completed session read-only as `--resume` input and
writes a fresh destination session. The source trace is audit evidence and is
never modified by a later task.

Codex retains its native thread by keeping a worker-local `CODEX_HOME` directory
and resuming the one audited thread UUID for each later slot. The runner records
only SHA-256 values for the native JSONL state and thread ID; login credentials
are removed before the retained state is recorded.

Pi retains its native JSONL session by mounting one worker-local session directory
writable only into the sequential Pi agents. Each Pi invocation uses the explicit
native `--session` file and `--session-dir`; no Pi home/configuration state is
persisted. The raw JSONL session is not uploaded. Artifacts contain only its
SHA-256 values plus the run-scoped session ID and fixed path-safe file name
`session.jsonl`.

Each new task prompt explicitly says that its `/testbed` is new and that old
paths, patches, and conclusions must not be reused. The artifacts record the
task order, session ID, retained-context flag, per-task position, source and
destination checkpoint/session SHA-256 values, and fresh workspace/evaluator
isolation. If a continuation source or completed output lacks the required native
session persistence, the runner fails closed rather than launching a
fresh-context replacement.

## What the policy does not promise

Compaction is not guaranteed to reduce total task cost or improve task success.
It is an observed trajectory decision: hard tasks may compact more often, and a
rewrite can affect later reasoning. Compare policies on the same predeclared
tasks before treating compaction as a causal explanation for cost or quality.
