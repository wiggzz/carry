# Carry

Carry is an experimental coding agent for people who want an inspectable trail of
what the model did—not just a patch. It gives a model a shell, records its work,
and saves the resulting patch, trace, and usage record.

> **Safety:** use a disposable checkout. Carry's shell is **not** a security
> boundary. Do not give it your home directory, secrets, credentials, or unrelated
> work.

## Why Carry

- **Keep useful evidence.** Human-authored content is kept by default. The model
  can protect evidence, mark stale tool output removable, or save a concise
  learning from it.
- **Compact only when it pays.** The default `economic` policy estimates whether
  the next request becomes cheaper before rewriting context; otherwise it keeps
  the existing history and its prompt-cache reuse.
- **Inspect the result.** Each session retains `final.patch`, `result.json`,
  `trace.jsonl`, and shell output. The trace has no API headers or keys.

Carry is an experiment, not a claim that compaction is always useful. See the
[context-policy design](docs/context-policy.md) for the lifecycle and accounting
rules.

## Evidence at a glance

### Official SWE-bench Verified 50

A matched 50-task run used the same source, task order, model, reasoning level,
and limits for Carry, Pi, and Codex. Carry resolved **32 / 50 (64%)** for
**$0.598024** modeled model cost in **40m44s**. Pi resolved **36 / 50 (72%)** for
**$0.661475** in **45m00s**: Carry was **9.6% lower-cost** and **9.5% faster**,
but **8 percentage points lower** in resolved rate.

| Harness | Resolved | Modeled model cost | Workflow wall time |
| --- | ---: | ---: | ---: |
| [Carry](https://github.com/wiggzz/carry/actions/runs/32545967486) | 32 / 50 (64%) | $0.598024 | 40m44s |
| [Pi](https://github.com/wiggzz/carry/actions/runs/32549988183) | 36 / 50 (72%) | $0.661475 | 45m00s |
| [Codex](https://github.com/wiggzz/carry/actions/runs/32547842935) | 37 / 50 (74%) | $1.045054 | 46m38s |

### FrontierHarness 30

Carry resolved **19 / 30 (63.3%)** on a frozen Terminal-Bench + DataCurve mix,
with a **$25.9656411** direct modeled token-cost lower bound (**$1.3666 / pass**),
**5m51s** median recorded agent time, and **5h19m45s** workflow wall time. The
main run and its copy-only evidence recovery are
[artifact-gated](docs/benchmark-results.md#frontierharness-30).

![FrontierHarness direct modeled token cost per resolved task](docs/assets/frontierharness-cost-per-pass.svg)

Against FrontierHarness's published Pi point, Carry is +3.3 points in pass rate,
43.8% lower in direct modeled cost per resolved task, and 22.6% lower in median
agent duration. **This is directional, not a head-to-head claim:** task IDs
partly overlap, but agent/model versions, evaluator/runtime/egress policy,
timeouts, and verifier semantics are not normalized.

Read [benchmark evidence](docs/benchmark-results.md) for immutable run links,
configuration, accounting, recovery provenance, and limitations. One run is
useful evidence, not a general quality ranking.

## Install and try it

### Install a release

Download the Linux x86_64 archive and `SHA256SUMS` from the
[latest release](https://github.com/wiggzz/carry/releases/latest), then verify
before installing:

```sh
sha256sum -c SHA256SUMS
mkdir -p ~/.local/bin
tar -xzf carry-<version>-x86_64-unknown-linux-gnu.tar.gz
install -m 0755 carry ~/.local/bin/carry
carry --help
```

### Build from source

```sh
git clone https://github.com/wiggzz/carry.git
cd carry
cargo build --release --locked
./target/release/carry --help
```

### Run an isolated task

Set an API key in the process environment and point Carry at a disposable
checkout:

```sh
export OPENAI_API_KEY=...
carry --cwd /path/to/disposable/repo fix the failing tests
```

An eligible ChatGPT subscription can use the Codex endpoint instead:

```sh
carry login
# headless host:
carry login --device-auth
```

`OPENAI_API_KEY` takes precedence and is required for a custom
`OPENAI_BASE_URL`. Remove the stored subscription credential with `carry logout`.
Use `-p` when the prompt begins with an option-like value.

Run without a prompt for an interactive session, or add `--interactive` after an
initial prompt. For the localhost-only UI, run:

```sh
carry --serve --cwd ../project
```

## Inspect a run

Sessions live under `$CARRY_HOME/sessions` (or `~/.carry/sessions`). Continue a
session by ID or path while writing the continuation elsewhere:

```sh
carry --resume 20260827-123456-abcdef --session-home /path/to/carry-home -p "continue"
carry --resume /path/to/prior-session --session-dir /path/to/fresh-output -p "new task"
```

Each session includes:

- `context-state.json` — atomically updated canonical context checkpoint
- `final.patch` — the agent's resulting patch
- `result.json` — outcome, usage, cost estimate, retries, and compactions
- `trace.jsonl` and shell-output files — chronological execution evidence

The default model is `gpt-5.6-luna`; choose another with `--model` or
`OPENAI_MODEL`. There is no default step limit; use `--max-steps N` only when a
specific cap is required.

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

Live fixtures require `OPENAI_API_KEY`; Codex fixture comparisons also need a
configured Codex CLI session. Neither runs in CI.

## Maintainer operations

The protected SWE-bench workflow is for maintainers. Read
[benchmark isolation](docs/benchmark-isolation.md) and the
[workflow](.github/workflows/run-swebench.yml) before dispatching it. Every
model-bearing official run uses disposable containers, a model-key-only agent
environment, credential-free grading, immutable task images, and exact cleanup
checks.

Contributions use Conventional Commit titles. CI runs formatting, Clippy, unit
tests, a release build, and the scripted fixture. Release Please publishes the
Linux x86_64 archive and checksum after its release PR merges.
