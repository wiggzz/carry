# Carry

Carry is an intentionally small coding-agent harness for experimenting with model-managed context. Each model turn emits exactly one required function call—`shell` or `finish`—whose strict arguments include an explicit context-management update.

The latest tool interaction is automatically visible for one step. The model can retain that interaction by ID, retain older interactions, drop items by omission, and add concise durable memories. A retained interaction replays the native assistant `function_call` item and its `function_call_output` verbatim. Interactions and model-authored memories share one byte budget.

This is an early experiment. Its shell tool is not a security sandbox; run it only in a disposable checkout or container.

## Build and test

```bash
cargo test
```

## Run against a repository

```bash
export OPENAI_API_KEY=...

cargo run -- run \
  --cwd /path/to/disposable/repo \
  --task-file task.md \
  --run-dir runs/my-run
```

The default model is `gpt-5.6-luna`. Override it with `--model` or `OPENAI_MODEL`.

Every action's strict function arguments contain:

```json
{
  "command": "...",
  "context_management": {
    "retain_ids": ["t0001", "m0001"],
    "add_memories": ["Concise durable conclusion"]
  }
}
```

`retain_ids` is the complete retention set: any existing item omitted from it expires. Tool-interaction IDs begin with `t`; model-authored memory IDs begin with `m`. The shared retained-context budget defaults to 8,000 serialized bytes and can be changed with `--context-budget-bytes`.

Requests are ordered for prefix reuse: stable system prompt, stable task, retained native history in chronological order, the latest automatic interaction, then a small changing context-status message. Carry uses `store: false` and `reasoning.context: "current_turn"`; it does not implicitly preserve prior reasoning or unselected response items.

The run directory contains `trace.jsonl`, a concise `trace.log`, complete shell outputs, `result.json`, and `final.patch`. Each live `model_request` event in `trace.jsonl` includes the complete JSON request body sent to the Responses API (excluding HTTP headers and the API key).

To inspect every model request from a run:

```bash
jq 'select(.event == "model_request") | .data.request' runs/*/artifacts/trace.jsonl
```

## Contained fixture tasks

Build the agent image:

```bash
docker build -t carry:dev .
```

Run a deterministic end-to-end smoke test. This exercises the same loop without calling a model:

```bash
./scripts/run-fixture.sh clamp scripted
```

Run a live fixture:

```bash
OPENAI_API_KEY=... ./scripts/run-fixture.sh clamp live
OPENAI_API_KEY=... ./scripts/run-fixture.sh slugify live
OPENAI_API_KEY=... ./scripts/run-fixture.sh median live
```

Each invocation creates a fresh Git repository under `runs/`, makes only the selected function incomplete, runs Carry inside the container, and grades the resulting checkout with tests that were not mounted into the agent container.
