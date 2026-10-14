# Opt-in Codex and Pi proxy trials

`carry proxy` sits between a native HTTP Responses client and its provider. `off` forwards without review; `audit` performs paid shadow review without selecting a rewritten primary context; `compact` enables the proxy planner. These modes are experiments, not a claim of savings or client compatibility established by unit tests.

The benchmark default is **`disabled`**, which selects the direct lanes without a Carry sidecar or reviewer. The shared benchmark gateway now validates and buffers POST bodies before forwarding, rejects hosted/malformed tools, and enforces a 16 MiB admission limit in both direct and active-proxy lanes; `disabled` does not preserve the former direct gateway's streaming/admission behavior. `disabled` and proxy `off` are distinct treatments. Compare `off` versus `compact` on the same candidate, tasks, model, effort, client configuration and native-compaction settings. Measure `disabled` versus `off` separately as transport overhead.

## Local ChatGPT/Codex login

The opt-in `--codex-login` adapter uses the same saved credential, proactive
refresh, account extraction and subscription headers as Carry's direct agent:

```sh
carry login                         # or: carry login --device-auth
carry proxy --codex-login --mode compact --classifier-model gpt-6.1-sol \
  --listen 127.0.0.1:8787 --state-dir /path/to/private-proxy-state
```

Credentials live in `CARRY_HOME` or `~/.carry`; `--codex-home` overrides that
directory. This reads **Carry's login**, not Pi's `auth.json` or a Codex CLI login.
No API key is needed when both primary and reviewer use the subscription. With
no `--classifier-url`, reviews use the same login/endpoint. Set an explicit
`--classifier-url` and `CARRY_PROXY_CLASSIFIER_KEY` to keep API-key-backed reviews.
Choose a reviewer model available to your subscription; model discovery is not
implemented for this adapter. `gpt-6.1-sol` has been exercised locally.

The default upstream switches to `https://chatgpt.com/backend-api/codex/responses`.
Subscription credentials are restricted to that exact endpoint (and explicit
HTTP loopback endpoints for credential-free fixtures). The proxy reloads and
refreshes the credential before requests, and refreshes/retries once on HTTP 401.

Unlike ordinary opaque API-key forwarding, this is an explicit wire adapter:
`store:false` and upstream SSE are required; initial system/developer messages
move to `instructions`; unsupported output limits, cache options/retention and
cache breakpoints are removed. Reviewer strict JSON-schema formatting survives
this adapter; it is not downgraded to instruction-only JSON.
Successful SSE bytes are relayed with the correct content type even when the
subscription server mislabels them. Nonstreaming callers receive the completed
Responses object extracted from the SSE. Public API explicit reviewer cache
writes are disabled for subscription reviews. Primary tools and surviving
ordinary input items are otherwise retained. Pi's summary requests work through
`/responses`; subscription `/responses/compact` and model discovery return 501.
This is not a broad Codex-client/native-compaction compatibility claim.

Reviews consume subscription capacity. Standard-rate usage estimates are still
modeled API equivalents, **not subscription charges or an invoice**. Neither
this adapter nor protocol fixtures establish savings. The protected API-key
benchmark lanes and default forwarding behavior are unchanged.

## Standalone proxy

Start Carry once, then connect any OpenAI Responses client independently:

```sh
carry login  # once, for subscription authentication
carry proxy --codex-login --mode compact --review-replayed-history \
  --state-dir "$HOME/.local/share/carry-proxy/state" --payoff-requests 1
```

Interactive startup opens the dashboard automatically. Use `--open-dashboard`
to open it from a noninteractive launch, or `--no-open-dashboard` to suppress
browser launch. The proxy prints the bound address and dashboard URL and saves
that URL as `dashboard-url` in the state directory. No client or model request
is started. The default reviewer is `gpt-6-luna`; the primary model is chosen by
the client. API-key users omit `--codex-login` and supply the upstream/reviewer
keys through the proxy environment.

