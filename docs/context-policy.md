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

Tool results end with a marker such as `[context 2 volatile]`. Carry preserves
native Responses API output items, including reasoning items, next to the
function result that produced them.

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
prices that request with and without a rewrite. Under the default `economic`
policy, Carry compacts only when the rewritten next request is already cheaper.

A compaction can remove explicitly removable items and selected neutral volatile
items, retain protected evidence, preserve chronology, and establish a new
explicit cache frontier. After the first rewrite that removes history, Carry
adds one stable status item stating that earlier context was removed.

Use `--compaction-policy disabled` when you need a no-compaction control. Carry
still records the session, but it never asks the planner to rewrite history.
Both the selected policy and aggregate compaction count appear in `result.json`
and `trace.jsonl`.

## Session-persistence benchmark mode

`session-smoke-5` is a separate **Carry-only** stress-test mode, not an ordinary
SWE-bench score. It uses the frozen five-task smoke manifest in its recorded
order. Every task gets a fresh prepared workspace/image and normal per-task
SWE-bench grading. Each completed Carry slot writes its native versioned
`context-state.json` checkpoint into that slot's output directory; the next slot
mounts that whole completed session read-only as `--resume` input and writes a
fresh destination session. The source trace is audit evidence and is never
modified by a later task.

Each new task prompt explicitly says that its `/testbed` is new and that old
paths, patches, and conclusions must not be reused. The artifacts record the
task order, session ID, retained-context flag, per-task position, source and
destination checkpoint SHA-256 values, and fresh workspace/evaluator isolation.
Raw checkpoints are not uploaded. If a continuation source or completed output
lacks `context-state.json`, the runner fails closed rather than launching a
fresh-context replacement.

This mode rejects Codex and Pi until their own native, cross-container session
contracts have equivalent behavioral verification.

## What the policy does not promise

Compaction is not guaranteed to reduce total task cost or improve task success.
It is an observed trajectory decision: hard tasks may compact more often, and a
rewrite can affect later reasoning. Compare policies on the same predeclared
tasks before treating compaction as a causal explanation for cost or quality.
