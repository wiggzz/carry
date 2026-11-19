# Shared model catalog

`src/model_catalog.json` is the exact-ID source for Carry's modeled rates,
context limits, long-input price tiers and cache capabilities. Rust embeds it
in the executable; Python benchmark reporting and local Pi trial configuration
read the same file from their pinned source checkout. The Node benchmark gateway
uses its keys for privacy-safe telemetry, so a recognized model is not erased
before receipt pricing. Its packaged worker mounts the same artifact read-only.
Adding a model does not require separate planner, dashboard, benchmark, gateway
or native-trial lists.

## Resolution and consumers

- An explicit context override is used if valid and within the known limit.
- Fresh, valid, exact-route provider metadata can narrow the catalog context
  limit. A published maximum override or auto-compaction trigger is not a default.
- Otherwise a known model uses its catalog limit, regardless of primary/reviewer
  role or API/subscription/gateway transport. This is Carry's operational model
  policy, not a claim about an account's entitlement or upstream advertisement.
- Unknown models have no context default and unknown prices stay unknown.
  An active primary request needs valid metadata or an explicit context override;
  an unknown auxiliary reviewer limit instead disables review without blocking a
  fitting primary. Catalog exports use null for unknown capacity, not a role cap.

The request's resolved contract drives primary admission, reviewer fit and
compaction planning, per-input-tier economics, receipt pricing and the saved
plan used by the dashboard. The dashboard separately shows the catalog-derived
ordinary economic target. **Capacity is a feasibility bound, not a cheap-tier
truncation ceiling:** crossing an input price threshold changes candidate prices;
it does not by itself force a rewrite or reject a fitting request.

Old fallback or missing-contract checkpoints resolve current policy on their
next request. Already recorded paid amounts are not repriced. Valid frozen
contracts, explicit overrides and dispatch-frozen reviewer receipts retain their
existing semantics. The dashboard describes the last saved plan, so a historical
plan is not relabeled as though a new request has already happened.

## Cache retention and comparison policy