Clients use `http://127.0.0.1:8787/v1`. A full-history client may omit
`x-carry-session`: Carry keeps one active prefix per tenant/branch and request
settings scope. Matching prefixes reuse removals and reviewer decisions; a
shorter or divergent history replaces that state, without searching old forks.
This also works across restarts with the same state directory. Headerless
clients sharing a scope share its active history; use explicit identities when
conversations must stay independent.

For explicit continuity, choose a unique `x-carry-session` per conversation and
reuse it when resuming. Optional `x-carry-tenant` and `x-carry-branch` headers
isolate owners and branches. Explicit sessions remain strict on divergence
unless the caller opts into a reset. `previous_response_id` requires an explicit
session and is validated there; Carry never discovers sessions from response
IDs. Restart Carry with the **same state directory** to recover checkpoints,
removals, reviewer state and usage. Do not put provider keys in client configuration.
For a shared listener, set `CARRY_PROXY_AUTH_TOKEN` securely and give clients
only that scoped gateway token. Loopback model endpoints also enforce it when
configured; token-free local dashboard access does not relax API authorization.

Pi needs only an ordinary Responses provider in `models.json` (merge, do not
replace other providers):

```json
{
  "providers": {
    "carry": {
      "baseUrl": "http://127.0.0.1:8787/v1",
      "api": "openai-responses",
      "apiKey": "carry-local",
      "headers": {"x-carry-session": "$CARRY_SESSION_ID"},
      "models": [{"id": "gpt-6.1-sol", "reasoning": true, "input": ["text", "image"], "contextWindow": 272000, "maxTokens": 128000}]
    }
  }
}
```

`carry-local` is a placeholder for a loopback proxy **without** a gateway token.
If gateway authentication is configured, use an environment reference to that
scoped token instead. Start Pi separately with, for example,
`CARRY_SESSION_ID=my-unique-conversation pi --model carry/gpt-6.1-sol`.
Use that same ID for a resume; choose a different one for a new conversation.
No Pi plugin, Pi runtime dependency, or client process management is required
by the proxy. Pi summary replacement needs an explicit new branch or the
`x-carry-history-policy: reset-on-divergence` opt-in described below.

### First replay compaction

On a new or rebased history, Carry automatically imports complete turns before
the latest user message when serialized input is at least 16 KiB. This applies
to both implicit prefixes and explicit sessions. Small histories skip this
bootstrap, and a fresh system/user request never qualifies as replay. Increase
`--bootstrap-min-input-bytes` to avoid automatic import for histories below a
chosen size. The threshold is a simple size heuristic, not a claim of savings.

`--review-replayed-history` or the per-request header
`x-carry-replay-history: before-latest-user` explicitly declares prior turns
consumed and overrides that threshold. Imported turns become reviewable before
the first primary request, independently of normal review cadence. The latest
user turn and its fresh tool results are not made removable by import.
Cross-boundary tool cohorts, opaque items and other pinned groups stay protected.
Histories with no eligible sources incur no bootstrap reviewer calls.

Each reviewer requests provider-enforced structured output with
`text.format.type=json_schema` and `strict:true`, on both API-key and Codex
routes. The schema enforces the three advice fields and memory shapes. Carry
still validates eligible IDs, decisions, provenance and size limits per item.
Valid items are applied even when siblings are invalid; protection takes precedence
over a conflicting drop. Invalid memories never activate. The next reviewer ledger
includes bounded `previous_review_feedback` with rejected field/index/reason and
accepted counts, so the model can correct rejected instructions without repeating
accepted facts. Feedback is checkpointed and never retains rejected source text.
Refusals, incomplete output, malformed top-level JSON or a provider rejecting the
schema reject the review; there is no silent unconstrained-output fallback.
Only the final assistant message is parsed as JSON advice; progress commentary
is not concatenated into that answer, and a final refusal does not fall back to
an earlier message.
The primary request can continue unchanged.
The newest live user group is pinned independently of reviewer advice.

