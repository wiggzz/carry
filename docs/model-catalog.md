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

## GPT-6 documented basis

The four exact model pages below were checked on 2026-10-22. Prices are Standard
USD per million tokens, not subscription charges or a gateway tariff:

| Model / source | Input | Cached input | Cache write | Output |
| --- | ---: | ---: | ---: | ---: |
| [gpt-6-luna](https://developers.openai.com/api/docs/models/gpt-6-luna) | $0.10 | $0.01 | $0.125 | $0.50 |
| [gpt-6-sol](https://developers.openai.com/api/docs/models/gpt-6-sol) | $2.00 | $0.20 | $2.50 | $10.00 |
| [gpt-6.1-sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol) | $2.00 | $0.10 | $2.50 | $10.00 |
| [gpt-6-astra](https://developers.openai.com/api/docs/models/gpt-6-astra) | $10.00 | $1.00 | $12.50 | $50.00 |

These pages document 1,050,000 total context tokens, maximum input 922,000,
maximum output 128,000, and a whole-request price tier above 272,000 input
tokens (2x input/cache rates, 1.5x output). Total capacity does not guarantee every
input/output split is accepted. Carry's existing margin/output reservation
checks and provider enforcement still apply. The conservative ordinary total
target is 272,000, with output reserve and estimation margin applied separately.

The three GPT-5.6 catalog context entries preserve the 272,000 operational
default observed in exact-ID Codex discovery on 2026-10-22, not its larger maximum
configuration override. Their existing modeled rates are unchanged.

Raw Codex default/max/effective-window/compaction fields stay unknown unless
provided by exact-route metadata. The discovery checked during this change
listed only GPT-5.6 IDs; this catalog does not invent GPT-6 discovery entries or
aliases. Cache transport permission remains route-qualified independently of
model context policy. Provenance is exported by `/carry/catalog?model=<exact-id>`.
