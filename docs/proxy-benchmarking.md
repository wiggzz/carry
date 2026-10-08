# Opt-in Codex and Pi proxy trials

`carry proxy` sits between a native HTTP Responses client and its provider. `off` forwards without review; `audit` performs paid shadow review without selecting a rewritten primary context; `compact` enables the proxy planner. These modes are experiments, not a claim of savings or client compatibility established by unit tests.

The benchmark default is **`disabled`**, which retains the existing direct lanes. `disabled` and proxy `off` are distinct treatments. Compare `off` versus `compact` on the same candidate, tasks, model, effort, client configuration and native-compaction settings. Measure `disabled` versus `off` separately as transport overhead.

## A local trial

Prerequisites: Python 3.10+, a candidate Carry binary, and Codex **0.147.0** or Pi **0.84.2** already installed in an isolated location. Pi requires Node 22.19.0+. The launcher never installs packages, edits your ordinary client home, or creates a service.

**This local launcher is not the benchmark sandbox.** Use a trusted disposable workspace and non-sensitive host only. Pi tools run with your user permissions. Separate homes and removing provider keys from the client environment do not prevent same-user tools from reading host files or another process. The externally isolated benchmark, not this convenience launcher, supplies the adversarial containment boundary.

Supply `CARRY_PROXY_UPSTREAM_KEY` and `CARRY_PROXY_CLASSIFIER_KEY` through your normal secret manager into the launcher's environment. Do not put values in CLI arguments, config files, prompts, or shell history. `OPENAI_API_KEY` fallback is the integrated proxy's contract, not a requirement of these examples. Audit and compact can incur classifier cost even when no primary rewrite occurs.

Example with an already-downloaded candidate binary and already-installed clients:

```sh
export CARRY_PROXY_UPSTREAM_KEY CARRY_PROXY_CLASSIFIER_KEY

python3 scripts/proxy_trial.py \
  --client codex --client-binary /path/to/codex \
  --carry-binary /path/to/carry \
  --workspace /path/to/disposable-git-workspace \
  --trial-dir /path/to/fresh-trials/codex-compact-01 \
  --listen 127.0.0.1:8787 \
  --upstream-url https://api.openai.com/v1/responses \
  --classifier-url https://api.openai.com/v1/responses \
  --mode compact --model gpt-6-luna --reasoning medium \
  --classifier-model gpt-6-luna --classifier-effort low \
  --payoff-requests 1 --min-payback-percent 25 \
  --prompt 'Inspect this disposable repository and summarize one useful improvement.'

python3 scripts/proxy_trial.py \
  --client pi --client-binary /path/to/pi \
  --carry-binary /path/to/carry \
  --workspace /path/to/disposable-git-workspace \
  --trial-dir /path/to/fresh-trials/pi-compact-01 \
  --listen 127.0.0.1:8787 \
  --upstream-url https://api.openai.com/v1/responses \
  --classifier-url https://api.openai.com/v1/responses \
  --mode compact --model gpt-6-luna --reasoning medium \
  --classifier-model gpt-6-luna --classifier-effort low \
  --payoff-requests 1 --min-payback-percent 25 \
  --prompt 'Inspect this disposable repository and summarize one useful improvement.'
```

Choose a new trial directory for every arm. It must not exist and must be outside the workspace. For a transport control, use `--mode off` with a different fresh directory; omission of `--mode` also defaults to off. Do not reuse a modified workspace between compared arms.

The launcher creates separate `HOME`, `CODEX_HOME` or `PI_CODING_AGENT_DIR`, a scoped gateway token, and a random session ID. Codex uses a custom Responses provider with `supports_websockets = false`; Pi uses `openai-responses`. Both send stable `x-carry-session`, `x-carry-tenant: local-trial`, and `x-carry-branch: main` headers for one conversation. Native compaction is not disabled or fed fictional usage. `proxy.log`, `client-events.jsonl`, `trial.json`, and private proxy state are retained under the trial directory. Protect these artifacts: they may contain your task contents.

Readiness checks `/health` and authenticated `/carry/metrics`, and rejects an effective-mode mismatch before launching the client. The launcher stops its exact proxy and client process groups, including tool descendants, after completion or timeout. `--timeout` defaults to 300 seconds. It returns the client status and does not interpret a final response as a benchmark grade.

### Binary artifact

Regular CI builds `target/x86_64-unknown-linux-musl/release/carry` once and uploads `carry-linux-amd64-musl-<exact-head-SHA>` for seven days. Obtain it from the successful regular CI run for your exact candidate, extract it into an isolated directory, and set its executable bit. Artifact upload alone is not a successful compatibility check. Do not compile on a host whose heavy-work guard refuses the build.

## Benchmark configuration contract

The protected `run-swebench.yml` workflow adds the following opt-in settings:

| Dispatch input | Worker/runner environment | Default |
| --- | --- | --- |
| `proxy_mode` | `CARRY_PROXY_MODE` | `disabled` |
| `proxy_classifier_model` | `CARRY_PROXY_CLASSIFIER_MODEL` | `gpt-6-luna` |
| `proxy_classifier_effort` | `CARRY_PROXY_CLASSIFIER_EFFORT` | `low` |
| `proxy_payoff_requests` | `CARRY_PROXY_PAYOFF_REQUESTS` | `1` |
| `proxy_min_payback_percent` | `CARRY_PROXY_MIN_PAYBACK_PERCENT` | `25` |