Reviewer sweeps advance chronologically through complete atomic groups. Window
ends are chosen at retention-signal changes, human/tool boundaries, compatible
previous cache boundaries, or the request capacity frontier. Among those choices,
the planner minimizes modeled reviewer cost per estimated token of actionable
source coverage over the configured payoff/cadence horizon, including shared
instructions, mutable suffix and bounded output. This is a local heuristic, not
a globally optimal partition or a prediction of future provider cache hits.
Each rendered reviewer request, including instructions, schema, ledger and
carried memories, is bounded by `--classifier-max-input-tokens` (default 120,000).
A simple local `o200k_base` tokenizer over rendered JSON replaces bytes/4 for
input estimates and planner economics. These estimates include JSON wrappers;
they are not provider token counts, and native whole-request usage cannot assign
exact costs to individual quoted context items. Native usage remains the source
for the observed cost ledger, with cache fractions kept on one local estimate
basis in the planner. Set the input budget conservatively for the model, leaving
room for output and provider overhead; it is separate from the output cap.
`--classifier-max-input-bytes` (default 8 MiB) remains a transport guard.
Both small and oversized views use this economic selection; the token and byte
limits are capacity guards rather than the reason to split. There is no fixed
group-count cutoff. Compatible receipts survive window rotation, including
observed implicit reads on subscription routes. A cold read or an incompatible
prefix receives no current cache credit. Future stable-prefix reuse remains an
explicit sensitivity assumption. Oversized views are reviewed in bounded windows
instead of being rejected. A single oversized atomic group stays whole and protected;
it is skipped without a paid call or an invalid-review increment. Pinned-only
windows likewise make no paid call. Earlier source windows retire from the
active reviewer projection; canonical history, retention decisions and sourced
memories remain checkpointed. Source observations stay immutable within an
active window and are rebuilt from canonical history after retirement. Legacy
split cohorts are repaired in native source order with corrected annotation IDs.
Subscription cache comparisons use the same adapted wire shape as submission,
including folded instructions and inserted memory slots. A missing client cache
key gets a stable session key; caller-provided keys remain intact.
Primary cache receipts use bounded exact-prefix fingerprints; older prompt-copy
checkpoints migrate on load. Memory retirement updates the scenario restoration
delta separately from source-compaction counts.

An ongoing sweep freezes the exposed-history frontier when it starts. Newer
context waits for a later sweep, so a growing conversation cannot prevent the
current sweep from finishing. Completed members and the frontier are saved;
unfinished windows resume even before the normal review cadence is due.
Successful windows survive failed primaries and restarts. A wholly rejected review
stops the attempt without advancing that window or discarding completed windows;
partially accepted reviews advance while reporting rejected items for correction.
Completed window memberships are checkpointed as source IDs/member snapshots,
without duplicate payloads. Rewrite economics price the complete frozen sweep,
including each scheduled call and output cap across the payoff horizon. Cadence,
per-request call limits and unfinished sweep progress determine those calls.
New eligible tail context is priced as conservative bounded chronological windows;
partial-window continuation prices only remaining sources, with no credit from a
removed prefix. Missing oversized coverage reports `reviewer_partition_unknown`
instead of pricing only the last active window. These fixed-partition projections
are conditional assumptions and do not predict provider latency or future changes.
Reports separate `future_review_events` from `future_reviewer_calls`; reviewer
input/cache totals are named for the whole sweep, rather than one request.
A rebase/native epoch reset clears this progress. The dashboard distinguishes wholly invalid reviews from partially accepted reviews; legacy invalid-review totals remain historical.

Reviewer requests replay native messages, reasoning (including encrypted content),
and tool call/result items. Source IDs are appended to the last message text block;
protocol/ciphertext items stay exact and receive an adjacent small metadata note.
Primary system/developer source roles become reviewer user evidence, with original
roles recorded in source metadata. Unknown native types use an opaque fallback.
The final mutable ledger carries the latest native user task even when reviewing
an older window, without rewriting the source prefix. It also exposes
`eligible_memory_ids` and `current_memories`. Existing
`m…` memory IDs can be protected or made removable independently of `g…` source
groups. Protection wins conflicting opinions. Retirement removes the fact from
both primary/reviewer views and its source anchor; IDs remain monotonic across
retirement and restart. New memories still cite eligible `g…` sources, never
memory handles. Invalid items are rejected individually.
Primary forwarding remains unchanged. Strict structured output is requested on
both API-key and subscription reviewer paths. Live cross-model encrypted reasoning
compatibility still requires provider verification; local fixtures verify transport.

