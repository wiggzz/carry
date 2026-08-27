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

Carry defaults to `gpt-5.6-luna`; override it with `--model` or `OPENAI_MODEL`.
There is no default step limit. Use `--max-steps N` only when you need a cap.

## Inspect a run

Sessions are written under `$CARRY_HOME/sessions` or `~/.carry/sessions`.
Choose another parent with `--session-home`, or choose the exact directory with
`--session-dir`.

Each session includes:

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