For supported models on the exact official Responses API route, qualified
observed writes support billing credit for the documented minimum lifetime
(30 minutes by default, or 12 hours for uniform long requests). A qualified
30-minute read can extend the credited window without restarting a longer
write clock. The [provider contract](https://developers.openai.com/api/docs/guides/prompt-caching#cache-lifetime)
does not guarantee physical reuse or eviction at that boundary. Unknown routes
and unsupported retention controls supply no dispatch-time cache credit.

The planner prices its first call with compatible receipt evidence. With no
arrival schedule, it selects using cold future calls and shows the fixed-history,
short-gap all-hit sensitivity for each eligible alternative. The traditional
trajectory is a separate hypothetical prompt and may still show conditional
model-level reuse, not a gateway retention claim.

Prospective comparison policy v3 uses documented paid write rates where
supported, zero separate surcharge for recognized Codex routes, and ordinary
input pricing where a gateway write tariff is unknown. Identical histories
still share the observed price. Prior sums retain their original policy and
mixed windows are labeled. Receipt prices are unchanged; unsupported native
write-lifetime partitions remain unpriced rather than being valued as short
writes. These are API-equivalent valuations, not subscription invoices.

## GPT-6.2 (checked 2026-11-17)

The exact OpenAI model pages for [Sol](https://developers.openai.com/api/docs/models/gpt-6.2-sol)
and [Astra](https://developers.openai.com/api/docs/models/gpt-6.2-astra) document
1,050,000 total context, 922,000 maximum input and 128,000 maximum output tokens.
These are model/API facts, not subscription defaults or account entitlement.
Standard USD/million rates (input / cached / 30-minute write / output) are
2 / 0.10 / 2.50 / 10 for Sol and 8 / 0.40 / 10 / 40 for Astra. Above 272,000
input tokens the **whole request** uses 2x input/cache and 1.5x output rates.
The ordinary target remains 272,000 with separate margin/output reservation.
The shared native-client output allowance is 128,000, not an input allowance.

Both models support the documented short prompt-cache policy (1,024-token
minimum prefix); 12-hour writes cost 2.5x input and are not priced as short writes.
Explicit cache transport remains restricted to the exact supported API route,
never enabled by subscription discovery. These additions do not enable provider
subagents, premium processing tiers, or new reasoning wire levels. Tool calling
requires Responses. OpenAI warns that prefix edits can drop reasoning items;
model registration does not certify unchanged reasoning across Carry rewrites.

Subscription discovery now asks for `client_version=99.0.0` instead of the frozen
`0.147.0` compatibility view. Authenticated read-only discovery on the two local
accounts returned Sol 500,000 and Astra 272,000 default context, each with an
872,000 maximum override. The maximum is **not** the default and does not grant
expansion. This also exposes older IDs omitted by the old view: on a newly
resolved namespace, 6.1 Sol can narrow from the 1.05M catalog policy to 500K and
6 Astra / 6 Sol / 6 Luna (including the default reviewer) to 272K. Valid frozen
contracts are not rewritten; existing explicit limits above a fresh route default
remain rejected by the existing resolver. Deployments must account for this
prospective change; static rates/default model selections remain unchanged.

Fresh discovery provenance records the actual requested URL, including its
client-version query, alongside the response before ingestion. The process cache
keeps that source with its projected facts; ingestion does not infer a version
from today's fetch policy. Loopback fixtures retain their actual loopback source
and synthetic route class. Existing frozen contracts keep their original source
labels, including historical compatibility views and unversioned fixture labels.

## GPT-6 documented basis

The four exact model pages below were checked on 2026-10-22. Prices are Standard
USD per million tokens, not subscription charges or a gateway tariff:

| Model / source | Input | Cached input | Cache write | Output |
| --- | ---: | ---: | ---: | ---: |
| [gpt-6-luna](https://developers.openai.com/api/docs/models/gpt-6-luna) | $0.10 | $0.01 | $0.125 | $0.50 |
| [gpt-6-sol](https://developers.openai.com/api/docs/models/gpt-6-sol) | $2.00 | $0.20 | $2.50 | $10.00 |
| [gpt-6.1-sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol) | $2.00 | $0.10 | $2.50 | $10.00 |
| [gpt-6-astra](https://developers.openai.com/api/docs/models/gpt-6-astra) | $10.00 | $1.00 | $12.50 | $50.00 |

These pages document 1,050,000 total context tokens, maximum output 128,000,
and a whole-request price tier above 272,000 input tokens (2x input/cache rates,
1.5x output). Subtracting maximum output from total context does not establish a
separately documented maximum-input ceiling. Total capacity does not guarantee
every input/output split is accepted. Carry's existing margin/output reservation
checks and provider enforcement still apply. The conservative ordinary total
target is 272,000, with output reserve and estimation margin applied separately.

The three GPT-5.6 catalog context entries preserve the 272,000 operational
default observed in exact-ID Codex discovery on 2026-10-22, not its larger maximum
configuration override. Their existing modeled rates are unchanged.

`client_output_tokens` is an explicit reviewed native-client allowance, separate
from the provider's `max_output_tokens` fact. It preserves the previous 128,000
Pi setting for all existing known models without changing unknown GPT-5.6 provider
maxima into invented facts. The shared `pi_model` projection validates that policy
against any known output/total bounds; both local trials and packaged adapters use
it. Adding another model requires an explicit client policy, not a role fallback.

Raw Codex default/max/effective-window/compaction fields stay unknown unless
provided by exact-route metadata. The discovery checked during this change
listed only GPT-5.6 IDs; this catalog does not invent GPT-6 discovery entries or
aliases. Cache transport permission remains route-qualified independently of
model context policy. Provenance is exported by `/carry/catalog?model=<exact-id>`.