Reviewer source observations precede carried memories and the final mutable ledger,
so newly accepted facts do not rewrite a source window's cache prefix. Per-turn
client telemetry does not create a new implicit lineage; session/thread identifiers
still isolate state, and primary client metadata is forwarded unchanged.

`--review-max-calls` defaults to 16 per primary request, for bootstrap and ongoing
windows alike; `--bootstrap-max-calls` remains a compatible alias. Subsequent
requests resume unfinished windows. `--bootstrap-max-cost-usd` is still
bootstrap-only and defaults to $1 of cumulative
estimated spend per active history. Spend is reserved durably before dispatch,
including failed attempts, using conservative input/cache-write pricing and the
configured reviewer output cap. Zero disables paid bootstrap calls; unknown
reviewer pricing also prevents dispatch. Restarting does not replenish this
allowance. These are modeled estimates, **not a hard provider billing limit**;
subscription estimates are API equivalents, and the subscription adapter does
not enforce the API output cap. Limits leave unreviewed history intact and never
block the primary request. By default reviews are synchronous; lower
`--review-max-calls` to bound paid round trips per primary while backlog windows
resume later. This call limit does not replenish or extend bootstrap spend.

Use `--background-reviews` to remove ordinary reviewer round-trip latency from
primary requests. Bootstrap remains synchronous to review imported history before
its first primary use. Ordinary reviews use an immutable snapshot, with at most
one review in flight per session; a bootstrap requested during old in-flight work
is deferred until a later request. The primary session lock is not held during
reviewer transport. Completion reacquires it and merges reviewer state and usage
into the newest checkpoint, preserving newer primary history, counters and cache.
Only already-merged advice can affect the next request, never a submitted request.
Membership checks cover sources actually replayed to that review. An unrelated
newer pending tool step may complete without invalidating advice about an older
window; changed reviewed cohorts or canonical lineage still discard that advice.
Changed source contents, atomic membership, removals or rebases discard stale
advice, but its paid usage is still counted. Bounded oversized windows run in the
same background job and remain governed by `--review-max-calls`.

In-flight background work is not resumed across process shutdown/restart. A durable
pending marker makes the next access censor an interrupted attempt's unknown usage
rather than treating it as free. Completed background work checkpoints normally;
unfinished sweep progress resumes on later requests. Dashboard savings estimates
are unavailable while a background review is pending.

`off` is unchanged; `audit` reviews without rewriting; `compact` rewrites only
after valid advice and the ordinary economic admission check. Import does not
fabricate completed requests or native usage. Failed primaries do not grant
exposure to fresh input or commit speculative removals. Previously committed
removals remain applied to matching echoed histories after restart.

## Everyday Pi launcher

`scripts/pi_carry.py` is the shareable version of the local `pi-carry` wrapper.
It uses your existing Pi configuration, starts one loopback proxy per invocation,
passes Pi only a scoped gateway token, and stops its own proxy on exit. It does
not install Pi, edit its settings, or replace a proxy that is already running.
This is a convenience launcher, **not a sandbox**: tools run with your user’s
permissions and can still access same-user credentials and processes.

```sh
cargo build --release
mkdir -p ~/.local/share/carry-proxy ~/.local/bin
install -m755 target/release/carry ~/.local/share/carry-proxy/carry
install -m755 scripts/pi_carry.py ~/.local/bin/pi-carry
carry login
```

Merge the following provider into your Pi agent-directory `models.json`
(default `~/.pi/agent/models.json`); do not overwrite other providers. This is
an OpenAI Responses endpoint, not an OpenAI Codex OAuth provider. Carry owns
the subscription authentication. Choose a model available to your account.