Modes are `disabled`, `off`, `audit`, `compact`. Active modes require exactly one selected `codex` or `pi` harness. Retained cross-task session modes are explicitly rejected: this integration creates one fresh trusted sidecar per ordinary task slot, not a persistent sidecar across tasks. Payoff requests are integers 1–100; minimum payback is an integer 0–100. Classifier efforts are `minimal`, `low`, `medium`, `high`.

The runner invokes the integrated frontdoor with:

```text
carry proxy --listen 0.0.0.0:8787
  --upstream-url https://api.openai.com/v1/responses
  --classifier-url http://openai-proxy:8080/v1/responses
  --state-dir /proxy-state --mode MODE
  --classifier-model MODEL --classifier-reasoning-effort EFFORT
  --payoff-requests N --min-payback-percent INTEGER
```

The local launcher's `--classifier-effort` maps to the actual Rust frontdoor flag `--classifier-reasoning-effort`. Upstream URLs are **full Responses endpoints**, not base URLs. Native client provider base URLs end in `/v1`. The current integrated binary persists canonical request/response/plan traces under its state directory without requiring a `--trace` flag.

### Trusted boundary

- The client/task container remains on the existing per-slot internal network with fixed gateway access, base-only Git, and no evaluator material. Existing prepared-image and official-grading paths are unchanged.
- The Carry sidecar and gateway are on the trusted side, outside the task workspace. The sidecar uses the immutable candidate Carry image, a fresh 0700 state directory, matching host UID/GID, read-only root, dropped capabilities and `no-new-privileges`.
- The task receives only `BENCHMARK_CLIENT_TOKEN` through its existing `OPENAI_API_KEY` adapter interface. Codex's existing tmpfs auth flow is preserved. Provider credentials remain in trusted process/container environments, never argv or client config.
- Gateway routing is fixed: primary requests go to `carry-context-proxy:8787`; the scoped shadow token can only POST `/v1/responses` to the fixed provider. A client cannot pick a provider, classifier endpoint, tenant, branch or session. Trusted per-slot headers replace caller-supplied identity.
- A random gateway session ID is stable within a conversation and fresh per slot/arm. Fresh native client homes prevent native cache/session identity reuse across arms; the classifier's cache key derives from the fresh trusted identity. Inspect actual submitted cache keys before making live cache-isolation claims.
- The evaluator receives no provider key. Exact container/network cleanup is bounded, verified, and fails closed when active-proxy absence cannot be proven.

All five public proxy settings are carried through bootstrap, validated worker exports, runner provenance and immutable attempt-merge identity. Readiness checks the reported effective mode, and the workflow result gate compares every slot with the dispatched mode. This does not authorize a paid dispatch or reuse of an old preparation catalog; candidate preparation and protected approval still belong to the operator.

## Evidence and accounting

Each active slot retains trusted gateway `BENCHMARK_CONTEXT_EVENT` records, `benchmark-events.jsonl`, `benchmark-summary.json`, and the integrated proxy's state/trace files in the sibling `attempt-NN-proxy-state` directory. Numeric summaries distinguish primary and shadow calls, ordinary/read/write/output tokens, observed function calls, latency, censored/unpriced requests, proxy rewrites (`compactions` in Rust metrics) and native compactions.

Native provider usage is used directly. Completed but invalid reviews still cost money. Missing, truncated, failed or unanswered usage is censored, not filled with synthetic tokens or zero cost. The total is unavailable if the ledger or required pricing is incomplete; `observed_cost_lower_bound_usd` is only the observed priced subtotal. Original native client usage remains in the record, with its old estimate labeled `client_estimated_cost_usd`; the treatment's primary and total estimates use trusted native accounting. A missing metric is unknown, not zero. Latency is completed-call latency, not total task wall time.

[The rate table](../benchmarks/proxy-standard-rates.json) records the reviewed exact model IDs, separate cache-write/read rates, and the strictly-greater-than-272000 whole-request surcharge. Unknown aliases, nonstandard tiers and unsupported input billing categories have no estimated price. These are modeled standard API costs, not an invoice or infrastructure bill.

## Verification boundary

Local behavioral tests exercise the actual workflow bootstrap/result shell, worker exports, Docker command construction/lifecycle, native-client configuration, fragmented HTTP telemetry and censorship-aware summaries. Fake binaries and a scripted provider are labeled fixtures; they are not evidence of real model output, real usage, SDK parity or retention efficacy.

Regular trusted CI installs the pinned native clients into a run-scoped prefix and runs `scripts/proxy_native_fixture.py` with the exact integrated binary, a loopback fake Responses provider, actual tool execution and full-history tool-result echo. The provider rejects missing or changing native `prompt_cache_key` values and the fixture rejects namespace reuse between fresh clients; namespace hashes are included in its result artifact. Its synthetic responses/usage are fixture-only artifacts. Currently this actual-client fixture tests **proxy off forwarding**; native-compaction/checkpoint replacement and a rewrite-triggering compact trajectory still require explicit combined Rust/native-client E2E evidence before paid comparison. A compact arm that never rewrites proves only forwarding/review overhead, not savings.
