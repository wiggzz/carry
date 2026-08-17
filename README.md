# Carry

Carry is an experimental agentic coding harness that aims to minimize token use and cost while maintaining performance comparable to state-of-the-art harnesses. It lets the model signal which context matters while a cache-aware planner decides when compaction is worth its rewrite cost. Each model turn must produce one structured `shell` or `finish` function call and a sparse context-management update.

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

Set an OpenAI API key only in the process environment, then run Carry against a disposable repository checkout. Prompt words can be passed directly or with `-p` when the text contains option-like values.

```sh
export OPENAI_API_KEY=...
carry --cwd /path/to/disposable/repo fix the failing tests
carry --cwd /path/to/disposable/repo -p "explain why --release is failing"
```

Run `carry` without a prompt to start an interactive session, or add `--interactive` to continue after an initial prompt. Input remains active while the model and shell commands run; steering is queued and appended immediately after the current action completes. Use `/help`, `/quit`, or `/exit` at the prompt.

The default model is `gpt-5.6-luna`; override it with `--model` or `OPENAI_MODEL`. Sessions have no default step limit. Use `--max-steps N` only when an explicit per-turn cap is required. Responses API `429` responses with `error.code=rate_limit_exceeded` and connection failures before a response is received are retried up to five times. Carry honors `Retry-After-Ms`, numeric `Retry-After`, and HTTP-date `Retry-After`; the total wait budget is 60 seconds, and Carry stops rather than sending earlier when the server requests a longer delay. Missing or invalid delay headers and transport failures use bounded exponential backoff. Retries resend the identical request body; successful model-response events include the retry count, and exhausted errors include the retry/wait summary. Quota failures and ambiguous `5xx` POST failures are not replayed.

Session data is written beneath `$CARRY_HOME/sessions` or `~/.carry/sessions` by default. Use `--session-home` to select another parent or `--session-dir` to choose the exact directory. A session contains `trace.jsonl`, `trace.log`, shell outputs, `result.json`, and `final.patch`. Model request traces exclude HTTP headers and the API key.

The terminal shows compact per-step input, cache-read, cache-write, and output token counts. Compactions are called out as `minor` or `major` with the dropped, retained, and rewritten token estimates; `result.json` records aggregate token usage and compaction counts.

## Context policy

Carry keeps one strictly chronological context ledger. Human messages and memories enter stable retention; tool interactions begin volatile. Stable items persist by default, while volatile items remain present until a compaction planner decides that acting on the model's signals will save more than rewriting the cache costs. The stable cache frontier is the longest chronological prefix made entirely of stable items; later stable human messages remain durable without reordering history or forcing earlier volatile items to promote.

Every item has a compact integer ID and an immutable marker such as `[2 tool volatile]`. Carry preserves all native Responses API output items—including reasoning items—alongside function results. Between compactions, retained history grows by exact appends for prompt-cache reuse. A minor compaction acts only on volatile drop candidates; a major compaction may also remove stable drop candidates. Both preserve chronology, make the retained generation stable, and establish a new explicit cache frontier. Cache age participates in the decision, so an expired cache is rebuilt promptly instead of preserving a no-longer-useful prefix.

Each action includes context management:

```json
{
  "command": "...",
  "message": "Checking the focused tests first.",
  "context": {
    "keep": [2, 3],
    "drop": [],
    "remember": ["Concise durable reasoning outcome"]
  }
}
```

`keep` and `drop` are sparse, sticky advisory signals, limited to four IDs each per turn. `keep` marks exact items the task cannot safely lose; stable items already persist by default and normally need no vote. `drop` marks exact content that is no longer useful, but Carry may defer the deletion to preserve a valuable cached prefix. A later opposite signal reverses the prior opinion, and `keep` wins if both name the same ID in one response. Unknown or stale IDs are ignored. Before marking exact context for dropping, `remember` can preserve up to two useful conclusions, constraints, evidence, decisions, or unresolved questions without retaining chain-of-thought.

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

## Running SWE-bench

The manually dispatched **Run SWE-bench** workflow has a credential-free
`bootstrap` default plus protected `smoke-5` and `official-50` modes. It requires the Terraform
deployment first, then these protected `swe-bench` GitHub Environment values:

- `BENCHMARK_AWS_REGION`
- `BENCHMARK_DISPATCH_ROLE_ARN`
- `BENCHMARK_ARTIFACT_BUCKET`
- `BENCHMARK_ARTIFACT_SESSION_ROLE_ARN`
- `BENCHMARK_WORKER_LAUNCH_TEMPLATE_ID`
- `BENCHMARK_WORKER_LAUNCH_TEMPLATE_VERSION` (the numeric `worker_launch_template_version` Terraform output)
- secret `OPENAI_API_KEY` (protected benchmark modes only)
- optional `BENCHMARK_MODEL` (defaults to `gpt-5.6-luna`) and
  `BENCHMARK_REASONING` (defaults to `medium`)

The reviewed worker code pins Node 22.19 and Rust base-image manifest digests,
`@openai/codex@0.147.0`, and `@earendil-works/pi-coding-agent@0.84.2`.
It builds all three run-local images once on the disposable worker; no registry
publishing pipeline or additional protected configuration is needed.

Dispatch the workflow on the reviewed branch and use that same branch in `carry_ref`;
the workflow checks out the dispatch event's immutable commit and rejects a different
candidate ref. Leave `mode=bootstrap` for the canary, choose `smoke-5` for the first
five frozen IDs (15 mandatory records), or choose `official-50` for all 50 frozen IDs
across Carry/Codex/Pi (150 mandatory records). Apply the Terraform change that raises
the protected dispatch role's maximum session to six hours before dispatching the
official mode. Automation never applies Terraform.

The official mode uses one disposable worker and builds each run-local method image
once. It executes five ordered ten-task agent shards at concurrency three, removes each
shard's working checkouts after patch capture, deletes the model key before grading,
and evaluates each method in ten-task shards with `swebench==4.1.0`. It writes the full
150-slot `not-run` checkpoint before image builds and checkpoints after every agent and
evaluator shard, so infrastructure failure cannot silently shrink the denominator. Carry's
aggregate Responses API retry count is copied into each slot record and method summary.
Each agent slot remains capped at six minutes. The limit is also passed into each named
agent container; host-side cleanup retries and verifies exact-container absence, failing
the worker closed if Docker cannot prove it stopped. Official execution reserves 190
minutes for agents, 90 minutes for grading at evaluator concurrency ten, and 20 minutes
for setup inside a five-hour wall clock that starts before package/source setup. Evaluator
shards have a 345-second outer cap, leaving 225 seconds of phase-level orchestration
margin. After every evaluator return path, exact-run containers receive verified cleanup
because SWE-bench may suppress its own stop/remove failures. Queued agent
slots become explicit budget-exhausted diagnostics rather than silently disappearing.
The controller permits twenty minutes for EC2 launch/boot around the worker clock and
reserves the remaining forty minutes of its six-hour job for termination and exact cleanup.
All limits, model-derived pricing, and image identities are recorded in provenance. Pricing is
looked up by exact model ID in the reviewed benchmark code and accounts separately for ordinary
input, cache reads, cache writes, and output; unknown models report cost as unavailable. The workflow
streams deduplicated slot start/completion/grading events from the worker's EC2 console and
publishes the final method and per-slot performance/time/token/cost table in the GitHub run
summary. The same data remains in `records.json`, `report.json`, and `report.md` in the artifact.

The smoke uses one-hour S3 capabilities. During the longer official mode, the protected
controller rotates exact-run presigned control/result capabilities every 25 minutes;
the zero-permission worker receives only those opaque capabilities and never AWS
credentials. The workflow uploads a `swebench-<mode>-*` artifact containing predictions,
all slot metadata/traces, official outputs, canonical records, and the report. Both live
modes incur EC2, model-token, storage, and image-build costs.

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
The selected-50 planning and 150-record denominator remain unchanged. Official execution
is deliberately manual and protected; review the immutable source commit and expected
cost before dispatch. See `docs/benchmark-isolation.md`.
Every credential-bearing agent harness—Carry, Codex, Pi, or another
alternative—must run inside a disposable container or equivalent sandbox that
limits its filesystem, process, and network reach. Carry Bubblewrap support is
useful defense in depth, but is not the sole security boundary. The later mode
must preserve the protected-environment, explicit-ref, run-scoped-artifact, and
always-cleanup boundaries. See `docs/benchmark-isolation.md` for the v1 contract.

## Releases and contributions

Use Conventional Commits. CI runs formatting, Clippy, unit tests, a release build, and the scripted fixture on pull requests and `main`. Release Please opens a release PR from conventional commits; merging that PR creates a GitHub Release with a Linux x86_64 binary and checksum.
