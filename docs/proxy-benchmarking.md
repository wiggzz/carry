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
| `proxy_history_policy` | `CARRY_PROXY_HISTORY_POLICY` | `strict` |
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
- Gateway routing is fixed: primary requests go to `carry-context-proxy:8787`; the scoped shadow token can only POST `/v1/responses` to the fixed provider. A client cannot pick a provider, classifier endpoint, tenant, branch or session. Trusted per-slot headers replace caller-supplied identity **and history policy**. The runner maps validated `CARRY_PROXY_HISTORY_POLICY` to the gateway-only `BENCHMARK_CONTEXT_HISTORY_POLICY`; the gateway strips every caller `x-carry-*` header and supplies the fixed operator-selected `x-carry-history-policy`. The caller cannot request a reset or choose the policy.
- A random gateway session ID is stable within a conversation and fresh per slot/arm. Fresh native client homes prevent native cache/session identity reuse across arms; the classifier's cache key derives from the fresh trusted identity. Inspect actual submitted cache keys before making live cache-isolation claims.
- Container networking does not constrain provider-hosted retrieval. The benchmark's fresh/resumed Codex invocation explicitly sets `web_search="disabled"`, including custom command templates. Before creating an upstream request, the trusted gateway accepts only local `function`/`custom` tools and recursively compliant namespaces; hosted, unknown or malformed tool definitions are rejected, never stripped. Accepted native request/response bytes remain unchanged. Its request-body admission matches Carry's existing 16 MiB HTTP limit, separately from the 8 MiB telemetry capture budget. This benchmark policy does not change the generic opaque proxy's forwarding semantics or the convenience local launcher.
- The evaluator receives no provider key. Exact container/network cleanup is bounded, verified, and fails closed when active-proxy absence cannot be proven.

All six public proxy settings are carried through bootstrap, validated worker exports, runner provenance and immutable attempt-merge identity. Readiness checks the reported effective mode, and the workflow result gate compares every slot with the dispatched mode. `strict` rejects non-prefix native summary replacement with HTTP 409 in review modes. Explicit `reset-on-divergence` retires both active primary and shadow projections and accepts the replacement without inferring ancestry. Use the same policy in matched off/compact arms; off remains forwarding-only even under strict. Historical provenance missing only this setting normalizes to the historical `strict` default, never to reset. This does not authorize a paid dispatch or reuse of an old preparation catalog; candidate preparation and protected approval still belong to the operator.

## Evidence and accounting

Each active slot retains trusted gateway `BENCHMARK_CONTEXT_EVENT` records, `benchmark-events.jsonl`, `benchmark-summary.json`, and the integrated proxy's state/trace files in the sibling `attempt-NN-proxy-state` directory. Gateway logs allow only fixed reviewed model identifiers, an enumerated tier, known nonnegative numeric usage fields, bounded counters and fixed event metadata. Request/provider text, error messages, secret values, unknown object keys and arbitrary payloads never enter those events. Unknown model/tier becomes `unrecognized`, and unknown billing becomes a fixed marker; neither is silently default-priced. HTTP errors remain censored even if an error body contains a usage-shaped object. Numeric summaries distinguish primary and shadow calls, ordinary/read/write/output tokens, observed function calls, latency, censored/unpriced requests, proxy rewrites (`compactions` in Rust metrics) and native compactions.

Native provider usage is used directly. Completed but invalid reviews still cost money. Missing, truncated, failed or unanswered usage is censored, not filled with synthetic tokens or zero cost. The total is unavailable if the ledger or required pricing is incomplete; `observed_cost_lower_bound_usd` is only the observed priced subtotal. Original native client usage remains in the record, with its old estimate labeled `client_estimated_cost_usd`; the treatment's primary and total estimates use trusted native accounting. A missing metric is unknown, not zero. Latency is completed-call latency, not total task wall time.

[The rate table](../benchmarks/proxy-standard-rates.json) records the reviewed exact model IDs, separate cache-write/read rates, and the strictly-greater-than-272000 whole-request surcharge. Unknown aliases, nonstandard tiers and unsupported input billing categories have no estimated price. These are modeled standard API costs, not an invoice or infrastructure bill.

## Verification boundary

Local behavioral tests exercise the actual workflow bootstrap/result shell, worker exports, Docker command construction/lifecycle, native-client configuration, fragmented HTTP telemetry and censorship-aware summaries. Fake binaries and a scripted provider are labeled fixtures; they are not evidence of real model output, real usage, SDK parity or retention efficacy.

Regular trusted CI installs Codex 0.147.0 and Pi 0.84.2 into a run-scoped prefix, then executes the exact built integrated binary with a credential-free loopback scripted provider. `scripts/proxy_native_fixture.py` covers **nine actual-client cases**:

- Codex off and rewrite-triggering compact; Pi off and rewrite-triggering compact (four cases).
- Codex V1 and V2 native checkpoint/compaction continuation (two cases).
- Pi native manual summary: strict 409 rejection, explicit reset/continuation, and off forwarding/continuation (three cases).

The compact cases require an actually removed atomic two-call/two-result cohort, later mechanically pruned shadow requests, unchanged surviving tool outputs, a valid classifier request, and zero invalid reviews. Native cache keys must be present and stable within each conversation; native and classifier namespaces must differ between fresh clients. These direct-frontdoor fixtures alone do **not** certify the trusted production gateway's header policy.

A separate `scripts/proxy_gateway_native_fixture.py` CI step uses the same exact binary and pinned Pi through the **actual production `openai_proxy.js` handler**, not a replacement gateway. It creates three fresh native conversations: compact/strict rejects caller-spoofed reset with 409; off/strict continues; compact/operator-reset continues. The two continuing cases each drive **two native Pi RPC summary commands and two subsequent continuations**, verify at least two active-history rebases, an empty retired shadow projection, unchanged final checkpoint wire, tool effects, and operator-owned session identity. The only Node transport substitution is an explicit fixture-only `--require` hook mapping the fixed Carry alias and fixed HTTPS provider hostname to owned loopback ports; production has no arbitrary-upstream environment switch or TLS interception.

The optional installed-client test `scripts.test_codex_hosted_tools` runs four additional pinned-Codex fresh/resume/native/custom cases through the actual benchmark entrypoint and production gateway against the scripted loopback provider. Regular CI explicitly enables it using the already installed Codex executable. It checks recursively inspected outbound tool types, real local tool effects, native completion and synthetic-auth removal; handler regressions prove forbidden tools create zero upstream requests. These are protocol/isolation fixtures, not paid benchmark scores.

Both fixtures use fabricated provider responses and usage **only as labeled protocol fixtures**. They do not establish real model quality, paid usage, live cache hits, automatic Pi compaction, arbitrary-length histories, or cost savings. Two-summary RPC coverage proves repeated manual native summary/continuation, not Pi's automatic trigger. Standalone handler canary/HTTP-400 tests additionally verify content-free telemetry and byte-preserving forwarding, but are not a substitute for the complete pinned-Pi gateway path. A live comparison still requires exact-head successful CI plus independent evidence/quality/cost gates. A compact arm that never rewrites proves only forwarding/review overhead, not savings.
