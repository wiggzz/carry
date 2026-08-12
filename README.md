# Carry

Carry is an intentionally small coding-agent harness for experimenting with model-managed context. After every observation, the model emits both its next action and the IDs of memory items that should survive into the following request.

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
