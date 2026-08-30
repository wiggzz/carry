# Carry

Carry is an experimental coding agent that tries to spend less context without
throwing away the evidence needed to finish the job. It runs a model with a
shell, keeps a chronological record of the work, and writes a patch plus a
trace you can inspect afterward.

Use it on a disposable checkout. Carry's shell is **not** a security boundary:
never give it a repository, home directory, or container that contains secrets
or unrelated work.

## Why Carry exists

Most coding agents either keep every tool result in context or periodically
summarize history with a fixed rule. Carry takes a different approach:

- The model marks recent evidence as protected, removable, or worth remembering.
- A cache-aware planner decides whether rewriting context will make the **next**
  request cheaper. If it will not, Carry keeps the existing history.
- The result is a chronological, inspectable record instead of an opaque
  summary. `trace.jsonl`, shell outputs, `result.json`, and `final.patch` stay
  with the session.

This is still an experiment. The point is to make the tradeoff measurable, not
claim that context compaction is always useful.

## Latest benchmark result

The latest complete comparison was a fixed 50-task SWE-bench Verified run with
`gpt-5.6-luna` at medium reasoning. All 150 Carry, Codex, and Pi slots were
evaluated.

| Harness | Resolved | Recorded model cost |
| --- | ---: | ---: |
| Carry | 40 / 50 | $0.536538 |
| Codex | 41 / 50 | $1.092152 |
| Pi | 39 / 50 | $0.649433 |

