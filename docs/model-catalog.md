# GPT-6 catalog sources

The exact IDs `gpt-6-luna`, `gpt-6-sol`, `gpt-6.1-sol`, and `gpt-6-astra`
have documented **OpenAI API** specifications. The model pages below were
checked on 2026-10-22. Their Markdown versions are available by appending `.md`.

| Exact model ID / source | Input | Cached input | Cache write | Output |
| --- | ---: | ---: | ---: | ---: |
| [gpt-6-luna](https://developers.openai.com/api/docs/models/gpt-6-luna) | $0.10 | $0.01 | $0.125 | $0.50 |
| [gpt-6-sol](https://developers.openai.com/api/docs/models/gpt-6-sol) | $2.00 | $0.20 | $2.50 | $10.00 |
| [gpt-6.1-sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol) | $2.00 | $0.10 | $2.50 | $10.00 |
| [gpt-6-astra](https://developers.openai.com/api/docs/models/gpt-6-astra) | $10.00 | $1.00 | $12.50 | $50.00 |

Prices are Standard USD per million tokens, not subscription charges or a
third-party gateway tariff. These rates were already present in Carry; the
catalog now identifies their public source. All four pages document:

- 1,050,000 total context tokens, maximum input 922,000, maximum output 128,000;
- a whole-request price tier above **272,000 input** tokens: 2x input/cache
  rates and 1.5x output rates.

Carry uses the documented total capacity only for the exact official
`https://api.openai.com/v1/responses` route and default processing tier. The
ordinary economic policy remains distinct: its conservative total budget is
still capped at 272,000, with output reserve and estimation margin applied by
`ordinary_policy`. Catalog capacity does not guarantee every input/output
split is accepted by the provider.

## Subscription discovery remains authoritative for its own route

Public API specifications do **not** establish a Codex subscription's
operational default, maximum configuration override, auto-compaction trigger,
effective-window percentage, or compaction hash. Those raw fields remain
unknown when the exact subscription ID is absent from fresh route-bound
metadata. Gateway and unknown-tier capacities also retain their role fallback.
There is no alias or prefix matching.

The read-only subscription catalog checked during this change advertised only
`gpt-5.6-sol`, `gpt-5.6-terra`, and `gpt-5.6-luna` as visible models. Consequently,
this change does **not** claim that the four GPT-6 IDs are newly advertised by
`/v1/models`, does not replace a missing subscription capacity with 1,050,000,
and does not repair that missing upstream metadata. The existing rates remain
API-equivalent modeling inputs on subscription/gateway routes, not invoices.

Saved checkpoint resolutions and prior accounting are not rewritten.
