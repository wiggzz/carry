# Carry

Carry is an experimental agentic coding harness that aims to minimize token use and cost while maintaining performance comparable to state-of-the-art harnesses. It lets the model dynamically retain only necessary context, reducing conversation-history bloat and avoiding regular compaction during long-running tasks. Each model turn must produce one structured `shell` or `finish` function call and an explicit context-management update.

> **Security:** Carry's shell tool is not a security boundary. Run it only in a disposable checkout or another isolation mechanism you control. Never give an agent a workspace containing secrets or unrelated source trees.

## Install

### Linux x86_64 release

Download the `carry-<version>-x86_64-unknown-linux-gnu.tar.gz` asset from the [latest release](https://github.com/wiggzz/carry/releases/latest), verify it with the accompanying `SHA256SUMS`, then place `carry` on your `PATH`.

```sh
tar -xzf carry-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
install -m 0755 carry ~/.local/bin/carry
```

### Build from source

```sh
git clone https://github.com/wiggzz/carry.git
cd carry
cargo build --release --locked
./target/release/carry --help
```

## Use

Set an OpenAI API key only in the process environment, then run Carry against a disposable repository checkout.

```sh
export OPENAI_API_KEY=...
carry run \
  --cwd /path/to/disposable/repo \
  --task-file task.md \
  --run-dir runs/my-run
```

The default model is `gpt-5.6-luna`; override it with `--model` or `OPENAI_MODEL`. Runs have no default step limit. Use `--max-steps N` only when an explicit cap is required.

A run directory contains `trace.jsonl`, `trace.log`, shell outputs, `result.json`, and `final.patch`. Model request traces exclude HTTP headers and the API key.

## Context policy

New tool interactions and memories begin volatile. They survive only when the model explicitly retains them. Volatile items promote into a stable generation after the configured retention age; stable items persist until explicitly released. Carry uses immutable history segments and stable-generation cache checkpoints to preserve reusable request prefixes.

Each action includes context management:

```json
{
  "command": "...",
  "context_management": {
    "retain_volatile_ids": ["t0001", "m0001"],
    "release_stable_ids": [],
    "add_memories": ["Concise durable conclusion"]
  }
}
```

`retain_volatile_ids` is the complete volatile survival set. Stable items are retained by default and are released only through `release_stable_ids`.

## Development

```sh
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release --locked
```

The deterministic fixture smoke test requires Docker but no model credentials:

```sh
docker build --tag carry:dev .
./scripts/run-fixture.sh clamp scripted
```

Live fixtures require `OPENAI_API_KEY`. Codex fixture comparisons require a separately configured Codex CLI session; neither runs in CI.

## Benchmark planning

The manually dispatched **Benchmark plan** workflow builds a requested Carry branch, tag, or commit and emits five deterministic shards from the frozen 50-instance SWE-bench selection. It does not run agents or require model credentials. The live Carry/Codex benchmark worker will be added separately after its isolated execution and protected credential path are reviewed.

## Releases and contributions

Use Conventional Commits. CI runs formatting, Clippy, unit tests, a release build, and the scripted fixture on pull requests and `main`. Release Please opens a release PR from conventional commits; merging that PR creates a GitHub Release with a Linux x86_64 binary and checksum.
