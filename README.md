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

The default model is `gpt-5.6-luna`; override it with `--model` or `OPENAI_MODEL`. Runs have no default step limit. Use `--max-steps N` only when an explicit cap is required. Responses API `429` and `5xx` responses are retried up to five times with the server's `Retry-After-Ms`/`Retry-After` delay (capped at 30 seconds), or bounded exponential backoff when no valid delay is supplied. Each retry resends the identical request body and is recorded in the run log.

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

The manually dispatched **Benchmark plan** workflow builds a requested Carry branch, tag, or commit and emits five deterministic shards from the frozen 50-instance SWE-bench selection. It does not run agents or require model credentials. The live Carry/Codex/Pi benchmark worker will be added separately after its external isolated execution and protected credential path are reviewed.

The manually dispatched **EC2 benchmark bootstrap** workflow has a credential-free
`bootstrap` default and an executable `smoke-5` preset. It requires the Terraform
deployment first, then these protected `swe-bench` GitHub Environment values:

- `BENCHMARK_AWS_REGION`
- `BENCHMARK_DISPATCH_ROLE_ARN`
- `BENCHMARK_ARTIFACT_BUCKET`
- `BENCHMARK_ARTIFACT_SESSION_ROLE_ARN`
- `BENCHMARK_WORKER_LAUNCH_TEMPLATE_ID`
- `BENCHMARK_WORKER_LAUNCH_TEMPLATE_VERSION` (the numeric `worker_launch_template_version` Terraform output)
- secret `OPENAI_API_KEY` (smoke only)
- optional `BENCHMARK_MODEL` (defaults to `gpt-5.6-luna`) and
  `BENCHMARK_REASONING` (defaults to `medium`)

The reviewed worker code pins Node 22.19 and Rust base-image manifest digests,
`@openai/codex@0.147.0`, and `@earendil-works/pi-coding-agent@0.84.2`.
It builds all three run-local images once on the disposable worker; no registry
publishing pipeline or additional protected configuration is needed.

Dispatch the workflow on the reviewed branch and use that same branch in `carry_ref`;
the workflow checks out the dispatch event's immutable commit and rejects a different
candidate ref. Leave `mode=bootstrap` for the existing canary, or
choose `smoke-5` for the first five IDs of the frozen selected-50 across Carry/Codex/Pi
(15 mandatory records). To stay inside the initial run's one-hour chained-role
capability lifetime, this nonofficial plumbing baseline caps each agent slot at six
minutes, runs five agent slots concurrently, and caps each evaluator container at five
minutes. Those caps are recorded in provenance and are not the settings for the later
official comparison. The smoke incurs EC2, model-token, storage, and image-build costs.
The workflow uploads a `swebench-smoke-5-*` artifact containing predictions, all slot
metadata/traces, official outputs, canonical records, and the report.

The key is encrypted in a run-scoped S3 object, fetched through a short-lived presigned
URL into root-only `/dev/shm`, and injected into agent containers only by environment
name. The host transiently holds it in the Actions secret environment, the encrypted
upload stream, worker process environment, and tmpfs. It is deleted before evaluation.
The worker has no AWS credentials and uploads through a presigned PUT; it self-terminates,
while the controller retains exact-instance and exact-object cleanup.

A reviewable live-run manifest is available without executing a model:

```sh
python3 scripts/swebench_live_runner.py plan --run-id review-1 --output live-plan.json
```

It fixes the denominator at 50 tasks × Carry/Codex/Pi = 150 records and records
distinct external agent/evaluator container contracts. A manual protected
workflow can invoke one selected task/method with digest-pinned external
containers. It forwards `OPENAI_API_KEY` only to the agent by environment name;
the evaluator gets neither that key nor model configuration or agent output.
Scaling to 50 remains blocked on measured capacity/cost, longer presigned URL and job
timeouts, concurrency design, cache strategy, and successful smoke evidence. The
selected-50 planning and 150-record denominator remain unchanged. See
`docs/benchmark-isolation.md`.
Every credential-bearing agent harness—Carry, Codex, Pi, or another
alternative—must run inside a disposable container or equivalent sandbox that
limits its filesystem, process, and network reach. Carry Bubblewrap support is
useful defense in depth, but is not the sole security boundary. The later mode
must preserve the protected-environment, explicit-ref, run-scoped-artifact, and
always-cleanup boundaries. See `docs/benchmark-isolation.md` for the v1 contract.

## Releases and contributions

Use Conventional Commits. CI runs formatting, Clippy, unit tests, a release build, and the scripted fixture on pull requests and `main`. Release Please opens a release PR from conventional commits; merging that PR creates a GitHub Release with a Linux x86_64 binary and checksum.