Carry was one task behind Codex and one ahead of Pi on this catalog while using
less recorded model spend than either. These are artifact-recorded estimates,
not provider invoices, and one 50-task run is evidence rather than a general
performance claim. The [run artifact](https://github.com/wiggzz/carry/actions/runs/33029120967)
contains the per-task outcomes, tokens, costs, and provenance.

## Try it

### Install a release

Download the Linux x86_64 archive and its `SHA256SUMS` file from the
[latest release](https://github.com/wiggzz/carry/releases/latest). Verify the
archive before installing it:

```sh
sha256sum -c SHA256SUMS
mkdir -p ~/.local/bin
tar -xzf carry-<version>-x86_64-unknown-linux-gnu.tar.gz
install -m 0755 carry ~/.local/bin/carry
carry --help
```

### Or build from source

```sh
git clone https://github.com/wiggzz/carry.git
cd carry
cargo build --release --locked
./target/release/carry --help
```

### Run a task

Set an OpenAI API key in the process environment, then point Carry at an
isolated repository checkout:

```sh
export OPENAI_API_KEY=...
carry --cwd /path/to/disposable/repo fix the failing tests
```

Use `-p` when the prompt itself starts with an option-like value:

```sh
carry --cwd /path/to/disposable/repo -p "explain why --release is failing"
```

Run `carry` without a prompt for an interactive session. You can also add
`--interactive` after an initial prompt. While Carry works, type another
instruction to queue it after the current shell action. `/help`, `/quit`, and
`/exit` work at the prompt.

Run `carry --serve --cwd ../project` to launch the embedded local UI at `http://127.0.0.1:8765`; use `--port` to choose another port. The UI submits the initial task and later steering through a local HTTP API, and receives activity over SSE. It is deliberately localhost-only. Context status appears as protected, removable, and removed pills on the relevant output. To continue an existing session in the browser, run `carry --resume SESSION --serve`, where `SESSION` is either a session directory or an ID below the configured session home; the UI's first message becomes the resumed session's fresh human turn.

The default model is `gpt-5.6-luna`; override it with `--model` or `OPENAI_MODEL`. Sessions have no default step limit. Use `--max-steps N` only when an explicit per-turn cap is required. Responses API `429` responses with `error.code=rate_limit_exceeded` and connection failures before a response is received are retried up to five times. Carry honors `Retry-After-Ms`, numeric `Retry-After`, and HTTP-date `Retry-After`; the total wait budget is 60 seconds, and Carry stops rather than sending earlier when the server requests a longer delay. Missing or invalid delay headers and transport failures use bounded exponential backoff. Retries resend the identical request body; successful model-response events include the retry count, and exhausted errors include the retry/wait summary. Quota failures and ambiguous `5xx` POST failures are not replayed.

## Inspect a run

Sessions are written under `$CARRY_HOME/sessions` or `~/.carry/sessions`.
Choose another parent with `--session-home`, or choose the exact output directory with
`--session-dir`. Resume by either a session directory or its ID under that home:

```sh
carry --resume 20260827-123456-abcdef --session-home /path/to/carry-home -p "continue"
carry --resume /path/to/prior-session --session-dir /path/to/fresh-output -p "new task"
```

The second form reads only the prior conversation state and writes the continued run to
its new output directory.

Each session includes:

- `context-state.json`: versioned canonical context checkpoint, atomically replaced after state changes
- `final.patch`: the patch produced by the agent
- `result.json`: outcome, aggregate token use, cost estimate, and compaction count
- `trace.jsonl`: chronological structured events with no API headers or key
- `trace.log` and shell-output files: human-readable execution evidence

The terminal prints compact per-step input, cache-read, cache-write, and output
token counts. `result.json` records the selected compaction policy and retries.

## Context policy

Carry retains human messages and model-authored memories by default. Tool
interactions start in a recent working window. The model may protect evidence
that must survive, mark evidence removable, or save one concise learning from
a bulky tool result.

The default `economic` policy compares the next model request with and without
compaction, then rewrites only when the compacted request is already cheaper.
For an ablation or control run, disable compaction explicitly:

```sh
carry --compaction-policy disabled --cwd /path/to/disposable/repo fix the tests
# or: CARRY_COMPACTION_POLICY=disabled carry ...
```

Carry records the selected policy in `trace.jsonl` and `result.json`. See the
[context-policy design](docs/context-policy.md) for the ledger, retention
markers, cache frontier, and rewrite rules.

### Context-pressure reminders

Opt-in `--context-pressure-reminder-at-tokens N` appends a compact advisory
reminder to the newest persisted tool result before each model request at or
above `N` rendered-context tokens. It lists up to ten older large tool-context
IDs so the model can preserve an essential conclusion with
`remember`/`protected` or mark stale IDs `removable` in its normal context
update. The decorated tool result is checkpointed, preserving exact request
prefixes across later rounds. The reminder never deletes or summarizes state
itself; normal economic compaction decides whether to apply model-authorized
removal on the following request.

For example, `139264` is half of a 272-Ki-token short-context budget:

```sh
carry --context-pressure-reminder-at-tokens 139264 --cwd /path/to/repo fix the tests
```

`trace.jsonl` records every reminder, threshold, estimated context size, and
suggested IDs; session/result metadata records the configured threshold. In the
protected SWE-bench workflow, set the identically named
`carry_context_pressure_reminder_at_tokens` input; leave it empty for the
control behavior.

## Development

```sh
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release --locked
```

The deterministic fixture smoke needs Docker but no model credential:

```sh
docker build --tag carry:dev .
./scripts/run-fixture.sh clamp scripted
```

Live fixtures require `OPENAI_API_KEY`. Codex fixture comparisons also require
a separately configured Codex CLI session; neither runs in CI.

## SWE-bench operations

The manual, protected `Run SWE-bench` workflow is for maintainers. It has
credential-free `bootstrap`, catalog-building `prepare-50`, pull-only
`smoke-5`, and fixed-denominator `official-50` modes. Read
[benchmark isolation](docs/benchmark-isolation.md) and
[the workflow](.github/workflows/run-swebench.yml) before dispatching one.

Every official model-bearing run uses disposable containers, a model-key-only
agent environment, separate credential-free grading, immutable task-image
digests, and exact cleanup checks. The benchmark has real model and cloud cost;
review its source commit, catalog digest, and expected spend before dispatch.

## Releases and contributions

Use a release-note-eligible Conventional Commit for each pull request title:
`feat`, `fix`, `perf`, or `revert`, with an optional scope and breaking-change
marker. CI accepts the other Conventional Commit types only with `!`, because
Release Please hides their non-breaking entries. CI runs formatting, Clippy,
unit tests, a release build, and the scripted fixture. Release Please opens a
release PR; merging it publishes a Linux x86_64 binary and checksum.
