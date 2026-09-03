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
item only when it contains learning not represented elsewhere. Mark an item
removable when it taught nothing, or after its learning has been preserved.
A later opposite signal reverses the earlier opinion. If both name the same ID
in one response, protection wins. Unknown and stale IDs are ignored.

`remember` accepts one concise learning per turn. The memory stays associated
with its source tool result without duplicating the text in the rendered
history. If a later compaction removes the source but retains the memory, Carry
materializes the memory as an assistant message with the same ID.

## Economic compaction

Between rewrites, retained history grows by exact appends so the model provider
can reuse a stable prompt-cache prefix. Before a model request, the planner
prices retaining the cached history against paying for a rewrite. The economic
policy uses a three-request payoff period: it charges the rewrite once, then
compares two later cache-read requests over the compacted and retained histories.
It also requires projected savings to exceed 10% of the retained-path payoff
cost. This deliberately avoids rewrites that only barely repay their cache
invalidation. A compaction still begins a new cache generation; the model-visible
history is otherwise prefix-continuous.

A compaction can remove explicitly removable items and selected neutral volatile
items, retain protected evidence, preserve chronology, and establish a new
explicit cache frontier. After the first rewrite that removes history, Carry
adds one stable status item stating that earlier context was removed.

Use `--compaction-policy disabled` when you need a no-compaction control. Carry
still records the session, but it never asks the planner to rewrite history.
Both the selected policy and aggregate compaction count appear in `result.json`
and `trace.jsonl`.

## Experimental keep leases

`--keep-lease-turns N` (or `CARRY_KEEP_LEASE_TURNS=N`) is disabled by default.
When enabled, a model `protected` signal is a lease for `N` later model turns,
not a permanent lock. Carry sweeps on the persisted `N`-turn cadence (rather
than on every individual expiry) and batches every due lease into prose appended
after the *newly completed* tool result. The next model action can renew an ID
only by naming it again in `protected`; after that action completes, an
unrenewed reviewed ID becomes neutral and volatile. Expiry is not an implicit
`removable` decision and never deletes an item by itself—the normal economic
planner may choose a later whole-round rewrite.

A resume and final answer do not independently create a review/status block:
reviews are emitted only with completed real tool results. The tool result plus
its optional review is checkpointed as one immutable context block before the
next provider request, preserving prompt-cache prefix continuity until an
intentional compaction rewrite.

Each `context_compacted` trace event includes `retention_audit`, with every
pre-rewrite item’s ID, estimated tokens, kept/removed outcome, and reason
(active lease, expired lease, explicit removable, neutral policy, or stable
baseline). Lease review and expiry events are also persisted in `trace.jsonl`.
The review is appended to persisted native context, so it extends the previous
request history and preserves prompt-cache continuity until a normal rewrite.

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