```json
{
  "providers": {
    "carry": {
      "baseUrl": "http://127.0.0.1:8787/v1",
      "api": "openai-responses",
      "apiKey": "$CARRY_PI_TOKEN",
      "headers": {
        "x-carry-session": "$CARRY_PI_SESSION",
        "x-carry-tenant": "local-pi",
        "x-carry-branch": "main",
        "x-carry-history-policy": "reset-on-divergence"
      },
      "models": [
        {
          "id": "gpt-6.1-sol",
          "reasoning": true,
          "input": ["text", "image"],
          "contextWindow": 272000,
          "maxTokens": 128000
        }
      ]
    }
  }
}
```

`pi-carry --check` checks authenticated proxy readiness and Pi’s model listing;
it makes no model requests. `pi-carry` then starts an ordinary interactive Pi
session. It prints the stats dashboard link and saves it with mode 0600 to
`~/.local/share/carry-proxy/dashboard-url`. Local dashboard access needs no token.
Private logs/state remain under `~/.local/share/carry-proxy/runs/<session>` and
may contain conversation contents.

Optional environment settings:

- `CARRY_PI_MODE`: `compact` (default), `audit`, or `off`.
- `CARRY_PI_AUTH`: `codex` (default, Carry’s saved login) or `api-key`.
- `CARRY_PI_MODEL`: primary model ID (default `gpt-6.1-sol`).
- `CARRY_PI_CLASSIFIER_MODEL`: reviewer model ID (default `gpt-6-luna`, independent of the primary model).
- `CARRY_PI_BINARY`: Carry executable (default `~/.local/share/carry-proxy/carry`).
- `CARRY_PI_CLIENT`: Pi executable (default `pi` on `PATH`).

For API-key mode, supply `CARRY_PROXY_UPSTREAM_KEY` and
`CARRY_PROXY_CLASSIFIER_KEY` securely in the launcher environment (or the
proxy’s `OPENAI_API_KEY` fallback). These variables are removed from Pi’s child
environment, not hidden from same-user tools. Compact/audit reviews consume
API or subscription capacity even if no rewrite occurs. An alternate primary
model also needs a matching entry in Pi’s `models.json`.

The launcher creates a fresh proxy session on each invocation; Pi summary
replacements explicitly rebase that session. It does not recover the previous
proxy state when resuming Pi. It opts into first replay review so a resumed
conversation can be compacted before the first primary call even with no proxy
checkpoint. For durable state independent of client restarts, use the standalone
workflow above. Port 8787 must be free; stop/restart your own launcher rather than
replacing a running binary’s process.

## Reviewer cache policy

The classifier cache policy is **reviewer-only**. It does not add cache fields,
Carry prompts, or tools to primary requests. `--classifier-cache-policy auto`
(the local-launcher default) enables stable explicit write boundaries only for
supported exact classifier models at the official OpenAI Responses endpoint.
`disabled` preserves the unmarked reviewer representation. `openai-explicit`
is an explicit assertion that a custom classifier URL is a trusted OpenAI
Responses gateway; it is not a generic-provider compatibility claim.

The supported exact model IDs are `gpt-6-luna`, `gpt-6-sol`, and `gpt-6.1-sol`.
Unknown models do not automatically receive this provider-specific schema;
explicit mode rejects unsupported models before launch. The protected benchmark
lane uses a fixed OpenAI upstream behind its local gateway, so its recorded
`CARRY_PROXY_CLASSIFIER_CACHE_POLICY` / workflow
`proxy_classifier_cache_policy` defaults to `openai-explicit`.

Write endpoints belong to stable reviewer observations, **never the changing
final ledger**. Up to four compatible write boundaries are retained/rotated.
Main retention still controls shadow pruning; the classifier has no independent
retention policy. Native usage receipts—not marker presence or simulated fixture
usage—are the authority for actual cache reads and billed costs. A cache repair
requires a new source-frozen matched cohort before changing any savings claim.
Absent cache-policy fields in historical benchmark provenance mean `disabled`,
not the new benchmark default. Explicit recorded values remain unchanged.

