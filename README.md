# Carry

Carry is an intentionally small coding-agent harness for experimenting with model-managed context. After every observation, the model emits both its next action and an explicit context-management update.

The latest tool result is automatically visible for one step. The model can retain its exact bounded output by ID, retain older items, drop items by omission, and add concise durable memories. Exact tool results and model-authored memories share one byte budget.

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

Every structured response contains:

```json
{
  "action": { "kind": "shell", "command": "...", "answer": null },
  "context_management": {
    "retain_ids": ["t0001", "m0001"],
    "add_memories": ["Concise durable conclusion"]
  }
}
```

`retain_ids` is the complete retention set: any existing item omitted from it expires. Tool-result IDs begin with `t`; model-authored memory IDs begin with `m`. The shared retained-context budget defaults to 8,000 bytes and can be changed with `--context-budget-bytes`.

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