Codex subscription transport does **not** accept the API's explicit cache schema;
Carry strips those fields and uses the stable reviewer cache key with
provider-managed implicit caching. A disabled explicit policy is not proof that
implicit reads are disabled. The dashboard reports `codex-implicit`,
`provider-implicit`, or `openai-explicit` separately from the policy. Its cached
read counters come from native receipts, not boundary estimates. Cold starts,
provider warm-up/routing and removed prefixes can miss even when stable source
remains; do not treat a byte-identical prefix as a guaranteed hit.

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

Regular CI builds `target/x86_64-unknown-linux-musl/release/carry` once and uploads `carry-linux-amd64-musl-<github.sha>` for seven days. The suffix identifies the actual built commit; on PR events it can be a synthetic merge SHA, not the PR head. Verify the run's built commit and tracked tree against your candidate, preserving both identities even when their trees are equal. Obtain the artifact from that successful regular CI run, extract it into an isolated directory, and set its executable bit. Artifact upload alone is not a successful compatibility check. Do not compile on a host whose heavy-work guard refuses the build.

## Benchmark configuration contract

The protected `run-swebench.yml` workflow adds the following opt-in settings:

| Dispatch input | Worker/runner environment | Default |
| --- | --- | --- |
| `proxy_mode` | `CARRY_PROXY_MODE` | `disabled` |
| `proxy_history_policy` | `CARRY_PROXY_HISTORY_POLICY` | `strict` |
| `proxy_classifier_model` | `CARRY_PROXY_CLASSIFIER_MODEL` | `gpt-6-luna` |
| `proxy_classifier_effort` | `CARRY_PROXY_CLASSIFIER_EFFORT` | `low` |
| `proxy_classifier_cache_policy` | `CARRY_PROXY_CLASSIFIER_CACHE_POLICY` | `openai-explicit` |
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
  --classifier-cache-policy POLICY
  --payoff-requests N --min-payback-percent INTEGER
```

The local launcher's `--classifier-effort` maps to the actual Rust frontdoor flag `--classifier-reasoning-effort`. Upstream URLs are **full Responses endpoints**, not base URLs. Native client provider base URLs end in `/v1`. The current integrated binary persists canonical request/response/plan traces under its state directory without requiring a `--trace` flag.

### Trusted boundary

- The client/task container remains on the existing per-slot internal network with fixed gateway access, base-only Git, and no evaluator material. Existing prepared-image and official-grading paths are unchanged.
- The Carry sidecar and gateway are on the trusted side, outside the task workspace. The sidecar uses the immutable candidate Carry image, a fresh 0700 state directory, matching host UID/GID, read-only root, dropped capabilities and `no-new-privileges`.
- The task receives only `BENCHMARK_CLIENT_TOKEN` through its existing `OPENAI_API_KEY` adapter interface. Codex's existing tmpfs auth flow is preserved. Provider credentials remain in trusted process/container environments, never argv or client config.
- Gateway routing is fixed: primary requests go to `carry-context-proxy:8787`; the scoped shadow token can only POST `/v1/responses` to the fixed provider. A client cannot pick a provider, classifier endpoint, tenant, branch or session. Trusted per-slot headers replace caller-supplied identity **and history policy**. The runner maps validated `CARRY_PROXY_HISTORY_POLICY` to the gateway-only `BENCHMARK_CONTEXT_HISTORY_POLICY`; the gateway strips every caller `x-carry-*` header and supplies the fixed operator-selected `x-carry-history-policy`. The caller cannot request a reset or choose the policy.
- A random gateway session ID is stable within a conversation and fresh per slot/arm. Fresh native client homes prevent native cache/session identity reuse across arms; the classifier's cache key derives from the fresh trusted identity. Inspect actual submitted cache keys before making live cache-isolation claims.
- Container networking does not constrain provider-hosted retrieval. The benchmark's fresh/resumed Codex invocation explicitly sets `web_search="disabled"`, including custom command templates. Before creating an upstream request, the shared Node benchmark gateway accepts only local `function`/`custom` tools and recursively compliant namespaces; hosted, unknown or malformed tool definitions are rejected, never stripped. This applies to direct and active-proxy lanes. Accepted native request/response bytes remain unchanged, but POST bodies are buffered before upstream admission. Its request-body admission matches Carry's existing 16 MiB HTTP limit, separately from the 8 MiB telemetry capture budget. This benchmark policy does not change the generic opaque Carry proxy's forwarding semantics or the convenience local launcher.
- The evaluator receives no provider key. Exact container/network cleanup is bounded, verified, and fails closed in both direct and active-proxy lanes. Absence requires exit 1 and a known diagnostic naming the exact resource kind and name; daemon/socket failures, other names and unexpected exits do not prove cleanup. Docker's exact `network NAME not found` diagnostic is supported.

All seven public proxy settings are carried through bootstrap, validated worker exports, runner provenance and immutable attempt-merge identity. Readiness checks the reported effective mode, and the workflow result gate compares every slot with the dispatched mode. `strict` rejects non-prefix native summary replacement with HTTP 409 in review modes. Explicit `reset-on-divergence` retires both active primary and shadow projections and accepts the replacement without inferring ancestry. Use the same policy in matched off/compact arms; off remains forwarding-only even under strict. Historical provenance missing only this setting normalizes to the historical `strict` default, never to reset. This does not authorize a paid dispatch or reuse of an old preparation catalog; candidate preparation and protected approval still belong to the operator.

## Evidence and accounting

Each active slot retains trusted gateway `BENCHMARK_CONTEXT_EVENT` records, `benchmark-events.jsonl`, `benchmark-summary.json`, and the integrated proxy's state/trace files in the sibling `attempt-NN-proxy-state` directory. Gateway logs allow only fixed reviewed model identifiers, an enumerated tier, known nonnegative numeric usage fields, bounded counters and fixed event metadata. Request/provider text, error messages, secret values, unknown object keys and arbitrary payloads never enter those events. Unknown model/tier becomes `unrecognized`, and unknown billing becomes a fixed marker; neither is silently default-priced. HTTP errors remain censored even if an error body contains a usage-shaped object. Numeric summaries distinguish primary and shadow calls, ordinary/read/write/output tokens, observed function calls, latency, censored/unpriced requests, proxy rewrites (`compactions` in Rust metrics) and native compactions.

Native provider usage is used directly. Completed but invalid reviews still cost money. Missing, truncated, failed or unanswered usage is censored, not filled with synthetic tokens or zero cost. Ledger integrity requires one ordered start and terminal per request, with an unambiguous actor; orphan, duplicate, conflicting or malformed records cannot certify a complete ledger. Orphan completions remain counted, and independently known unique receipt costs—including missing-start receipts—remain in `observed_cost_lower_bound_usd`. Duplicate receipts are priced once; conflicting model/tier/usage receipts are not arbitrarily selected. A valid censored terminal can close a lifecycle without providing a price. The total is unavailable if the ledger or required pricing is incomplete; the lower bound is only the observed priced subtotal. Original native client usage remains in the record, with its old estimate labeled `client_estimated_cost_usd`; the treatment's primary and total estimates use trusted native accounting. A missing metric is unknown, not zero. Latency is completed-call latency, not total task wall time. Native-compaction request classification covers the legacy compact path and V2 `input` trigger items; it is separate from the collected Rust `native_compactions` metric.

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

Both fixtures use fabricated provider responses and usage **only as labeled protocol fixtures**. They do not establish real model quality, paid usage, live cache hits, automatic Pi compaction, arbitrary-length histories, or cost savings. Two-summary RPC coverage proves repeated manual native summary/continuation, not Pi's automatic trigger. Standalone handler canary/HTTP-400 tests additionally verify content-free telemetry and byte-preserving forwarding, but are not a substitute for the complete pinned-Pi gateway path. A live comparison still requires successful candidate-source CI with verified built-commit/tree identity plus independent evidence/quality/cost gates. A compact arm that never rewrites proves only forwarding/review overhead, not savings.

### Local stats dashboard

Open `/carry/dashboard` on the proxy (normally
`http://127.0.0.1:8787/carry/dashboard`). Loopback connections need no token.
`pi-carry` prints this plain URL. For remote access, enter the **proxy** token
in the page, never your OpenAI credential. A URL fragment `#token=...` is still
supported; the page removes it immediately and holds the token only in memory.

The shell is public; `/carry/dashboard/stats` waives gateway authorization only
when the TCP peer is loopback (IPv4 or IPv6). Model, metrics, and other API
endpoints still require the gateway token. Host and forwarded headers cannot
claim local access. A local reverse proxy counts as a local peer, so it must
enforce its own authentication before exposing these routes remotely.
On a shared/non-loopback listener, configure a gateway token: dashboard stats
are gateway-wide, not restricted to a single session.
The endpoint only returns counters, hashes, ledger totals, and context sizes,
not conversation text, reviewer memory, or credentials. All responses are
`Cache-Control: no-store`.

The dashboard refreshes every two seconds while visible. It shows primary and
reviewer usage separately, native cached-read share, cache writes, rewrites,
failures, explicit history rebases, and retained/removed item counts. Retained
input bytes are serialized bytes, **not** tokens. State is checkpoint-based,
not a live stream of tokens. Unreadable checkpoints are flagged and totals are
marked incomplete. Cost values are standard API equivalents, not subscription
charges, invoices, or measured savings; unpriced calls make the total unavailable
while the known priced subtotal remains visible.

The separate **cost comparison scenario** follows the main savings estimator's
frozen-trajectory approach: compare Carry's retained input against restoring
its removed context until a conventional threshold-triggered summary. Both
branches use the same local token estimates for inputs and the same recorded
output tokens. Carry includes reviewer calls (priced uncached); the
traditional branch pays for hypothetical summary calls. A negative saving is
shown as a loss, not clamped to zero, and percentages use traditional scenario
cost as the denominator—not actual provider spend.

Assumptions are displayed: 272,000-token window, 16,384 reserve, 20,000 recent
tokens retained and a 1,000-token summary, checked after completed non-tool
responses. Cache reuse assumes matching route/key/settings, append-compatible
prefixes and a 30-minute expiry; the restored branch's literal prefix after a
Carry edit is unknown, so its frozen-size reuse is conditional. These are not
reconstructed client/provider compaction or cache guarantees. Summaries could
change subsequent actions, quality or rereads; the scenario does not model that.
Both scenario totals remain separate from the observed native-usage ledger.

Scenario state survives restart. Failed calls, missing usage, unknown pricing,
explicit native compaction, native image/audio/video requests, or older untracked
history mark the comparison
unavailable, with coverage counters in stats JSON. This change does not backfill
older checkpoints from raw traces. Dashboard comparison cards sum only sessions
with complete estimates and display the included/total session count. Excluded
sessions remain in usage totals and the session list, labeled `Excluded` with
dashes for scenario values. With no eligible sessions, comparison cards show
`No eligible sessions` rather than zero savings. Dashboard responses include only numerical
scenario estimates, never the prior prompt kept privately for cache matching.
See [the proxy backlog](proxy-backlog.md) for completed background review and
consolidation with the shared main savings/report estimator.

Review modes (`compact`/`audit`) accept full-history input or a delta referencing
the latest completed `previous_response_id` in the explicit session/branch. Unknown
or stale IDs return 409; an explicit rebase retires the continuation. The proxy
reconstructs history and sends full retained input upstream, without
`previous_response_id`. Conversation handles and background requests are rejected.
Reviewer memories are small atomic facts: they activate when any source is removed
and are inserted after the latest source group’s last member, retained or removed. The
proxy renders the retained primary history, including any previously applied
rewrites, into the outbound `input`. `off` forwards the caller's wire request
unchanged, including a caller-supplied `previous_response_id`; the proxy does
not automatically build server-side response chains in any mode.
